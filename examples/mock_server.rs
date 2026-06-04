//! Mock health server for manual probing of the CLI.
//!
//! Listens on `127.0.0.1:50051`. Most service names hold a fixed status so that
//! every exit code path can be exercised; `demo.Flapping` toggles between
//! SERVING and NOT_SERVING every few seconds so `--watch` shows a live stream.
//!
//! ```text
//! cargo run --example mock_server
//!
//! cargo run -- --port 50051                             # 0 SERVING
//! cargo run -- --port 50051 --service demo.Serving      # 0
//! cargo run -- --port 50051 --service demo.NotServing   # 3
//! cargo run -- --port 50051 --service demo.Unknown      # 3
//! cargo run -- --port 50051 --service demo.Missing      # 2 (rpc NOT_FOUND)
//! cargo run -- --port 50052                             # 1 (connection)
//!
//! cargo run -- --port 50051 --service demo.Flapping --watch   # live updates
//! ```
//!
//! Incoming metadata key names are logged, so `--metadata key=value` can be
//! seen reaching the server.

use std::time::Duration;

use tonic::metadata::KeyAndValueRef;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic::{Request, Status};
use tonic_health::ServingStatus;

/// Logs the names of the ascii metadata on each request so manual `--metadata`
/// checks are visible, then passes the request through unchanged. Values are
/// not logged: metadata often carries credentials, and this would set the
/// pattern for copied code.
fn log_metadata(request: Request<()>) -> Result<Request<()>, Status> {
    let keys: Vec<&str> = request
        .metadata()
        .iter()
        .filter_map(|entry| match entry {
            KeyAndValueRef::Ascii(key, _) => Some(key.as_str()),
            KeyAndValueRef::Binary(..) => None,
        })
        .collect();
    if !keys.is_empty() {
        println!("request metadata keys: {}", keys.join(", "));
    }
    Ok(request)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:50051".parse()?;

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", ServingStatus::Serving)
        .await;
    reporter
        .set_service_status("demo.Serving", ServingStatus::Serving)
        .await;
    reporter
        .set_service_status("demo.NotServing", ServingStatus::NotServing)
        .await;
    reporter
        .set_service_status("demo.Unknown", ServingStatus::Unknown)
        .await;
    reporter
        .set_service_status("demo.Flapping", ServingStatus::Serving)
        .await;

    // Flip demo.Flapping on a timer so a `--watch` stream keeps producing updates.
    tokio::spawn(async move {
        let mut serving = true;
        let mut ticker = tokio::time::interval(Duration::from_secs(3));
        ticker.tick().await; // the first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            serving = !serving;
            let status = if serving {
                ServingStatus::Serving
            } else {
                ServingStatus::NotServing
            };
            reporter.set_service_status("demo.Flapping", status).await;
        }
    });

    println!("mock_server listening on {addr}");
    println!("services (probe with `--service <name>`):");
    println!("  \"\"               -> SERVING (overall)");
    println!("  demo.Serving     -> SERVING");
    println!("  demo.NotServing  -> NOT_SERVING");
    println!("  demo.Unknown     -> UNKNOWN");
    println!("  demo.Missing     -> not registered (server returns NOT_FOUND)");
    println!("  demo.Flapping    -> toggles SERVING/NOT_SERVING every 3s (for --watch)");

    Server::builder()
        .add_service(InterceptedService::new(health_service, log_metadata))
        .serve(addr)
        .await?;

    Ok(())
}
