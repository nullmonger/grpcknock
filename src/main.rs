use std::process::ExitCode;

use clap::{ArgGroup, Parser};
use tonic::transport::Endpoint;
use tonic_health::pb::HealthCheckRequest;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;

/// gRPC health check probe.
#[derive(Parser)]
#[command(version, about)]
#[command(group(ArgGroup::new("target").required(true).args(["addr", "port"])))]
struct Cli {
    /// Target address as host:port
    #[arg(long)]
    addr: Option<String>,

    /// Shortcut for localhost:<port>
    #[arg(long)]
    port: Option<u16>,

    // TODO: accept multiple services (Vec<String>) once multi-service checks
    // are in scope; the health protocol allows probing several in one run.
    /// Service name to check; omit to check overall server health
    #[arg(long)]
    service: Option<String>,
}

impl Cli {
    // TODO: return a structured endpoint (host, port) instead of a String.
    // TLS will need the SNI/authority from the host, and `--addr` needs
    // host:port validation (port present, IPv6 literals) once dialing exists.
    /// Resolves the endpoint to dial from `--addr` or `--port`.
    fn target(&self) -> String {
        match (&self.addr, self.port) {
            (Some(addr), _) => addr.clone(),
            (_, Some(port)) => format!("localhost:{port}"),
            (None, None) => unreachable!("clap requires one of --addr or --port"),
        }
    }
}

/// Maps a serving status to a process exit code, following grpc-health-probe:
/// SERVING is healthy, anything else is a failure.
fn status_exit_code(status: ServingStatus) -> u8 {
    match status {
        ServingStatus::Serving => 0,
        _ => 3,
    }
}

/// A failed probe, wrapping the underlying transport or rpc error.
#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("connection failed: {0}")]
    Transport(#[source] tonic::transport::Error),
    #[error("rpc error: {0}")]
    Rpc(#[source] tonic::Status),
}

impl ProbeError {
    // TODO: carve timeout out of Transport into exit code 4 once --timeout
    // and --connect-timeout land.
    /// Exit code for a failed probe.
    fn exit_code(&self) -> u8 {
        match self {
            ProbeError::Transport(_) => 1,
            ProbeError::Rpc(_) => 2,
        }
    }
}

/// Connects to the target and calls `grpc.health.v1.Health/Check`.
async fn run(cli: &Cli) -> Result<ServingStatus, ProbeError> {
    let endpoint =
        Endpoint::from_shared(format!("http://{}", cli.target())).map_err(ProbeError::Transport)?;
    let channel = endpoint.connect().await.map_err(ProbeError::Transport)?;
    let mut client = HealthClient::new(channel);

    let request = HealthCheckRequest {
        service: cli.service.clone().unwrap_or_default(),
    };
    let status = client
        .check(request)
        .await
        .map_err(ProbeError::Rpc)?
        .into_inner()
        .status;

    // An out-of-range enum value is treated as not serving.
    Ok(ServingStatus::try_from(status).unwrap_or(ServingStatus::Unknown))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli).await {
        Ok(status) => {
            println!("status: {}", status.as_str_name());
            ExitCode::from(status_exit_code(status))
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(err.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_resolves_to_localhost() {
        let cli = Cli {
            addr: None,
            port: Some(50051),
            service: None,
        };
        assert_eq!(cli.target(), "localhost:50051");
    }

    #[test]
    fn addr_passes_through() {
        let cli = Cli {
            addr: Some("example.test:1234".into()),
            port: None,
            service: None,
        };
        assert_eq!(cli.target(), "example.test:1234");
    }

    #[test]
    fn addr_and_port_are_mutually_exclusive() {
        let result = Cli::try_parse_from(["grpcknock", "--addr", "h:1", "--port", "2"]);
        assert!(result.is_err());
    }

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
        let err = Endpoint::from_shared(String::new()).unwrap_err();
        assert_eq!(ProbeError::Transport(err).exit_code(), 1);
    }

    #[test]
    fn rpc_error_exits_two() {
        let err = tonic::Status::internal("rpc failed");
        assert_eq!(ProbeError::Rpc(err).exit_code(), 2);
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

        let cli = Cli {
            addr: Some(addr.to_string()),
            port: None,
            service: None,
        };
        let status = run(&cli).await.expect("check should succeed");
        assert_eq!(status_exit_code(status), 0);

        server.abort();
    }
}
