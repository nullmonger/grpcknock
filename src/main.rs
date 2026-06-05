use std::process::ExitCode;

use clap::Parser;
use tonic_health::pb::health_check_response::ServingStatus;

mod cli;
mod endpoint;
mod output;
mod probe;
mod tls;

use cli::Cli;
use output::{Outcome, ProbeReport, Rendered, render, render_run};
use probe::{
    ProbeError, ProbeParams, decode_status, open_watch, run, watch_exit_code, worst_exit_code,
};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(err) = cli.validate() {
        err.exit();
    }
    let params = cli.probe_params();
    // Metadata commonly carries credentials; warn when it would travel in the
    // clear so a token is not leaked over a plaintext connection unnoticed.
    if !params.metadata.is_empty() && !params.tls_mode.is_enabled() {
        eprintln!("warning: metadata is sent over an unencrypted connection; add --tls");
    }
    let format = cli.output_format();
    let code = if cli.watch {
        run_watch(&params, format).await
    } else {
        run_check(&params, format).await
    };
    ExitCode::from(code)
}

/// Builds a single-service report for the watch stream against the probed target.
fn report(params: &ProbeParams, outcome: Outcome) -> ProbeReport {
    let service = params.watch_service();
    ProbeReport {
        endpoint: params.target.to_string(),
        service: (!service.is_empty()).then_some(service),
        outcome,
    }
}

/// One-shot `Check` across every requested service:
/// probe, report each, and exit by the worst outcome.
async fn run_check(params: &ProbeParams, format: output::OutputFormat) -> u8 {
    match run(params).await {
        Ok(results) => {
            let reports: Vec<ProbeReport> = results
                .into_iter()
                .map(|r| ProbeReport {
                    endpoint: params.target.to_string(),
                    service: r.service,
                    outcome: match r.result {
                        Ok(status) => Outcome::Status(status),
                        Err(err) => Outcome::Error(err),
                    },
                })
                .collect();
            emit(render_run(&reports, format));
            worst_exit_code(reports.iter().map(ProbeReport::exit_code))
        }
        // A connect-level failure aborts the whole run: report it once.
        Err(err) => {
            let report = ProbeReport {
                endpoint: params.target.to_string(),
                service: None,
                outcome: Outcome::Error(err),
            };
            emit(render(&report, format));
            report.exit_code()
        }
    }
}

/// `Watch`: stream status updates, printing each,
/// until the server closes the stream or a shutdown signal arrives.
/// Exits by the last status observed - "observed" means a status this loop actually read,
/// so a shutdown signal finishes on the most recent printed status.
async fn run_watch(params: &ProbeParams, format: output::OutputFormat) -> u8 {
    let mut stream = match open_watch(params).await {
        Ok(stream) => stream,
        Err(err) => {
            let report = report(params, Outcome::Error(err));
            emit(render(&report, format));
            return report.exit_code();
        }
    };

    // Registered once, before the loop, so SIGTERM is not re-armed each tick.
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let mut last: Option<ServingStatus> = None;
    let mut consecutive_failures: u32 = 0;
    loop {
        tokio::select! {
            message = stream.message() => match message {
                Ok(Some(response)) => {
                    let status = decode_status(response.status);
                    last = Some(status);
                    emit(render(&report(params, Outcome::Status(status)), format));
                    // --watch-failures: stop after N non-serving updates in a row;
                    // a return to SERVING clears the streak.
                    if status == ServingStatus::Serving {
                        consecutive_failures = 0;
                    } else {
                        consecutive_failures += 1;
                        if params.watch_failures.is_some_and(|limit| consecutive_failures >= limit) {
                            break;
                        }
                    }
                }
                Ok(None) => break, // server closed the stream
                Err(status) => {
                    let report = report(params, Outcome::Error(ProbeError::Rpc(status)));
                    emit(render(&report, format));
                    return report.exit_code();
                }
            },
            _ = &mut shutdown => break, // Ctrl-C / SIGTERM
        }
    }
    watch_exit_code(last)
}

/// Writes a rendered result to stdout/stderr.
fn emit(rendered: Rendered) {
    if let Some(out) = rendered.stdout {
        println!("{out}");
    }
    if let Some(err) = rendered.stderr {
        eprintln!("{err}");
    }
}

/// Resolves once an interrupt (Ctrl-C) or termination (SIGTERM) signal arrives.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
