use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nudgepost=info".parse()?))
        .init();
    nudgepost::commands::run().await
}
