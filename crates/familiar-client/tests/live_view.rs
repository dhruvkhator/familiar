//! The live view end to end, as the desktop app sees it: a real familiar-server, the owner's engine in the same
//! process with a real Chrome (temporary profile) on a local test page, frames through `GET /live/stream`, input
//! through `POST /live/input` → `familiar_input` → CDP. Needs `TEST_DATABASE_URL` and Chrome or Edge:
//! `cargo test -p familiar-client --test live_view -- --ignored --nocapture`.

use familiar_client::*;
use familiar_server::{Config, serve_listener};
use futures_util::StreamExt;
use serde_json::Value;
use sqlx::{Connection, PgConnection};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const PAGE: &str = r#"<!doctype html><html><body style="margin:0;font:40px sans-serif">
<input id="t" style="font-size:40px;width:90%">
<div id="b" style="height:2400px;background:#fff" onclick="this.style.background=this.style.background=='rgb(255, 255, 255)'?'#000':'#fff'">click me</div>
</body></html>"#;
const ANIM: &str = r#"<!doctype html><html><body style="margin:0"><div id="n" style="font:120px sans-serif"></div>
<script>let i=0;(function f(){document.getElementById('n').textContent=++i;document.body.style.background='hsl('+(i%360)+',60%,60%)';requestAnimationFrame(f)})()</script>
</body></html>"#;

async fn serve_pages() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let body = if String::from_utf8_lossy(&buf[..n]).starts_with("GET /anim") { ANIM } else { PAGE };
                let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = s.write_all(format!("{head}{body}").as_bytes()).await;
            });
        }
    });
    port
}

fn stats(name: &str, mut ms: Vec<f64>) -> f64 {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| ms[((ms.len() as f64 - 1.0) * q).round() as usize];
    println!("{name}: n={} p50={:.0} ms p95={:.0} ms max={:.0} ms", ms.len(), p(0.5), p(0.95), ms[ms.len() - 1]);
    p(0.95)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "starts a real Chrome on a temporary profile"]
async fn live_view_end_to_end() {
    let Ok(admin) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: TEST_DATABASE_URL not set");
        return;
    };
    let db = format!("c_{}", Uuid::new_v4().simple());
    let mut c = PgConnection::connect(&admin).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {db}"))).execute(&mut c).await.unwrap();
    let mut u = url::Url::parse(&admin).unwrap();
    u.set_path(&format!("/{db}"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let cfg = Config { database_url: u.to_string(), host: [127, 0, 0, 1], port: 0, secret_key: None, public_url: None, web_origins: vec![], bots_dir: None };
    tokio::spawn(async move { serve_listener(listener, cfg, std::future::pending()).await });
    let client = Client::new(&base, None);
    tokio::time::timeout(Duration::from_secs(60), async {
        while client.auth_state().await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("server up");
    client.setup("owner@example.com", "correct horse battery").await.unwrap();
    let owner = client.me().await.unwrap().id;
    let bot = client.create_bot(&NewBot { name: Some("Bench".into()), ..Default::default() }).await.unwrap();

    // The owner's engine, in this process like in the desktop app: its browser and the take-over listener.
    let pool = sqlx::PgPool::connect(u.as_str()).await.unwrap();
    let core_db = familiar_core::db::Db { pool: pool.clone(), owner };
    let manager = Arc::new(familiar_core::browser::Manager::default());
    let profiles = std::env::temp_dir().join(format!("familiar-e2e-{}", Uuid::new_v4()));
    let _local = familiar_core::browser::register(core_db.clone(), manager.clone(), profiles.clone(), None);
    if manager.ensure(bot.id, &bot.slug, profiles.clone(), None).await.is_none() {
        eprintln!("SKIP: no Chrome or Edge");
        return;
    }
    let mut l = sqlx::postgres::PgListener::connect_with(&pool).await.unwrap();
    l.listen("familiar_input").await.unwrap();
    let (m, d) = (manager.clone(), core_db.clone());
    tokio::spawn(async move {
        while let Ok(n) = l.recv().await {
            let ev: Value = serde_json::from_str(n.payload()).unwrap();
            if let Err(e) = m.input(&d, bot.id, &ev).await {
                eprintln!("input: {e:#}");
            }
        }
    });

    let port = serve_pages().await;
    client.live_input(bot.id, &LiveInput::navigate(format!("http://127.0.0.1:{port}/anim"))).await.unwrap();

    // Frames as the app gets them: (when it arrived, frame).
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<(Instant, LiveFrame)>>(None);
    let frames = client.live_stream(bot.id, 1280, 800).await.unwrap();
    tokio::spawn(async move {
        futures_util::pin_mut!(frames);
        while let Some(Ok(f)) = frames.next().await {
            tx.send_replace(Some((Instant::now(), f)));
        }
    });
    let next = async |rx: &mut tokio::sync::watch::Receiver<Option<(Instant, LiveFrame)>>, within: Duration| {
        tokio::time::timeout(within, async {
            rx.changed().await.ok()?;
            rx.borrow_and_update().clone()
        })
        .await
        .ok()
        .flatten()
    };
    let settle = async |rx: &mut tokio::sync::watch::Receiver<Option<(Instant, LiveFrame)>>| {
        while next(rx, Duration::from_millis(400)).await.is_some() {}
    };

    // Watching a page that changes every frame.
    let (_, first) = next(&mut rx, Duration::from_secs(10)).await.expect("a first frame");
    let t0 = Instant::now();
    let (mut n, mut last) = (0, first.seq);
    while t0.elapsed() < Duration::from_secs(3) {
        if let Some((_, f)) = next(&mut rx, Duration::from_secs(1)).await {
            n += 1;
            last = f.seq;
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let fps = n as f64 / secs;
    println!(
        "app receives, animated page: {fps:.1} fps ({:.1} fps made by the engine, {} KB a frame)",
        (last - first.seq) as f64 / secs,
        first.jpeg.len() / 1024
    );
    assert!(fps >= 20.0);

    // Input → the changed picture reaches the app.
    client.live_input(bot.id, &LiveInput::navigate(format!("http://127.0.0.1:{port}/"))).await.unwrap();
    next(&mut rx, Duration::from_secs(5)).await.expect("the page shows");
    let (mut click, mut key) = (Vec::new(), Vec::new());
    for i in 0..40 {
        if i == 20 {
            client.live_input(bot.id, &LiveInput::click(100, 25)).await.unwrap();
        }
        settle(&mut rx).await;
        let t = Instant::now();
        let input = if i < 20 { LiveInput::click(600, 500) } else { LiveInput::type_text("a") };
        client.live_input(bot.id, &input).await.unwrap();
        let (at, _) = next(&mut rx, Duration::from_secs(3)).await.expect("the change shows");
        let ms = at.duration_since(t).as_secs_f64() * 1000.0;
        if i < 20 { click.push(ms) } else { key.push(ms) }
    }
    let p95c = stats("app: click → picture received (POST, NOTIFY, CDP, screencast, stream)", click);
    let p95k = stats("app: key → picture received", key);
    assert!(p95c < 150.0 && p95k < 150.0);

    manager.shutdown().await;
    let _ = std::fs::remove_dir_all(&profiles);
}
