//! Dialing the target and calling the Health service.

use tonic::transport::{Channel, Endpoint};
use tonic_health::pb::HealthCheckRequest;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;

use crate::endpoint::Target;
use crate::tls::{self, TlsError, TlsMode};

/// Everything the transport needs to probe a target, assembled once from the
/// CLI. Keeps `probe` independent of the argument parser and gives the later
/// stages (timeouts, retry, metadata) a single place to grow.
pub(crate) struct ProbeParams {
    pub(crate) target: Target,
    pub(crate) tls_mode: TlsMode,
    pub(crate) service: Option<String>,
}

impl ProbeParams {
    /// The service name sent in the Health request; empty means overall health.
    fn service_name(&self) -> String {
        self.service.clone().unwrap_or_default()
    }
}

/// Decodes a wire status code, treating an out-of-range value as not serving.
pub(crate) fn decode_status(raw: i32) -> ServingStatus {
    ServingStatus::try_from(raw).unwrap_or(ServingStatus::Unknown)
}

/// Maps a serving status to a process exit code, following grpc-health-probe:
/// SERVING is healthy, anything else is a failure.
pub(crate) fn status_exit_code(status: ServingStatus) -> u8 {
    match status {
        ServingStatus::Serving => 0,
        _ => 3,
    }
}

/// Exit code for a finished watch, based on the last status seen. With no
/// status observed at all (the stream closed or was interrupted before any
/// update), the server was never confirmed healthy, so this is a failure.
pub(crate) fn watch_exit_code(last: Option<ServingStatus>) -> u8 {
    match last {
        Some(status) => status_exit_code(status),
        None => 3,
    }
}

/// A failed probe, wrapping the underlying transport or rpc error.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProbeError {
    #[error("connection failed: {0}")]
    Transport(#[source] tonic::transport::Error),
    #[error("rpc error: {0}")]
    Rpc(#[source] tonic::Status),
    #[error(transparent)]
    Tls(#[from] TlsError),
}

impl ProbeError {
    // TODO: carve timeout into exit code 4 once --timeout and --connect-timeout
    // land - both Transport (connect timeout) and Rpc (DEADLINE_EXCEEDED).
    /// Exit code for a failed probe.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            ProbeError::Transport(_) => 1,
            ProbeError::Rpc(_) => 2,
            ProbeError::Tls(_) => 1,
        }
    }
}

/// Dials the target, applying TLS, and returns a Health client. Shared by the
/// one-shot `Check` and the streaming `Watch` paths.
async fn connect(params: &ProbeParams) -> Result<HealthClient<Channel>, ProbeError> {
    let endpoint = Endpoint::from_shared(params.target.uri(params.tls_mode.is_enabled()))
        .map_err(ProbeError::Transport)?;
    let endpoint = tls::configure(endpoint, &params.tls_mode, params.target.host())?;
    let channel = endpoint.connect().await.map_err(ProbeError::Transport)?;
    Ok(HealthClient::new(channel))
}

/// Connects to the target and calls `grpc.health.v1.Health/Check`.
pub(crate) async fn run(params: &ProbeParams) -> Result<ServingStatus, ProbeError> {
    let mut client = connect(params).await?;

    let request = HealthCheckRequest {
        service: params.service_name(),
    };
    let status = client
        .check(request)
        .await
        .map_err(ProbeError::Rpc)?
        .into_inner()
        .status;

    Ok(decode_status(status))
}

/// Opens the `grpc.health.v1.Health/Watch` stream for the target's service.
pub(crate) async fn open_watch(
    params: &ProbeParams,
) -> Result<tonic::Streaming<tonic_health::pb::HealthCheckResponse>, ProbeError> {
    let mut client = connect(params).await?;

    let request = HealthCheckRequest {
        service: params.service_name(),
    };
    let stream = client
        .watch(request)
        .await
        .map_err(ProbeError::Rpc)?
        .into_inner();
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    #[test]
    fn serving_status_exits_zero() {
        assert_eq!(status_exit_code(ServingStatus::Serving), 0);
    }

    #[test]
    fn non_serving_status_exits_three() {
        assert_eq!(status_exit_code(ServingStatus::NotServing), 3);
        assert_eq!(status_exit_code(ServingStatus::Unknown), 3);
        assert_eq!(status_exit_code(ServingStatus::ServiceUnknown), 3);
    }

    #[test]
    fn transport_error_exits_one() {
        // Any transport error works; we only test the variant -> exit code mapping.
        let err = Endpoint::from_shared("http://[".to_string()).unwrap_err();
        assert_eq!(ProbeError::Transport(err).exit_code(), 1);
    }

    #[test]
    fn rpc_error_exits_two() {
        let err = tonic::Status::internal("rpc failed");
        assert_eq!(ProbeError::Rpc(err).exit_code(), 2);
    }

    #[test]
    fn watch_exit_uses_last_observed_status() {
        assert_eq!(watch_exit_code(Some(ServingStatus::Serving)), 0);
        assert_eq!(watch_exit_code(Some(ServingStatus::NotServing)), 3);
    }

    #[test]
    fn watch_exit_without_any_status_is_unhealthy() {
        assert_eq!(watch_exit_code(None), 3);
    }

    #[test]
    fn tls_error_exits_one() {
        // A TLS setup failure (e.g. an unreadable CA file) is a connection-level
        // failure, like a refused dial.
        let err = TlsError::ReadCa {
            path: "/nope.pem".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert_eq!(ProbeError::Tls(err).exit_code(), 1);
    }

    // End-to-end: a real Health server reporting SERVING drives the whole
    // connect -> Check -> status path to a zero exit code.
    #[tokio::test]
    async fn check_against_serving_server() {
        use tokio::net::TcpListener;
        use tonic::transport::Server;
        use tonic::transport::server::TcpIncoming;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (reporter, health_service) = tonic_health::server::health_reporter();
        reporter
            .set_service_status("", tonic_health::ServingStatus::Serving)
            .await;

        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(health_service)
                .serve_with_incoming(TcpIncoming::from(listener))
                .await
                .unwrap();
        });

        let params = Cli::try_parse_from(["grpcknock", "--addr", &addr.to_string()])
            .unwrap()
            .probe_params();
        let status = run(&params).await.expect("check should succeed");
        assert_eq!(status_exit_code(status), 0);

        server.abort();
    }

    // End-to-end: Health/Watch streams the current status, then each change the
    // server reports.
    #[tokio::test]
    async fn watch_reflects_status_changes() {
        use tokio::net::TcpListener;
        use tonic::transport::Server;
        use tonic::transport::server::TcpIncoming;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (reporter, health_service) = tonic_health::server::health_reporter();
        reporter
            .set_service_status("demo", tonic_health::ServingStatus::Serving)
            .await;

        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(health_service)
                .serve_with_incoming(TcpIncoming::from(listener))
                .await
                .unwrap();
        });

        let params = Cli::try_parse_from([
            "grpcknock",
            "--addr",
            &addr.to_string(),
            "--service",
            "demo",
            "--watch",
        ])
        .unwrap()
        .probe_params();
        let mut stream = open_watch(&params).await.expect("watch should open");

        let first = stream.message().await.unwrap().unwrap();
        assert_eq!(first.status, ServingStatus::Serving as i32);

        reporter
            .set_service_status("demo", tonic_health::ServingStatus::NotServing)
            .await;
        let second = stream.message().await.unwrap().unwrap();
        assert_eq!(second.status, ServingStatus::NotServing as i32);

        server.abort();
    }
}
