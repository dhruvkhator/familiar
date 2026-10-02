//! The built-in database: a private PostgreSQL the app installs and runs itself (no Docker, no setup).
//! Binaries are downloaded once into ~/.familiar/pg; data lives in ~/.familiar/pgdata.

// The embedded role and database keep their original name "zed" so existing data directories stay valid.
use anyhow::{Context, Result};
use postgresql_embedded::{PostgreSQL, SettingsBuilder};
use familiar_core::Config;

pub const PORT: u16 = 47432;

pub async fn start(cfg: &Config) -> Result<(PostgreSQL, String)> {
    let home = Config::home_dir();
    let password = cfg.embedded_db_password.clone().context("embedded_db_password missing from config")?;
    let settings = SettingsBuilder::new()
        .installation_dir(home.join("pg"))
        .data_dir(home.join("pgdata"))
        .password_file(home.join("pg").join(".pgpass"))
        .host("127.0.0.1")
        // Prefer the usual port; if something else holds it, any free one works (the URL is derived each start).
        .port(if std::net::TcpListener::bind(("127.0.0.1", PORT)).is_ok() { PORT } else { 0 })
        .username("zed")
        .password(password)
        .temporary(false)
        .build();
    let mut pg = PostgreSQL::new(settings);
    pg.setup().await.context("installing the built-in database")?;
    pg.start().await.context("starting the built-in database")?;
    if !pg.database_exists("zed").await? {
        pg.create_database("zed").await?;
    }
    let url = pg.settings().url("zed");
    Ok((pg, url))
}
