use clap::{ArgGroup, Parser};

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

fn main() {
    let cli = Cli::parse();
    // TODO: map the probe outcome to grpc-health-probe exit codes (0 SERVING,
    // 1 connection error, 3 NOT_SERVING/UNKNOWN, 4 timeout) via process::exit;
    // clap already exits 2 on invalid arguments. For now this prints the
    // resolved target as a placeholder for the Health/Check step.
    println!("{}", cli.target());
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
}
