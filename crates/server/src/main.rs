use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

mod rate_limit;
mod state;
mod tcp;
mod tls;
mod udp;

use state::ServerState;
use tc_shared::config;
use tc_shared::ServerMessage;

#[derive(Parser)]
#[command(name = "tc-server", about = "tc voice chat server")]
pub struct Args {
    /// Bind address
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// TCP control port
    #[arg(long, default_value_t = config::TCP_PORT)]
    tcp_port: u16,

    /// UDP voice port
    #[arg(long, default_value_t = config::UDP_PORT)]
    udp_port: u16,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Max simultaneous channels (0 = unlimited)
    #[arg(long, default_value_t = 0)]
    max_channels: usize,

    /// Max simultaneous clients (0 = unlimited)
    #[arg(long, default_value_t = 0)]
    max_clients: usize,

    /// Maintenance cleanup interval in seconds
    #[arg(long, default_value_t = config::MAINTENANCE_INTERVAL_SECS)]
    maintenance_interval: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let args = Args::parse();

    let directive = format!("tc={}", args.log_level);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(directive.parse()?))
        .init();

    let tcp_addr = format!("{}:{}", args.host, args.tcp_port);
    let udp_addr = format!("{}:{}", args.host, args.udp_port);

    tracing::info!("tc-server starting on TCP:{} UDP:{}", tcp_addr, udp_addr);

    let tls_config = tls::load_or_generate_tls_config()?;
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

    let limits = state::Limits {
        max_channels: args.max_channels,
        max_clients: args.max_clients,
    };
    let state = ServerState::new(limits);
    let senders: tcp::ClientSenders = Arc::new(RwLock::new(HashMap::new()));

    let shutdown_senders = senders.clone();

    // Spawn server tasks
    let tcp_handle = tokio::spawn(tcp::run_tcp_server(
        state.clone(),
        senders,
        tcp_addr,
        acceptor,
    ));
    let udp_handle = tokio::spawn(udp::run_udp_relay(state.clone(), udp_addr));
    let maint_handle = tokio::spawn(run_maintenance(state, args.maintenance_interval));

    // Wait for shutdown signal (SIGINT or SIGTERM)
    shutdown_signal().await;

    tracing::info!("shutdown signal received, notifying clients...");

    // Broadcast shutdown message to all connected clients
    {
        let senders = shutdown_senders.read().await;
        let msg = ServerMessage::Error {
            message: "server shutting down".into(),
        };
        for (addr, tx) in senders.iter() {
            if tx.try_send(msg.clone()).is_err() {
                tracing::trace!(%addr, "could not send shutdown notice");
            }
        }
    }

    // Give clients a moment to receive the message
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Abort server tasks
    tcp_handle.abort();
    udp_handle.abort();
    maint_handle.abort();

    tracing::info!("server stopped");
    Ok(())
}

/// Wait for SIGINT (Ctrl+C) or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

async fn run_maintenance(state: ServerState, interval_secs: u64) -> Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        let removed = state.cleanup_empty_channels().await;
        if removed > 0 {
            tracing::info!("cleaned up {} empty channels", removed);
        }
    }
}
