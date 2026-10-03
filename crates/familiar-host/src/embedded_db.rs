//! The built-in database: a private PostgreSQL the app installs and runs itself (no Docker, no setup).
//! Binaries are downloaded once into ~/.familiar/pg; data lives in ~/.familiar/pgdata.

// The embedded role and database keep their original name "zed" so existing data directories stay valid.
use anyhow::{Context, Result};
use postgresql_embedded::{BOOTSTRAP_DATABASE, BOOTSTRAP_SUPERUSER, PostgreSQL, SettingsBuilder};
use sqlx::Connection as _;
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
        .port(if port_free(PORT) { PORT } else { free_loopback_port()? })
        .username("zed")
        .password(password.clone())
        .temporary(false)
        .build();
    let mut pg = PostgreSQL::new(settings);
    pg.setup().await.context("installing the built-in database")?;
    pg.start().await.context("starting the built-in database")?;
    ensure_role(&pg, &password).await.context("creating the built-in database's role")?;
    if !pg.database_exists("zed").await? {
        pg.create_database("zed").await?;
    }
    let url = pg.settings().url("zed");
    Ok((pg, url))
}

/// postgresql_embedded's initdb makes only its bootstrap superuser (`postgres`); the app connects as `zed`, so a fresh
/// data directory needs that role (directories from before had it from initdb, so this is a no-op there).
async fn ensure_role(pg: &PostgreSQL, password: &str) -> Result<()> {
    let mut bootstrap = pg.settings().clone();
    bootstrap.username = BOOTSTRAP_SUPERUSER.into();
    let mut conn = match sqlx::PgConnection::connect(&bootstrap.url(BOOTSTRAP_DATABASE)).await {
        Ok(c) => c,
        // A data directory from before has no bootstrap superuser, only `zed`.
        Err(_) => return Ok(()),
    };
    let exists: bool = sqlx::query_scalar("select exists(select 1 from pg_roles where rolname = 'zed')")
        .fetch_one(&mut conn)
        .await?;
    if !exists {
        let sql = format!("create role zed login superuser password '{}'", password.replace('\'', "''"));
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(&mut conn).await?;
    }
    conn.close().await?;
    Ok(())
}

/// A free port on 127.0.0.1. (postgresql_embedded's own pick binds 0.0.0.0, which raises a Windows Firewall prompt.)
fn free_loopback_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Nothing listens on `port`. Binding 127.0.0.1 alone is not enough: on Windows it succeeds while another process
/// (e.g. a Docker-published Postgres) holds the wildcard address, and Postgres then fails to bind "localhost".
fn port_free(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpListener::bind(addr).is_ok()
        && std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_err()
}
