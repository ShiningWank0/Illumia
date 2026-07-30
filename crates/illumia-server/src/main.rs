use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("illumia_server=info,tower_http=info")),
        )
        .init();
    illumia_server::run(illumia_server::Config::from_env()?).await
}
