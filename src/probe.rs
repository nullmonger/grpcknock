//! Dialing the target and calling the Health service.

use std::future::Future;
use std::time::Duration;

use tonic::metadata::{AsciiMetadataKey, AsciiMetadataValue};
use tonic::transport::{Channel, Endpoint};
use tonic_health::pb::HealthCheckRequest;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;

use crate::endpoint::Target;
use crate::tls::{self, TlsError, TlsMode};

/// Fixed pause between retry attempts. Kept constant so `--retry` behaviour is predictable
/// rather than a backoff curve the caller has to reason about.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Everything the transport needs to probe a target,
/// assembled once from the CLI. Keeps `probe` independent of the argument parser.
pub(crate) struct ProbeParams {
    pub(crate) target: Target,
    pub(crate) tls_mode: TlsMode,
    pub(crate) services: Vec<String>,
    pub(crate) metadata: Vec<MetaPair>,
    pub(crate) retry: u32,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) watch_failures: Option<u32>,
}

impl ProbeParams {
    /// Services to check, one Health request each. An empty list means
    /// a single overall-health probe (empty service name), matching the no-flag default.
    fn service_list(&self) -> Vec<String> {
        if self.services.is_empty() {
            vec![String::new()]
        } else {
            self.services.clone()
        }
    }

    /// The single service a `--watch` stream follows; empty means overall health.
    pub(crate) fn watch_service(&self) -> String {
        self.services.first().cloned().unwrap_or_default()
    }
}

/// One gRPC metadata header attached to outgoing requests.
/// Validated at parse time so request assembly never fails on a bad key or value.
#[derive(Clone)]
pub(crate) struct MetaPair {
    key: AsciiMetadataKey,
    value: AsciiMetadataValue,
}

/// Why a `--metadata` value could not be parsed into a header.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MetaParseError {
    #[error("metadata {0:?} must be key=value")]
    MissingEquals(String),
    #[error("metadata {0:?} has an empty key")]
    EmptyKey(String),
    #[error("metadata key {key:?} is not a valid header name: {detail}")]
    InvalidKey { key: String, detail: String },
    #[error("metadata value for {key:?} is not valid: {detail}")]
    InvalidValue { key: String, detail: String },
}

impl MetaPair {
    /// Parses a `key=value` header.
    /// The value keeps everything after the first `=`, so values may themselves contain `=`.
    pub(crate) fn parse(raw: &str) -> Result<MetaPair, MetaParseError> {
        let (key, value) = raw
            .split_once('=')
            .ok_or_else(|| MetaParseError::MissingEquals(raw.to_string()))?;
        if key.is_empty() {
            return Err(MetaParseError::EmptyKey(raw.to_string()));
        }
        let key = AsciiMetadataKey::from_bytes(key.as_bytes()).map_err(|e| {
            MetaParseError::InvalidKey {
                key: key.to_string(),
                detail: e.to_string(),
            }
        })?;
        let value =
            AsciiMetadataValue::try_from(value).map_err(|e| MetaParseError::InvalidValue {
                key: key.as_str().to_string(),
                detail: e.to_string(),
            })?;
        Ok(MetaPair { key, value })
    }
}

/// Attaches the parsed metadata headers to an outgoing request.
fn apply_metadata<T>(request: &mut tonic::Request<T>, pairs: &[MetaPair]) {
    let md = request.metadata_mut();
    for pair in pairs {
        md.append(pair.key.clone(), pair.value.clone());
    }
}

/// Decodes a wire status code, treating an out-of-range value as UNKNOWN.
pub(crate) fn decode_status(raw: i32) -> ServingStatus {
    ServingStatus::try_from(raw).unwrap_or(ServingStatus::Unknown)
}

/// Maps a serving status to a process exit code:
/// SERVING is healthy, anything else is a failure.
pub(crate) fn status_exit_code(status: ServingStatus) -> u8 {
    match status {
        ServingStatus::Serving => 0,
        _ => 3,
    }
}

/// Exit code for a finished watch, based on the last status seen.
/// With no status observed at all (the stream closed or was interrupted before any update),
/// the server was never confirmed healthy, so this is a failure.
pub(crate) fn watch_exit_code(last: Option<ServingStatus>) -> u8 {
    match last {
        Some(status) => status_exit_code(status),
        None => 3,
    }
}

/// Severity rank of an exit code for worst-of aggregation across services.
/// Higher wins: a total outage (connection) outranks a hung call (timeout),
/// which outranks an invocation error, which outranks a valid not-serving answer,
/// which outranks success.
fn severity(code: u8) -> u8 {
    match code {
        0 => 0,
        3 => 1,
        2 => 2,
        4 => 3,
        1 => 4,
        _ => 5,
    }
}

/// The exit code of the most severe outcome among several services.
/// Empty input is success.
pub(crate) fn worst_exit_code(codes: impl IntoIterator<Item = u8>) -> u8 {
    codes
        .into_iter()
        .max_by_key(|code| severity(*code))
        .unwrap_or(0)
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
    #[error("connection timed out")]
    ConnectTimeout,
    #[error("request timed out")]
    RequestTimeout,
}

impl ProbeError {
    /// Exit code for a failed probe.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            ProbeError::Transport(_) => 1,
            ProbeError::Rpc(_) => 2,
            ProbeError::Tls(_) => 1,
            ProbeError::ConnectTimeout | ProbeError::RequestTimeout => 4,
        }
    }
}

/// Whether a connect failure is worth retrying.
/// A refused/reset connection or a connect timeout is transient
/// (the server may still be coming up); a TLS failure or an rpc error is deterministic.
// FIXME: a TLS handshake failure (bad certificate, hostname mismatch)
// surfaces from connect() as ProbeError::Transport and is retried
// even though it is deterministic; only CA config errors map to ProbeError::Tls.
fn is_retryable(err: &ProbeError) -> bool {
    matches!(err, ProbeError::Transport(_) | ProbeError::ConnectTimeout)
}

/// The result of probing one service: the service name checked
/// (None for the overall health) and either its status or the failure of that single call.
pub(crate) struct ServiceResult {
    pub(crate) service: Option<String>,
    pub(crate) result: Result<ServingStatus, ProbeError>,
}

/// Runs `attempt` and retries it, up to `retries` times, while it fails with a retryable error.
/// Generic over the attempt so the retry policy is unit testable without a live server.
async fn retry_loop<F, Fut, T>(
    retries: u32,
    delay: Duration,
    mut attempt: F,
) -> Result<T, ProbeError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProbeError>>,
{
    let mut remaining = retries;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(err) if remaining > 0 && is_retryable(&err) => {
                remaining -= 1;
                tokio::time::sleep(delay).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// Dials the target, applying TLS and the connect timeout,
/// and returns a Health client. A connect timeout maps to its own error so it exits 4, not 1.
async fn connect(params: &ProbeParams) -> Result<HealthClient<Channel>, ProbeError> {
    let endpoint = Endpoint::from_shared(params.target.uri(params.tls_mode.is_enabled()))
        .map_err(ProbeError::Transport)?;
    let endpoint = tls::configure(endpoint, &params.tls_mode, params.target.host())?;
    let connecting = endpoint.connect();
    let channel = match params.connect_timeout {
        Some(limit) => tokio::time::timeout(limit, connecting)
            .await
            .map_err(|_| ProbeError::ConnectTimeout)?
            .map_err(ProbeError::Transport)?,
        None => connecting.await.map_err(ProbeError::Transport)?,
    };
    Ok(HealthClient::new(channel))
}

/// Connects with retries. Only connection establishment is retried;
/// once the channel is up, per-service calls are reported as they come.
async fn connect_with_retry(params: &ProbeParams) -> Result<HealthClient<Channel>, ProbeError> {
    retry_loop(params.retry, RETRY_DELAY, || connect(params)).await
}

/// Calls `grpc.health.v1.Health/Check` for one service,
/// applying metadata and the per-request timeout. A request timeout maps to exit 4.
async fn check_service(
    client: &mut HealthClient<Channel>,
    params: &ProbeParams,
    service: &str,
) -> Result<ServingStatus, ProbeError> {
    let mut request = tonic::Request::new(HealthCheckRequest {
        service: service.to_string(),
    });
    apply_metadata(&mut request, &params.metadata);
    let call = client.check(request);
    let response = match params.timeout {
        Some(limit) => tokio::time::timeout(limit, call)
            .await
            .map_err(|_| ProbeError::RequestTimeout)?
            .map_err(ProbeError::Rpc)?,
        None => call.await.map_err(ProbeError::Rpc)?,
    };
    Ok(decode_status(response.into_inner().status))
}

/// Connects once and checks every requested service over the same channel.
/// A connect failure aborts the whole run;
/// per-service failures are captured individually so the worst outcome can set the exit code.
pub(crate) async fn run(params: &ProbeParams) -> Result<Vec<ServiceResult>, ProbeError> {
    let mut client = connect_with_retry(params).await?;
    let mut results = Vec::new();
    for service in params.service_list() {
        let result = check_service(&mut client, params, &service).await;
        results.push(ServiceResult {
            service: nonempty(service),
            result,
        });
    }
    Ok(results)
}

/// Maps the empty (overall-health) service name to None for reporting.
fn nonempty(service: String) -> Option<String> {
    if service.is_empty() {
        None
    } else {
        Some(service)
    }
}

/// Opens the `grpc.health.v1.Health/Watch` stream for the target's service.
/// `--timeout` bounds opening the stream, not its lifetime.
pub(crate) async fn open_watch(
    params: &ProbeParams,
) -> Result<tonic::Streaming<tonic_health::pb::HealthCheckResponse>, ProbeError> {
    let mut client = connect_with_retry(params).await?;
    let mut request = tonic::Request::new(HealthCheckRequest {
        service: params.watch_service(),
    });
    apply_metadata(&mut request, &params.metadata);
    let opening = client.watch(request);
    let stream = match params.timeout {
        Some(limit) => tokio::time::timeout(limit, opening)
            .await
            .map_err(|_| ProbeError::RequestTimeout)?
            .map_err(ProbeError::Rpc)?
            .into_inner(),
        None => opening.await.map_err(ProbeError::Rpc)?.into_inner(),
    };
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::cell::Cell;

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
    fn timeout_errors_exit_four() {
        assert_eq!(ProbeError::ConnectTimeout.exit_code(), 4);
        assert_eq!(ProbeError::RequestTimeout.exit_code(), 4);
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
        // A TLS setup failure (e.g. an unreadable CA file) is a connection-level failure,
        // like a refused dial.
        let err = TlsError::ReadCa {
            path: "/nope.pem".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert_eq!(ProbeError::Tls(err).exit_code(), 1);
    }

    #[test]
    fn worst_exit_code_picks_most_severe() {
        assert_eq!(worst_exit_code(std::iter::empty::<u8>()), 0);
        assert_eq!(worst_exit_code([0, 0]), 0);
        // A failure always beats SERVING.
        assert_eq!(worst_exit_code([0, 3]), 3);
        // Invocation error beats a valid not-serving answer.
        assert_eq!(worst_exit_code([3, 2]), 2);
        // Timeout beats an invocation error.
        assert_eq!(worst_exit_code([2, 4]), 4);
        // A total outage beats a timeout.
        assert_eq!(worst_exit_code([4, 1]), 1);
    }

    #[test]
    fn only_transient_connect_failures_retry() {
        let transport = Endpoint::from_shared("http://[".to_string()).unwrap_err();
        assert!(is_retryable(&ProbeError::Transport(transport)));
        assert!(is_retryable(&ProbeError::ConnectTimeout));
        assert!(!is_retryable(&ProbeError::RequestTimeout));
        assert!(!is_retryable(&ProbeError::Rpc(tonic::Status::internal(
            "x"
        ))));
    }

    #[tokio::test]
    async fn retry_loop_stops_after_the_limit() {
        let calls = Cell::new(0);
        let result: Result<(), ProbeError> = retry_loop(3, Duration::ZERO, || {
            calls.set(calls.get() + 1);
            async { Err(ProbeError::ConnectTimeout) }
        })
        .await;
        assert!(result.is_err());
        // One initial attempt plus three retries.
        assert_eq!(calls.get(), 4);
    }

    #[tokio::test]
    async fn retry_loop_does_not_retry_deterministic_errors() {
        let calls = Cell::new(0);
        let result: Result<(), ProbeError> = retry_loop(3, Duration::ZERO, || {
            calls.set(calls.get() + 1);
            async { Err(ProbeError::Rpc(tonic::Status::not_found("x"))) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn retry_loop_returns_first_success() {
        let calls = Cell::new(0);
        let result: Result<u8, ProbeError> = retry_loop(3, Duration::ZERO, || {
            calls.set(calls.get() + 1);
            async { Ok(7u8) }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn metadata_parses_key_and_value() {
        let pair = MetaPair::parse("authorization=Bearer token").unwrap();
        assert_eq!(pair.key.as_str(), "authorization");
        assert_eq!(pair.value.to_str().unwrap(), "Bearer token");
    }

    #[test]
    fn metadata_keeps_equals_in_value() {
        let pair = MetaPair::parse("token=a=b=c").unwrap();
        assert_eq!(pair.value.to_str().unwrap(), "a=b=c");
    }

    #[test]
    fn metadata_rejects_missing_equals() {
        assert!(matches!(
            MetaPair::parse("noeq"),
            Err(MetaParseError::MissingEquals(_))
        ));
    }

    #[test]
    fn metadata_rejects_empty_key() {
        assert!(matches!(
            MetaPair::parse("=value"),
            Err(MetaParseError::EmptyKey(_))
        ));
    }

    // End-to-end: a real Health server reporting SERVING
    // drives the whole connect -> Check -> status path to a zero exit code.
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
        let results = run(&params).await.expect("check should succeed");
        assert_eq!(results.len(), 1);
        assert_eq!(
            status_exit_code(results[0].result.as_ref().copied().unwrap()),
            0
        );

        server.abort();
    }

    // End-to-end: several services over one connection
    // aggregate to the worst outcome (SERVING + NOT_SERVING -> exit 3).
    #[tokio::test]
    async fn check_aggregates_multiple_services() {
        use tokio::net::TcpListener;
        use tonic::transport::Server;
        use tonic::transport::server::TcpIncoming;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (reporter, health_service) = tonic_health::server::health_reporter();
        reporter
            .set_service_status("demo.Serving", tonic_health::ServingStatus::Serving)
            .await;
        reporter
            .set_service_status("demo.NotServing", tonic_health::ServingStatus::NotServing)
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
            "demo.Serving",
            "--service",
            "demo.NotServing",
        ])
        .unwrap()
        .probe_params();
        let results = run(&params).await.expect("checks should complete");
        assert_eq!(results.len(), 2);
        let codes = results.iter().map(|r| match &r.result {
            Ok(status) => status_exit_code(*status),
            Err(err) => err.exit_code(),
        });
        assert_eq!(worst_exit_code(codes), 3);

        server.abort();
    }

    // End-to-end: Health/Watch streams the current status,
    // then each change the server reports.
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
