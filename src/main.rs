mod actions;
mod cloud_event;
mod config;
mod routing;
mod server;
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
    info!(
        bind = %bind,
        webhooks = config.webhooks.len(),
        rules = config.rules.len(),
        "starting nexus-server"
    );

    let router = server::build_router(config);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    info!("listening on {bind}");

    axum::serve(listener, router).await?;
    Ok(())
}
