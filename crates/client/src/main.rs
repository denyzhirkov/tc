use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("termicall=info".parse()?))
        .init();

    tracing::info!("termicall-client starting");

    // TODO: parse args, connect, start TUI

    Ok(())
}
