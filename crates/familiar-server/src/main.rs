//! Standalone familiar-server (Render, Docker, any host). Configured with env vars.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .or_else(|| k.strip_prefix("FAMILIAR_").and_then(|s| std::env::var(format!("ZED_{s}")).ok()))
            .filter(|v| !v.trim().is_empty())
    };
    let cfg = familiar_server::Config {
        database_url: env("DATABASE_URL")
            .ok_or_else(|| anyhow::anyhow!("DATABASE_URL is required"))?,
        host: [0, 0, 0, 0],
        port: env("PORT").and_then(|p| p.parse().ok()).unwrap_or(8080),
        secret_key: env("FAMILIAR_SECRET_KEY"),
        public_url: env("FAMILIAR_PUBLIC_URL"),
        web_origins: env("FAMILIAR_WEB_ORIGINS")
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        // Set when this API runs on the PC with the teammates' workspaces (next to `familiard`).
        bots_dir: env("FAMILIAR_BOTS_DIR").map(std::path::PathBuf::from),
    };
    familiar_server::serve(cfg, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
