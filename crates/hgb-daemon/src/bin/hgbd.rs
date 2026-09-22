use hgb_daemon::HagibisDaemon;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = std::env::var("HGB_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(runtime_dir).join("hgb.sock")
        });

    println!("⚡ Starting Hagibis Daemon (hgbd)...");
    let daemon = HagibisDaemon::new(socket_path);
    daemon.run().await?;
    Ok(())
}
