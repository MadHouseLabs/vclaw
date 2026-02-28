mod config;
mod event;
mod tmux;
mod brain;
mod tts;
mod tui;
mod voice;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    println!("vclaw v{}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
