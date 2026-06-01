use std::process::ExitCode;

use clap::{ArgGroup, Parser};
use tonic::transport::Endpoint;
use tonic_health::pb::HealthCheckRequest;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;

mod output;

use output::{Outcome, OutputFormat, ProbeReport, render};

/// gRPC health check probe.
#[derive(Parser)]
#[command(version, about)]
#[command(group(ArgGroup::new("target").required(true).args(["addr", "port"])))]
#[command(group(ArgGroup::new("output").args(["verbose", "json", "quiet"])))]
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

    /// Print the status word alongside the exit code
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Print the result as a single JSON object
    #[arg(long)]
    json: bool,

    /// Suppress output; rely on the exit code only
    #[arg(long, short = 'q')]
    quiet: bool,
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

    /// Output format chosen by the mutually exclusive `--verbose`/`--json`/`--quiet`
    /// flags; the plain status line when none is set.
    fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else if self.quiet {
            OutputFormat::Quiet
        } else if self.verbose {
            OutputFormat::Verbose
        } else {
            OutputFormat::Default
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
    // TODO: carve timeout into exit code 4 once --timeout and --connect-timeout
    // land - both Transport (connect timeout) and Rpc (DEADLINE_EXCEEDED).
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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match run(&cli).await {
        Ok(status) => Outcome::Status(status),
        Err(err) => Outcome::Error(err),
    };
    let report = ProbeReport {
        endpoint: cli.target(),
        service: cli.service.clone(),
        outcome,
    };

    let rendered = render(&report, cli.output_format());
    if let Some(out) = rendered.stdout {
        println!("{out}");
    }
    if let Some(err) = rendered.stderr {
        eprintln!("{err}");
    }
    ExitCode::from(report.exit_code())
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
            verbose: false,
            json: false,
            quiet: false,
        };
        assert_eq!(cli.target(), "localhost:50051");
    }

    #[test]
    fn addr_passes_through() {
        let cli = Cli {
            addr: Some("example.test:1234".into()),
            port: None,
            service: None,
            verbose: false,
            json: false,
            quiet: false,
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
    fn no_output_flag_selects_default() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1"]).unwrap();
        assert_eq!(cli.output_format(), OutputFormat::Default);
    }

    #[test]
    fn json_flag_selects_json() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1", "--json"]).unwrap();
        assert_eq!(cli.output_format(), OutputFormat::Json);
    }

    #[test]
    fn verbose_flag_selects_verbose() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1", "--verbose"]).unwrap();
        assert_eq!(cli.output_format(), OutputFormat::Verbose);
    }

    #[test]
    fn quiet_flag_selects_quiet() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1", "--quiet"]).unwrap();
        assert_eq!(cli.output_format(), OutputFormat::Quiet);
    }

    #[test]
    fn output_flags_are_mutually_exclusive() {
        let result = Cli::try_parse_from(["grpcknock", "--port", "1", "--json", "--quiet"]);
        assert!(result.is_err());
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
            verbose: false,
            json: false,
            quiet: false,
        };
        let status = run(&cli).await.expect("check should succeed");
        assert_eq!(status_exit_code(status), 0);

        server.abort();
    }
}
