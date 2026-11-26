use std::net::SocketAddr;

use clap::Parser;
use hoops_server::{build_router, build_state, ServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hoops_server=info,tower_http=info".into()),
        )
        .init();

    let cfg = ServerConfig::parse();
    let addr: SocketAddr = cfg.bind.parse()?;
    let state = build_state(cfg.clone()).await?;
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("hoops-server listening on http://{} (viewer served at /)", addr);
    axum::serve(listener, app).await?;
    Ok(())
}
