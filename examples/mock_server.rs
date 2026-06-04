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

use std::time::Duration;

use tonic::transport::Server;
use tonic_health::ServingStatus;

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
        .add_service(health_service)
        .serve(addr)
        .await?;

    Ok(())
}
