//! End-to-end against a real in-process familiar-server. Needs `TEST_DATABASE_URL`
//! (a Postgres the role can CREATE DATABASE on); skipped with a message otherwise.

use familiar_client::*;
use familiar_server::{Config, serve_listener};
use futures_util::StreamExt;
use sqlx::{Connection, PgConnection};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::test]
async fn setup_bot_message_notice() {
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
    let cfg = Config { database_url: u.to_string(), host: [127, 0, 0, 1], port: 0, secret_key: None, public_url: None, web_origins: vec![] };
    let (stop, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = serve_listener(listener, cfg, async {
            let _ = rx.await;
        })
        .await;
    });

    let client = Client::new(&base, None);
    let state = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(s) = client.auth_state().await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("server up");
    assert!(state.setup_needed);

    // unauthenticated call -> Unauthorized
    assert!(matches!(client.overview().await, Err(ApiError::Unauthorized)));
    let sess = client.setup("owner@example.com", "correct horse battery").await.unwrap();
    assert_eq!(client.token().as_deref(), Some(sess.token.as_str()));
    assert_eq!(client.me().await.unwrap().email, "owner@example.com");
    assert!(!client.auth_state().await.unwrap().setup_needed);

    let mut stream = Box::pin(client.stream());
    // the stream connects lazily; poll it in the background so the subscription exists before we write
    let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(ev) = stream.next().await {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(800)).await;

    let bot = client.create_bot(&NewBot { name: Some("Tester".into()), ..Default::default() }).await.unwrap();
    assert_eq!(bot.name, "Tester");
    let thread = client.create_thread(bot.id, None).await.unwrap();
    let msg = client.post_message(thread.id, "hello there").await.unwrap();
    assert_eq!(msg.role, Role::User);

    let mut saw_message = false;
    let mut saw_run = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(saw_message && saw_run) {
        let ev = tokio::time::timeout_at(deadline, notices.recv()).await.expect("notice before deadline").expect("stream open");
        if let LiveEvent::Notice(n) = ev {
            saw_message |= n.t == "messages";
            saw_run |= n.t == "runs";
        }
    }

    let msgs = client.messages(thread.id, None, Some(50)).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(client.bot_runs(bot.id, Some(5), None).await.unwrap().len(), 1);
    let ov = client.overview().await.unwrap();
    assert_eq!(ov.bots.len(), 1);

    // SWR: second read is served from cache first
    let mut hit = false;
    client.cached_get::<Overview>("/api/overview", |_| hit = true).await.unwrap();
    assert!(hit);

    // templates: hire one, its schedules start off, tick a sign-in on its Set up checklist
    let templates = client.templates().await.unwrap();
    assert!(templates.len() >= 9);
    let t = templates.iter().find(|t| t.id == "social-media-manager").expect("in the catalog");
    assert!(!t.questions.is_empty() && t.avatar.is_some());
    let answers = [("product".to_owned(), "Familiar".to_owned())].into_iter().collect();
    let hired = client
        .create_from_template(&t.id, &FromTemplate { answers: Some(answers), name: Some("Poster".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(hired.bot.name, "Poster");
    assert!(hired.bot.persona.as_deref().unwrap_or_default().contains("social media for Familiar"));
    assert!(hired.first_task.is_some());
    let setup = hired.bot.setup.clone().expect("template setup");
    assert_eq!((setup.logins.len(), setup.schedules.len()), (t.logins.len(), t.schedules.len()));
    assert!(client.schedules(hired.bot.id).await.unwrap().iter().all(|s| !s.enabled && setup.schedules.contains(&s.id)));
    let site = setup.logins[0].site.clone();
    let bot = client.update_bot_setup(hired.bot.id, &SetupPatch { login: Some(site), ..Default::default() }).await.unwrap();
    assert!(bot.setup.unwrap().logins[0].done);

    client.logout().await.unwrap();
    assert!(client.token().is_none());

    let _ = stop.send(());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {db} with (force)"))).execute(&mut c).await;
}
