//! Command-line interface: flags, argument groups and validation.

use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{ArgGroup, CommandFactory, Parser};

use crate::endpoint::Target;
use crate::output::OutputFormat;
use crate::probe::{MetaPair, ProbeParams};
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

    /// Shortcut for localhost:PORT
    #[arg(long)]
    pub(crate) port: Option<u16>,

    /// Service name to check; repeat to check several, omit for overall health
    #[arg(long)]
    pub(crate) service: Vec<String>,

    /// gRPC metadata as key=value, attached to every request; repeatable
    #[arg(long, visible_alias = "rpc-header", value_parser = MetaPair::parse)]
    pub(crate) metadata: Vec<MetaPair>,

    /// Retry transient connection failures up to N times
    #[arg(long, default_value_t = 0)]
    pub(crate) retry: u32,

    /// Timeout for establishing the connection, e.g. 500ms, 2s, 1m
    #[arg(long, value_parser = parse_duration)]
    pub(crate) connect_timeout: Option<Duration>,

    /// Timeout for the health request, e.g. 500ms, 2s, 1m
    #[arg(long, value_parser = parse_duration)]
    pub(crate) timeout: Option<Duration>,

    /// In --watch mode, exit after N consecutive non-serving updates
    #[arg(long, requires = "watch", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) watch_failures: Option<u32>,

    /// Print the endpoint and service alongside the status
    #[arg(long, short = 'v')]
    pub(crate) verbose: bool,

    /// Print the result as JSON; an array when several services are checked
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
    /// Resolves the endpoint to dial from `--addr` or `--port`.
    /// `--addr` is already validated into a [`Target`] during parsing.
    pub(crate) fn target(&self) -> Target {
        match (&self.addr, self.port) {
            (Some(addr), _) => addr.clone(),
            (_, Some(port)) => Target::localhost(port),
            (None, None) => unreachable!("clap requires one of --addr or --port"),
        }
    }

    /// Cross-flag rules clap cannot express declaratively. `--watch` follows a single stream,
    /// so more than one `--service` is ambiguous: reject it rather than silently picking one.
    pub(crate) fn validate(&self) -> Result<(), clap::Error> {
        if self.watch && self.service.len() > 1 {
            return Err(Self::command().error(
                ErrorKind::ArgumentConflict,
                "--watch follows a single service; remove the extra --service values",
            ));
        }
        Ok(())
    }

    /// Assembles the transport parameters for a probe from the parsed flags.
    pub(crate) fn probe_params(&self) -> ProbeParams {
        ProbeParams {
            target: self.target(),
            tls_mode: self.tls_mode(),
            services: self.service.clone(),
            metadata: self.metadata.clone(),
            retry: self.retry,
            connect_timeout: self.connect_timeout,
            timeout: self.timeout,
            watch_failures: self.watch_failures,
        }
    }

    /// TLS setup from the flags.
    /// `--ca-cert` and `--tls-no-verify` both imply TLS; the parser rejects their combination.
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

    /// Output format chosen by the mutually exclusive `--verbose`/`--json`/`--quiet` flags;
    /// the plain status line when none is set.
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

/// Why a duration flag value could not be parsed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DurationParseError {
    #[error("duration {0:?} must be a number followed by a unit (ms, s or m)")]
    MissingUnit(String),
    #[error("duration {0:?} is missing a number")]
    MissingNumber(String),
    #[error("duration {0:?} has an invalid number: {1}")]
    InvalidNumber(String, std::num::ParseIntError),
    #[error("duration {raw:?} has an unknown unit {unit:?} (use ms, s or m)")]
    UnknownUnit { raw: String, unit: String },
    #[error("duration {0:?} is too large")]
    Overflow(String),
    #[error("duration {0:?} must be greater than zero (omit the flag for no timeout)")]
    Zero(String),
}

/// Parses a duration like `500ms`, `2s` or `1m`. A unit is required,
/// matching grpc-health-probe's Go-style durations;
/// bare numbers are rejected so the time scale is never ambiguous.
/// Zero is rejected because a zero timeout fires instantly -
/// "no timeout" is expressed by omitting the flag.
fn parse_duration(raw: &str) -> Result<Duration, DurationParseError> {
    let split = raw
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| DurationParseError::MissingUnit(raw.to_string()))?;
    if split == 0 {
        return Err(DurationParseError::MissingNumber(raw.to_string()));
    }
    let (number, unit) = raw.split_at(split);
    let value: u64 = number
        .parse()
        .map_err(|e| DurationParseError::InvalidNumber(raw.to_string(), e))?;
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => value
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(|| DurationParseError::Overflow(raw.to_string()))?,
        other => {
            return Err(DurationParseError::UnknownUnit {
                raw: raw.to_string(),
                unit: other.to_string(),
            });
        }
    };
    if duration.is_zero() {
        return Err(DurationParseError::Zero(raw.to_string()));
    }
    Ok(duration)
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

    #[test]
    fn service_flag_collects_multiple_values() {
        let cli = Cli::try_parse_from([
            "grpcknock",
            "--port",
            "1",
            "--service",
            "a",
            "--service",
            "b",
        ])
        .unwrap();
        assert_eq!(cli.service, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn rpc_header_is_an_alias_for_metadata() {
        // Both spellings feed the same Vec, so a grpc-health-probe user's
        // --rpc-header keeps working.
        let cli = Cli::try_parse_from([
            "grpcknock",
            "--port",
            "1",
            "--metadata",
            "a=1",
            "--rpc-header",
            "b=2",
        ])
        .unwrap();
        assert_eq!(cli.metadata.len(), 2);
    }

    #[test]
    fn invalid_metadata_is_rejected_at_parse() {
        let result = Cli::try_parse_from(["grpcknock", "--port", "1", "--metadata", "noeq"]);
        assert!(result.is_err());
    }

    #[test]
    fn duration_flags_accept_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn duration_requires_a_unit() {
        assert!(matches!(
            parse_duration("10"),
            Err(DurationParseError::MissingUnit(_))
        ));
    }

    #[test]
    fn duration_rejects_unknown_unit() {
        assert!(matches!(
            parse_duration("5x"),
            Err(DurationParseError::UnknownUnit { .. })
        ));
    }

    #[test]
    fn duration_rejects_a_missing_number() {
        assert!(matches!(
            parse_duration("ms"),
            Err(DurationParseError::MissingNumber(_))
        ));
    }

    #[test]
    fn duration_rejects_zero() {
        // A zero timeout fires instantly; reject it so it is not a silent footgun.
        assert!(matches!(
            parse_duration("0s"),
            Err(DurationParseError::Zero(_))
        ));
        assert!(matches!(
            parse_duration("0ms"),
            Err(DurationParseError::Zero(_))
        ));
    }

    #[test]
    fn watch_failures_rejects_zero() {
        let result = Cli::try_parse_from([
            "grpcknock",
            "--port",
            "1",
            "--watch",
            "--watch-failures",
            "0",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn watch_failures_requires_watch() {
        // Without this pairing the flag would be accepted and silently ignored.
        let result = Cli::try_parse_from(["grpcknock", "--port", "1", "--watch-failures", "3"]);
        assert!(result.is_err());
    }

    #[test]
    fn watch_with_several_services_is_rejected() {
        let cli = Cli::try_parse_from([
            "grpcknock",
            "--port",
            "1",
            "--watch",
            "--service",
            "a",
            "--service",
            "b",
        ])
        .unwrap();
        assert!(cli.validate().is_err());
    }

    #[test]
    fn watch_with_one_service_is_allowed() {
        let cli =
            Cli::try_parse_from(["grpcknock", "--port", "1", "--watch", "--service", "a"]).unwrap();
        assert!(cli.validate().is_ok());
    }
}
