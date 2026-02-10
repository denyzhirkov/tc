use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod state;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("termicall=info".parse()?))
        .init();

    tracing::info!(
        "termicall-server starting on TCP:{} UDP:{}",
        termicall_shared::DEFAULT_TCP_PORT,
        termicall_shared::DEFAULT_UDP_PORT,
    );

    // TODO: start TCP + UDP listeners

    Ok(())
}
