//! Command-line interface: flags, argument groups and validation.

use std::path::PathBuf;

use clap::{ArgGroup, Parser};

use crate::endpoint::Target;
use crate::output::OutputFormat;
use crate::probe::ProbeParams;
use crate::tls::TlsMode;

/// gRPC health check probe.
#[derive(Parser)]
#[command(version, about)]
#[command(group(ArgGroup::new("target").required(true).args(["addr", "port"])))]
#[command(group(ArgGroup::new("output").args(["verbose", "json", "quiet"])))]
pub(crate) struct Cli {
    /// Target address as host:port
    #[arg(long, value_parser = Target::parse_addr)]
    pub(crate) addr: Option<Target>,

    /// Shortcut for localhost:<port>
    #[arg(long)]
    pub(crate) port: Option<u16>,

    // TODO: accept multiple services (Vec<String>) once multi-service checks
    // are in scope; the health protocol allows probing several in one run.
    /// Service name to check; omit to check overall server health
    #[arg(long)]
    pub(crate) service: Option<String>,

    /// Print the status word alongside the exit code
    #[arg(long, short = 'v')]
    pub(crate) verbose: bool,

    /// Print the result as a single JSON object
    #[arg(long)]
    pub(crate) json: bool,

    /// Suppress output; rely on the exit code only
    #[arg(long, short = 'q')]
    pub(crate) quiet: bool,

    /// Connect over TLS using the system's trusted roots
    #[arg(long)]
    pub(crate) tls: bool,

    /// Verify the server against this PEM CA certificate (implies --tls)
    #[arg(long, value_name = "PATH")]
    pub(crate) ca_cert: Option<PathBuf>,

    /// DANGEROUS: skip TLS certificate verification (implies --tls; debugging only)
    #[arg(long, conflicts_with = "ca_cert")]
    pub(crate) tls_no_verify: bool,

    /// Stream Health/Watch updates instead of a single Check
    #[arg(long)]
    pub(crate) watch: bool,
}

impl Cli {
    /// Resolves the endpoint to dial from `--addr` or `--port`. `--addr` is
    /// already validated into a [`Target`] during parsing.
    pub(crate) fn target(&self) -> Target {
        match (&self.addr, self.port) {
            (Some(addr), _) => addr.clone(),
            (_, Some(port)) => Target::localhost(port),
            (None, None) => unreachable!("clap requires one of --addr or --port"),
        }
    }

    /// Assembles the transport parameters for a probe from the parsed flags.
    pub(crate) fn probe_params(&self) -> ProbeParams {
        ProbeParams {
            target: self.target(),
            tls_mode: self.tls_mode(),
            service: self.service.clone(),
        }
    }

    /// TLS setup from the flags. `--ca-cert` and `--tls-no-verify` both imply
    /// TLS; the parser rejects their combination.
    pub(crate) fn tls_mode(&self) -> TlsMode {
        if self.tls_no_verify {
            TlsMode::NoVerify
        } else if let Some(path) = &self.ca_cert {
            TlsMode::CustomCa(path.clone())
        } else if self.tls {
            TlsMode::SystemRoots
        } else {
            TlsMode::Disabled
        }
    }

    /// Output format chosen by the mutually exclusive `--verbose`/`--json`/`--quiet`
    /// flags; the plain status line when none is set.
    pub(crate) fn output_format(&self) -> OutputFormat {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_resolves_to_localhost() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "50051"]).unwrap();
        assert_eq!(cli.target().to_string(), "localhost:50051");
    }

    #[test]
    fn addr_passes_through() {
        let cli = Cli::try_parse_from(["grpcknock", "--addr", "example.test:1234"]).unwrap();
        assert_eq!(cli.target().to_string(), "example.test:1234");
    }

    #[test]
    fn addr_without_port_is_rejected_at_parse() {
        let result = Cli::try_parse_from(["grpcknock", "--addr", "example.test"]);
        assert!(result.is_err());
    }

    #[test]
    fn addr_and_port_are_mutually_exclusive() {
        let result = Cli::try_parse_from(["grpcknock", "--addr", "h:1", "--port", "2"]);
        assert!(result.is_err());
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

    #[test]
    fn no_tls_flag_is_disabled() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1"]).unwrap();
        assert_eq!(cli.tls_mode(), TlsMode::Disabled);
    }

    #[test]
    fn tls_flag_uses_system_roots() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1", "--tls"]).unwrap();
        assert_eq!(cli.tls_mode(), TlsMode::SystemRoots);
    }

    #[test]
    fn ca_cert_implies_custom_ca() {
        let cli =
            Cli::try_parse_from(["grpcknock", "--port", "1", "--ca-cert", "/tmp/ca.pem"]).unwrap();
        assert_eq!(cli.tls_mode(), TlsMode::CustomCa("/tmp/ca.pem".into()));
    }

    #[test]
    fn tls_no_verify_implies_no_verify() {
        let cli = Cli::try_parse_from(["grpcknock", "--port", "1", "--tls-no-verify"]).unwrap();
        assert_eq!(cli.tls_mode(), TlsMode::NoVerify);
    }

    #[test]
    fn ca_cert_and_no_verify_conflict() {
        let result = Cli::try_parse_from([
            "grpcknock",
            "--port",
            "1",
            "--ca-cert",
            "/tmp/ca.pem",
            "--tls-no-verify",
        ]);
        assert!(result.is_err());
    }
}
