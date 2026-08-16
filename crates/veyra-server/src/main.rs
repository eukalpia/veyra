#![forbid(unsafe_code)]

use std::error::Error;

use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use veyra_server::{bootstrap_runtime, parse_bind_address, serve};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .try_init()?;

    let bind = std::env::var("VEYRA_BIND").ok();
    let listener = TcpListener::bind(parse_bind_address(bind.as_deref())?).await?;
    let local_address = listener.local_addr()?;

    info!(%local_address, "Veyra administrative server listening");
    serve(listener, bootstrap_runtime(), shutdown_signal()).await?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "failed to install shutdown signal handler");
    }
}
