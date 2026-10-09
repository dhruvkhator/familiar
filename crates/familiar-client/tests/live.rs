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
    let cfg = Config { database_url: u.to_string(), host: [127, 0, 0, 1], port: 0, secret_key: None, public_url: None, web_origins: vec![], bots_dir: None };
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

/// The CRM methods against a real server: create (with dedupe), patch, the timeline, the board, the change log, undo,
/// delete, and the CSV round trip.
#[tokio::test]
async fn crm_roundtrip() {
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
    let (stop, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = serve_listener(listener, cfg, async {
            let _ = rx.await;
        })
        .await;
    });
    let client = Client::new(&base, None);
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if client.auth_state().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("server up");
    client.setup("owner@example.com", "correct horse battery").await.unwrap();

    let acme = client
        .create_crm_company(&NewCompany { name: Some("Acme".into()), website: Some("https://www.acme.com/x".into()), tags: Some(vec!["icp".into()]), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(acme.domain.as_deref(), Some("acme.com"));
    // the same domain again: the same company
    let again = client.create_crm_company(&NewCompany { name: Some("Acme Inc".into()), domain: Some("acme.com".into()), ..Default::default() }).await.unwrap();
    assert_eq!((again.id, again.name.as_str()), (acme.id, "Acme Inc"));
    assert_eq!(client.crm_companies(&CrmListParams::default()).await.unwrap().len(), 1);
    assert_eq!(client.crm_companies(&CrmListParams { q: Some("acme".into()), tag: Some("ICP".into()), ..Default::default() }).await.unwrap().len(), 1);
    let bad = client.create_crm_company(&NewCompany { name: Some("x".into()), domain: Some("not a domain".into()), ..Default::default() }).await;
    assert!(matches!(bad, Err(ApiError::Http { status: 400, .. })), "{bad:?}");

    let sam = client
        .create_crm_contact(&NewContact { name: Some("Sam".into()), email: Some("Sam@Acme.com".into()), company_id: Some(acme.id), ..Default::default() })
        .await
        .unwrap();
    assert_eq!((sam.email.as_deref(), sam.company_name.as_deref()), (Some("sam@acme.com"), Some("Acme Inc")));
    let sam = client.update_crm_contact(sam.id, &ContactPatch { do_not_contact: Some(true), dnc_reason: Some("asked".into()), ..Default::default() }).await.unwrap();
    assert!(sam.do_not_contact && sam.dnc_at.is_some());
    assert_eq!(client.crm_contacts(&CrmListParams { dnc: Some(true), ..Default::default() }).await.unwrap().len(), 1);
    assert_eq!(client.crm_contact(sam.id).await.unwrap().name, "Sam");

    let deal = client
        .create_crm_deal(&NewDeal { company_id: Some(acme.id), contact_id: Some(sam.id), title: Some("Pilot".into()), value_cents: Some(5000), ..Default::default() })
        .await
        .unwrap();
    assert_eq!((deal.stage, deal.contact_name.as_deref()), (DealStage::New, Some("Sam")));
    let moved = client.update_crm_deal(deal.id, &DealPatch { stage: Some("meeting".into()), ..Default::default() }).await.unwrap();
    assert_eq!(moved.stage, DealStage::Meeting);
    let board = client.crm_pipeline().await.unwrap();
    assert_eq!(board.len(), 8);
    let meeting = board.iter().find(|s| s.stage == DealStage::Meeting).unwrap();
    assert_eq!((meeting.count, meeting.value_cents, meeting.deals[0].title.as_str()), (1, 5000, "Pilot"));
    assert_eq!(client.crm_deals(&CrmListParams { stage: Some("meeting".into()), ..Default::default() }).await.unwrap().len(), 1);

    let note = client
        .log_crm_activity(&NewActivity { deal_id: Some(deal.id), kind: Some("call".into()), summary: Some("Intro call".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!((note.kind, note.company_id, note.actor_kind.as_str()), (ActivityKind::Call, Some(acme.id), "user"));
    assert_eq!(client.crm_activities(&CrmActivityParams { company_id: Some(acme.id), ..Default::default() }).await.unwrap().len(), 2, "the call and the stage_change of the move to meeting");

    // the change log, and undo (only the newest change of a record)
    let changes = client.crm_changes(&CrmChangeParams { entity: Some("deal".into()), entity_id: Some(deal.id), ..Default::default() }).await.unwrap();
    assert_eq!((changes.len(), changes[0].op.as_str(), changes[1].op.as_str()), (2, "update", "create"));
    assert!(matches!(client.undo_crm_change(changes[1].id).await, Err(ApiError::Http { status: 409, .. })));
    let undo = client.undo_crm_change(changes[0].id).await.unwrap();
    assert_eq!(undo.op, "undo");
    assert_eq!(client.crm_deal(deal.id).await.unwrap().stage, DealStage::New);

    // CSV: an export imports as a dry run that creates nothing
    let csv = client.crm_export_csv("contacts").await.unwrap();
    assert!(csv.starts_with("name,title,email,"), "{csv}");
    assert!(csv.contains("sam@acme.com"));
    let dry = client.crm_import("contacts", &csv, true).await.unwrap();
    assert_eq!((dry.created, dry.updated, dry.skipped, dry.errors.len()), (0, 0, 1, 0));
    let new = client.crm_import("companies", "name,domain\r\nBeta,beta.io\r\nBad,not a domain\r\n", false).await.unwrap();
    assert_eq!((new.created, new.errors.len(), new.errors[0].row), (1, 1, 3));

    // delete is soft: the lists lose it, undo brings it back
    client.delete_crm_company(acme.id).await.unwrap();
    assert!(matches!(client.crm_company(acme.id).await, Err(ApiError::Http { status: 404, .. })));
    let last = client.crm_changes(&CrmChangeParams { entity_id: Some(acme.id), limit: Some(1), ..Default::default() }).await.unwrap();
    assert_eq!(last[0].op, "delete");
    client.undo_crm_change(last[0].id).await.unwrap();
    assert_eq!(client.crm_company(acme.id).await.unwrap().name, "Acme Inc");
    client.delete_crm_deal(deal.id).await.unwrap();
    client.delete_crm_contact(sam.id).await.unwrap();
    assert!(client.crm_contacts(&CrmListParams::default()).await.unwrap().is_empty());

    let _ = stop.send(());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {db} with (force)"))).execute(&mut c).await;
}

/// Webhook settings and the GTM crew through the client: the secret comes back once, a Test to a closed port fails and
/// is listed, a bundle hires its whole crew.
#[tokio::test]
async fn crm_webhooks_and_bundles() {
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
    let key = Some("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".to_string());
    let cfg = Config { database_url: u.to_string(), host: [127, 0, 0, 1], port: 0, secret_key: key, public_url: None, web_origins: vec![], bots_dir: None };
    let (stop, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = serve_listener(listener, cfg, async {
            let _ = rx.await;
        })
        .await;
    });
    let client = Client::new(&base, None);
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if client.auth_state().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("server up");
    client.setup("owner@example.com", "correct horse battery").await.unwrap();

    // a port nothing listens on, on this computer
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/hook", closed.local_addr().unwrap().port());
    drop(closed);
    let w = client
        .create_crm_webhook(&NewCrmWebhook { url: Some(url.clone()), events: Some(vec!["deal.stage_changed".into()]), ..Default::default() })
        .await
        .unwrap();
    assert!(w.secret.as_deref().is_some_and(|s| s.starts_with("whsec_")) && w.enabled);
    let listed = client.crm_webhooks().await.unwrap();
    assert_eq!((listed.len(), listed[0].secret.as_deref()), (1, None));
    let bad = client.create_crm_webhook(&NewCrmWebhook { url: Some("http://8.8.8.8/x".into()), events: Some(vec!["deal.created".into()]), ..Default::default() }).await;
    assert!(matches!(bad, Err(ApiError::Http { status: 400, .. })), "{bad:?}");

    let d = client.test_crm_webhook(w.id).await.unwrap();
    assert_eq!((d.event.as_str(), d.status.as_str(), d.attempts), ("ping", "failed", 1));
    assert!(d.last_error.is_some());
    let all = client.crm_webhook_deliveries(w.id, Some(10)).await.unwrap();
    assert_eq!((all.len(), all[0].id), (1, d.id));
    let off = client.update_crm_webhook(w.id, &CrmWebhookPatch { enabled: Some(false), ..Default::default() }).await.unwrap();
    assert!(!off.enabled && off.secret.is_none());
    client.delete_crm_webhook(w.id).await.unwrap();
    assert!(client.crm_webhooks().await.unwrap().is_empty());

    let bundles = client.template_bundles().await.unwrap();
    let gtm = bundles.iter().find(|b| b.id == "gtm-crew").unwrap();
    assert!(gtm.questions.iter().any(|q| q.key == "follow_up_days"));
    let answers = [("product".to_owned(), "Familiar".to_owned())].into();
    let hired = client.create_bundle("gtm-crew", &FromBundle { answers: Some(answers) }).await.unwrap();
    assert_eq!(hired.hired.len(), gtm.templates.len());
    assert!(hired.hired.iter().all(|h| h.bot.persona.as_deref().is_some_and(|p| !p.contains("{{"))));

    let _ = stop.send(());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {db} with (force)"))).execute(&mut c).await;
}
