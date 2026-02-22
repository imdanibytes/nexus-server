mod actions;
mod agent;
mod config;
mod github_auth;
mod mcp;
mod routing;
mod server;
mod sources;
mod verification;

use clap::Parser;
use tracing::{info, warn};

#[derive(Parser)]
#[command(name = "nexus-server", version, about = "Nexus automation server")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install rustls crypto provider before any TLS usage (ngrok, reqwest)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Load .env before anything else reads env vars
    if let Err(e) = dotenvy::dotenv() {
        // Not an error — .env is optional
        warn!("no .env file loaded: {e}");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nexus_server=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;

    let bind = config.server.bind.clone();
    let tunnel_config = config.tunnel.clone();

    info!(
        bind = %bind,
        sources = config.sources.len(),
        rules = config.rules.len(),
        "starting nexus-server"
    );

    let router = server::build_router(config, cli.config.into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let local_addr = listener.local_addr()?;
    info!("listening on {local_addr}");

    // Start ngrok tunnel in background if configured
    let use_tunnel = tunnel_config
        .as_ref()
        .is_some_and(|t| t.enabled);

    if use_tunnel {
        let forward_to = format!("http://{local_addr}");
        tokio::spawn(async move {
            if let Err(e) = start_tunnel(tunnel_config.as_ref().unwrap(), &forward_to).await {
                tracing::error!(error = %e, "ngrok tunnel failed");
            }
        });
    }

    axum::serve(listener, router).await?;
    Ok(())
}

async fn start_tunnel(
    config: &config::TunnelConfig,
    forward_to: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use ngrok::config::ForwarderBuilder;
    use ngrok::tunnel::EndpointInfo;

    let session = ngrok::Session::builder()
        .authtoken_from_env()
        .connect()
        .await?;

    let mut endpoint = session.http_endpoint();
    if let Some(domain) = config.domain.as_deref() {
        endpoint.domain(domain);
    }

    let tunnel: ngrok::forwarder::Forwarder<ngrok::tunnel::HttpTunnel> = endpoint
        .listen_and_forward(forward_to.parse()?)
        .await?;

    info!(url = %tunnel.url(), "ngrok tunnel established");

    // Block forever — tunnel stays alive as long as this future is alive
    futures::future::pending::<()>().await;
    Ok(())
}
