#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "c67_server=info".into()),
        )
        .init();
    c67_server::run(c67_server::ServerConfig::from_env(), None).await
}
