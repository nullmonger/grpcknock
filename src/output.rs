//! Rendering of probe results into the format selected on the command line.

use tonic_health::pb::health_check_response::ServingStatus;

use crate::probe::{ProbeError, status_exit_code};

/// How a probe result is written out. The exit code does not depend on the
/// format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Status word on success, error text on stderr (default).
    Default,
    /// Human-readable detail: endpoint, service and status.
    Verbose,
    /// A single machine-readable JSON object.
    Json,
    /// Nothing printed; rely on the exit code alone.
    Quiet,
}

/// A single probe result: the endpoint dialed, the service checked, and the
/// outcome.
pub(crate) struct ProbeReport {
    pub(crate) endpoint: String,
    pub(crate) service: Option<String>,
    pub(crate) outcome: Outcome,
}

/// The outcome of one probe: a reported serving status or a failure.
pub(crate) enum Outcome {
    Status(ServingStatus),
    Error(ProbeError),
}

/// What to write to each stream; `None` means nothing goes to that stream.
pub(crate) struct Rendered {
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
}

impl ProbeReport {
    /// Process exit code for this result, following grpc-health-probe.
    pub(crate) fn exit_code(&self) -> u8 {
        match &self.outcome {
            Outcome::Status(status) => status_exit_code(*status),
            Outcome::Error(err) => err.exit_code(),
        }
    }

    /// Service label for human-readable output, or "(overall)" when no service is set.
    fn service_label(&self) -> &str {
        self.service.as_deref().unwrap_or("(overall)")
    }
}

/// Renders a probe result in the requested format.
pub(crate) fn render(report: &ProbeReport, format: OutputFormat) -> Rendered {
    match format {
        OutputFormat::Default => render_default(report),
        OutputFormat::Verbose => render_verbose(report),
        OutputFormat::Json => render_json(report),
        OutputFormat::Quiet => Rendered {
            stdout: None,
            stderr: None,
        },
    }
}

/// Plain mode: the status word on stdout, or the error on stderr.
fn render_default(report: &ProbeReport) -> Rendered {
    match &report.outcome {
        Outcome::Status(status) => Rendered {
            stdout: Some(format!("status: {}", status.as_str_name())),
            stderr: None,
        },
        Outcome::Error(err) => Rendered {
            stdout: None,
            stderr: Some(err.to_string()),
        },
    }
}

/// Verbose mode: endpoint and service on every line, then status or error.
fn render_verbose(report: &ProbeReport) -> Rendered {
    let head = format!(
        "endpoint: {}\nservice: {}",
        report.endpoint,
        report.service_label()
    );
    match &report.outcome {
        Outcome::Status(status) => Rendered {
            stdout: Some(format!("{head}\nstatus: {}", status.as_str_name())),
            stderr: None,
        },
        Outcome::Error(err) => Rendered {
            stdout: None,
            stderr: Some(format!("{head}\nerror: {err}")),
        },
    }
}

/// JSON mode: one object on stdout, success and failure alike, so a parser
/// always reads a single stream.
fn render_json(report: &ProbeReport) -> Rendered {
    let mut obj = serde_json::json!({
        "endpoint": report.endpoint,
        "service": report.service,
    });
    match &report.outcome {
        Outcome::Status(status) => obj["status"] = serde_json::json!(status.as_str_name()),
        Outcome::Error(err) => obj["error"] = serde_json::json!(err.to_string()),
    }
    Rendered {
        stdout: Some(obj.to_string()),
        stderr: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ProbeError;
    use tonic_health::pb::health_check_response::ServingStatus;

    fn report(outcome: Outcome) -> ProbeReport {
        ProbeReport {
            endpoint: "localhost:50051".to_string(),
            service: None,
            outcome,
        }
    }

    #[test]
    fn exit_code_from_status_and_error() {
        assert_eq!(
            report(Outcome::Status(ServingStatus::Serving)).exit_code(),
            0
        );
        assert_eq!(
            report(Outcome::Status(ServingStatus::NotServing)).exit_code(),
            3
        );
        let err = ProbeError::Rpc(tonic::Status::internal("x"));
        assert_eq!(report(Outcome::Error(err)).exit_code(), 2);
    }

    #[test]
    fn default_prints_status_line_on_success() {
        let r = render(
            &report(Outcome::Status(ServingStatus::Serving)),
            OutputFormat::Default,
        );
        assert_eq!(r.stdout.as_deref(), Some("status: SERVING"));
        assert_eq!(r.stderr, None);
    }

    #[test]
    fn default_sends_error_to_stderr() {
        let err = ProbeError::Rpc(tonic::Status::internal("boom"));
        let r = render(&report(Outcome::Error(err)), OutputFormat::Default);
        assert_eq!(r.stdout, None);
        assert!(r.stderr.unwrap().contains("rpc error"));
    }

    #[test]
    fn verbose_lists_endpoint_service_status() {
        let r = render(
            &report(Outcome::Status(ServingStatus::Serving)),
            OutputFormat::Verbose,
        );
        let out = r.stdout.unwrap();
        assert!(out.contains("localhost:50051"));
        assert!(out.contains("SERVING"));
        // A missing service renders as the overall-health marker.
        assert!(out.contains("overall"));
    }

    #[test]
    fn json_success_carries_fields() {
        let report = ProbeReport {
            endpoint: "localhost:50051".to_string(),
            service: Some("demo.Serving".to_string()),
            outcome: Outcome::Status(ServingStatus::Serving),
        };
        let r = render(&report, OutputFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&r.stdout.unwrap()).unwrap();
        assert_eq!(v["status"], "SERVING");
        assert_eq!(v["endpoint"], "localhost:50051");
        assert_eq!(v["service"], "demo.Serving");
    }

    #[test]
    fn json_error_carries_error_field() {
        let err = ProbeError::Rpc(tonic::Status::internal("boom"));
        let r = render(&report(Outcome::Error(err)), OutputFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&r.stdout.unwrap()).unwrap();
        assert!(v["error"].is_string());
        assert_eq!(v["service"], serde_json::Value::Null);
    }

    #[test]
    fn quiet_prints_nothing() {
        let r = render(
            &report(Outcome::Status(ServingStatus::Serving)),
            OutputFormat::Quiet,
        );
        assert_eq!(r.stdout, None);
        assert_eq!(r.stderr, None);
    }
}
