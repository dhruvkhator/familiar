//! End-to-end API tests: a real server on 127.0.0.1:0 over a throwaway database.
//! Needs `TEST_DATABASE_URL` (any Postgres >= 13 the role can CREATE DATABASE on); skipped with a message otherwise.

use familiar_server::{Config, serve_listener};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgListener};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

const PW: &str = "correct horse battery";

/// A folder for this test run outside the home folder (temp folders live in app data, which is never shared).
fn test_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("api-{}", std::process::id())).join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}
const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

struct App {
    base: String,
    http: reqwest::Client,
    pool: PgPool,
    admin: String,
    db: String,
    db_url: String,
    stop: Option<oneshot::Sender<()>>,
}

struct User {
    tok: String,
    id: Uuid,
}

async fn start(secret: bool) -> Option<App> {
    let Ok(admin) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: TEST_DATABASE_URL not set");
        return None;
    };
    // As in the desktop app: the webhook tests receive on this computer.
    familiar_core::crm::webhooks::set_allow_loopback(true);
    let db = format!("t_{}", Uuid::new_v4().simple());
    let mut c = PgConnection::connect(&admin).await.expect("connect admin db");
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {db}")))
        .execute(&mut c)
        .await
        .expect("create database");
    let mut u = url::Url::parse(&admin).unwrap();
    u.set_path(&format!("/{db}"));
    let db_url = u.to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cfg = Config {
        database_url: db_url.clone(),
        host: [127, 0, 0, 1],
        port: 0,
        secret_key: secret.then(|| KEY.to_string()),
        public_url: None,
        web_origins: vec![],
        bots_dir: Some(test_dir("bots")),
    };
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = serve_listener(listener, cfg, async {
            let _ = rx.await;
        })
        .await;
    });
    let pool = PgPool::connect(&db_url).await.unwrap();
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    // the server owns the migrations: wait until they ran before tests touch tables directly
    let up = async {
        loop {
            if http.get(format!("{base}/healthz")).send().await.is_ok_and(|r| r.status() == 200)
                && sqlx::query("select 1 from public.users limit 1").execute(&pool).await.is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), up).await.expect("server did not come up");
    Some(App { base, http, pool, admin, db, db_url, stop: Some(tx) })
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let (admin, db) = (self.admin.clone(), self.db.clone());
        // best-effort cleanup on a private runtime, so it also runs when a test panics
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async {
                if let Ok(mut c) = PgConnection::connect(&admin).await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {db} with (force)")))
                        .execute(&mut c)
                        .await;
                }
            });
        })
        .join();
    }
}

macro_rules! app {
    () => {
        app!(false)
    };
    ($secret:expr) => {
        match start($secret).await {
            Some(a) => a,
            None => return,
        }
    };
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap_or_else(|| panic!("no id in {v}")).to_string()
}
fn uid(v: &Value) -> Uuid {
    id(v).parse().unwrap()
}
fn len(v: &Value) -> usize {
    v.as_array().unwrap_or_else(|| panic!("not an array: {v}")).len()
}

impl App {
    async fn call(&self, m: Method, path: &str, tok: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let mut r = self.http.request(m, format!("{}{path}", self.base));
        if let Some(t) = tok {
            r = r.bearer_auth(t);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let st = resp.status().as_u16();
        let bytes = resp.bytes().await.unwrap();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
    async fn get(&self, t: &str, p: &str) -> (u16, Value) {
        self.call(Method::GET, p, Some(t), None).await
    }
    async fn post(&self, t: &str, p: &str, b: Value) -> (u16, Value) {
        self.call(Method::POST, p, Some(t), Some(b)).await
    }
    async fn patch(&self, t: &str, p: &str, b: Value) -> (u16, Value) {
        self.call(Method::PATCH, p, Some(t), Some(b)).await
    }
    async fn put(&self, t: &str, p: &str, b: Value) -> (u16, Value) {
        self.call(Method::PUT, p, Some(t), Some(b)).await
    }
    async fn del(&self, t: &str, p: &str) -> (u16, Value) {
        self.call(Method::DELETE, p, Some(t), None).await
    }
    /// POST that must return `want`; gives back the body.
    async fn ok_post(&self, t: &str, p: &str, b: Value, want: u16) -> Value {
        let (s, v) = self.post(t, p, b).await;
        assert_eq!(s, want, "POST {p} -> {v}");
        v
    }
    async fn hook(&self, token: &str, body: Vec<u8>) -> reqwest::Response {
        self.http.post(format!("{}/hooks/{token}", self.base)).body(body).send().await.unwrap()
    }

    async fn owner(&self) -> User {
        let v = self.ok_post("", "/api/auth/setup", json!({"email": "a@example.com", "password": PW}), 200).await;
        User { tok: v["token"].as_str().unwrap().into(), id: uid(&v["user"]) }
    }
    /// A second user inserted via SQL (re-using the owner's password hash: argon2 is slow in debug builds).
    async fn second(&self) -> User {
        let id: Uuid = sqlx::query_scalar(
            "insert into users (email, password_hash) select 'b@example.com', password_hash from users limit 1 returning id",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let tok = "second-user-token";
        sqlx::query("insert into sessions (token_hash, user_id, expires_at) values (encode(digest($1,'sha256'),'hex'), $2, now() + interval '1 day')")
            .bind(tok)
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        User { tok: tok.into(), id }
    }
    async fn bot(&self, t: &str, name: &str) -> String {
        id(&self.ok_post(t, "/api/bots", json!({"name": name}), 201).await)
    }
    /// Start a thread and post a user message; returns (thread_id, the queued run).
    async fn chat(&self, t: &str, bot: &str, text: &str) -> (String, Value) {
        let th = id(&self.ok_post(t, &format!("/api/bots/{bot}/threads"), json!({}), 201).await);
        self.ok_post(t, &format!("/api/threads/{th}/messages"), json!({"content": text}), 201).await;
        let (s, runs) = self.get(t, &format!("/api/threads/{th}/runs")).await;
        assert_eq!(s, 200);
        (th, runs[0].clone())
    }
    async fn exec(&self, q: &str, bind: &[Uuid]) {
        let mut query = sqlx::query(sqlx::AssertSqlSafe(q.to_string()));
        for b in bind {
            query = query.bind(*b);
        }
        query.execute(&self.pool).await.unwrap_or_else(|e| panic!("{q}: {e}"));
    }
    async fn approval(&self, owner: Uuid, run: Uuid, bot: Uuid, tool: &str) -> Uuid {
        sqlx::query_scalar("insert into approvals (owner_id, run_id, bot_id, tool_name, input) values ($1,$2,$3,$4,'{}') returning id")
            .bind(owner)
            .bind(run)
            .bind(bot)
            .bind(tool)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

// ------------------------------------------------------------------ auth

#[tokio::test(flavor = "multi_thread")]
async fn auth_flow() {
    let app = app!();
    let (s, v) = app.call(Method::GET, "/api/auth/state", None, None).await;
    assert_eq!((s, &v["setup_needed"]), (200, &json!(true)));
    assert_eq!(app.http.get(format!("{}/healthz", app.base)).send().await.unwrap().text().await.unwrap(), "ok");

    for bad in [json!({"email": "nope", "password": PW}), json!({"email": "a@example.com", "password": "short"})] {
        assert_eq!(app.post("", "/api/auth/setup", bad).await.0, 400);
    }
    let a = app.owner().await;
    let (_, v) = app.call(Method::GET, "/api/auth/state", None, None).await;
    assert_eq!(v["setup_needed"], false);
    assert_eq!(app.post("", "/api/auth/setup", json!({"email": "c@example.com", "password": PW})).await.0, 409);

    let (s, me) = app.get(&a.tok, "/api/me").await;
    assert_eq!((s, me["email"].as_str()), (200, Some("a@example.com")));

    assert_eq!(app.call(Method::GET, "/api/me", None, None).await.0, 401);
    assert_eq!(app.get("garbage", "/api/me").await.0, 401);
    assert_eq!(app.get(&a.tok, "/api/nope").await.0, 404);
    // the query token is only honoured on stream / download / live.jpg
    assert_eq!(app.call(Method::GET, &format!("/api/me?token={}", a.tok), None, None).await.0, 401);

    // login: wrong password / unknown email (2 failures: still under the slowdown threshold), then the right one
    assert_eq!(app.post("", "/api/auth/login", json!({"email": "a@example.com", "password": "wrong password!"})).await.0, 401);
    assert_eq!(app.post("", "/api/auth/login", json!({"email": "ghost@example.com", "password": PW})).await.0, 401);
    let v = app.ok_post("", "/api/auth/login", json!({"email": "A@Example.com", "password": PW}), 200).await;
    let t2 = v["token"].as_str().unwrap().to_string();
    assert_eq!(app.get(&t2, "/api/me").await.0, 200);

    // password change: needs the current password, kills the *other* sessions, keeps this one
    let newpw = "a brand new password";
    assert_eq!(app.patch(&a.tok, "/api/auth/account", json!({"current_password": PW, "new_password": "short"})).await.0, 400);
    assert_eq!(app.patch(&a.tok, "/api/auth/account", json!({"current_password": "wrong password!", "new_password": newpw})).await.0, 401);
    let (s, v) = app.patch(&a.tok, "/api/auth/account", json!({"current_password": PW, "new_password": newpw})).await;
    assert_eq!((s, v["email"].as_str()), (200, Some("a@example.com")));
    assert_eq!(app.get(&t2, "/api/me").await.0, 401, "other session must die");
    assert_eq!(app.get(&a.tok, "/api/me").await.0, 200, "current session survives");
    let v = app.ok_post("", "/api/auth/login", json!({"email": "a@example.com", "password": newpw}), 200).await;
    let t3 = v["token"].as_str().unwrap().to_string();

    // logout kills exactly that token
    assert_eq!(app.post(&t3, "/api/auth/logout", json!({})).await.0, 204);
    assert_eq!(app.get(&t3, "/api/me").await.0, 401);
    assert_eq!(app.get(&a.tok, "/api/me").await.0, 200);
}

// ------------------------------------------------------------------ bots

#[tokio::test(flavor = "multi_thread")]
async fn bots_crud_and_validation() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;

    let b = app.ok_post(t, "/api/bots", json!({"name": "  Research Buddy!  "}), 201).await;
    assert_eq!(b["slug"], "research-buddy");
    assert_eq!((b["model"].as_str(), b["engine"].as_str(), b["paused"].clone()), (Some("sonnet"), Some("claude"), json!(false)));
    assert!(b.get("owner_id").is_none());
    assert_eq!(b["status"], "idle");
    let bid = id(&b);

    for bad in ["Bad", "-x", "a_b", "has space", &"x".repeat(41)] {
        assert_eq!(app.post(t, "/api/bots", json!({"name": "n", "slug": bad})).await.0, 400, "slug {bad}");
    }
    app.ok_post(t, "/api/bots", json!({"name": "n", "slug": "ok-1"}), 201).await;
    assert_eq!(app.post(t, "/api/bots", json!({"name": "dup", "slug": "ok-1"})).await.0, 409);
    assert_eq!(app.post(t, "/api/bots", json!({"name": "Research Buddy"})).await.0, 409, "derived slug collides too");
    assert_eq!(app.post(t, "/api/bots", json!({"name": "   "})).await.0, 400);
    assert_eq!(app.post(t, "/api/bots", json!({})).await.0, 400);
    assert_eq!(app.post(t, "/api/bots", json!({"name": "p", "persona": "x".repeat(20_001)})).await.0, 400);

    // engine/model validation
    assert_eq!(app.post(t, "/api/bots", json!({"name": "m", "model": "gpt-9"})).await.0, 400);
    for bad in ["claude-", "claude-x", "claude-Opus-5", "claude-opus 5", &format!("claude-{}", "a".repeat(61))] {
        assert_eq!(app.post(t, "/api/bots", json!({"name": "m", "model": bad})).await.0, 400, "{bad}");
    }
    let v = app.ok_post(t, "/api/bots", json!({"name": "Exact", "model": "claude-opus-5-5"}), 201).await;
    assert_eq!(v["model"], "claude-opus-5-5");
    let (s, v) = app.patch(t, &format!("/api/bots/{}", id(&v)), json!({"model": "claude-haiku-4-5-20251001"})).await;
    assert_eq!((s, v["model"].as_str()), (200, Some("claude-haiku-4-5-20251001")));
    assert_eq!(app.post(t, "/api/bots", json!({"name": "m", "engine": "llama"})).await.0, 400);
    assert_eq!(app.post(t, "/api/bots", json!({"name": "m", "engine": "codex", "model": "bad model!"})).await.0, 400);
    let c = app.ok_post(t, "/api/bots", json!({"name": "Codex Bot", "engine": "codex", "model": "gpt-5.1-codex"}), 201).await;
    assert_eq!((c["engine"].as_str(), c["model"].as_str()), (Some("codex"), Some("gpt-5.1-codex")));
    let cid = id(&c);
    // switching engine needs a model that is valid for it
    assert_eq!(app.patch(t, &format!("/api/bots/{cid}"), json!({"engine": "claude"})).await.0, 400);
    let (s, v) = app.patch(t, &format!("/api/bots/{cid}"), json!({"engine": "claude", "model": "opus"})).await;
    assert_eq!((s, v["engine"].as_str(), v["model"].as_str()), (200, Some("claude"), Some("opus")));
    assert_eq!(app.patch(t, &format!("/api/bots/{cid}"), json!({"model": "gpt-5.1-codex"})).await.0, 400);

    // avatar: object of known string fields, or null to clear
    let av = json!({"shape": "blob", "color": "teal"});
    let v = app.ok_post(t, "/api/bots", json!({"name": "Face", "avatar": av}), 201).await;
    assert_eq!(v["avatar"], av);
    let fid = id(&v);
    let built = json!({"shape": 2, "color": "#4fb98a", "eyes": 0, "mouth": 3, "accessory": "crown"});
    assert_eq!(app.ok_post(t, "/api/bots", json!({"name": "Built", "avatar": built.clone()}), 201).await["avatar"], built);
    for bad in [json!("str"), json!({"hat": "top"}), json!({"shape": -1}), json!({"shape": 1.5}), json!({"shape": true}), json!({"shape": "x".repeat(33)})] {
        assert_eq!(app.post(t, "/api/bots", json!({"name": "bad", "avatar": bad.clone()})).await.0, 400, "{bad}");
        assert_eq!(app.patch(t, &format!("/api/bots/{fid}"), json!({"avatar": bad})).await.0, 400);
    }
    let (_, v) = app.patch(t, &format!("/api/bots/{fid}"), json!({"name": "Face 2"})).await;
    assert_eq!((v["avatar"].clone(), v["name"].as_str()), (av, Some("Face 2")), "absent avatar is left alone");
    let (_, v) = app.patch(t, &format!("/api/bots/{fid}"), json!({"avatar": null})).await;
    assert!(v["avatar"].is_null());

    let (s, v) = app.patch(t, &format!("/api/bots/{bid}"), json!({"paused": true, "persona": "be nice", "name": "RB"})).await;
    assert_eq!((s, v["paused"].clone(), v["persona"].as_str(), v["name"].as_str()), (200, json!(true), Some("be nice"), Some("RB")));
    assert_eq!(app.get(t, &format!("/api/bots/{bid}")).await.1["paused"], true);
    assert_eq!(len(&app.get(t, "/api/bots").await.1), 6);
    assert_eq!(app.get(t, "/api/bots/not-a-uuid").await.0, 400);
    assert_eq!(app.get(t, &format!("/api/bots/{}", Uuid::new_v4())).await.0, 404);
    let (_, ov) = app.get(t, "/api/overview").await;
    assert_eq!((len(&ov["bots"]), ov["pending_approvals"].clone()), (6, json!(0)));

    // skills are read-only (the daemon mirrors them)
    app.exec("insert into skills (owner_id, bot_id, name, body) select owner_id, id, 'sk', 'b' from bots where slug = 'ok-1'", &[]).await;
    let ok1 = app.get(t, "/api/bots").await.1.as_array().unwrap().iter().find(|b| b["slug"] == "ok-1").map(id).unwrap();
    assert_eq!(len(&app.get(t, &format!("/api/bots/{ok1}/skills")).await.1), 1);

    assert_eq!(app.del(t, &format!("/api/bots/{bid}")).await.0, 204);
    assert_eq!(app.del(t, &format!("/api/bots/{bid}")).await.0, 404);
    assert_eq!(app.get(t, &format!("/api/bots/{bid}/skills")).await.0, 404);
}

// ------------------------------------------------------------------ threads, messages, runs

#[tokio::test(flavor = "multi_thread")]
async fn threads_messages_runs() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Chatty").await;

    // thread: the body is optional
    let (s, th) = app.call(Method::POST, &format!("/api/bots/{bot}/threads"), Some(t), None).await;
    assert_eq!((s, th["title"].as_str()), (201, Some("New thread")));
    assert!(th.get("claude_session_id").is_none());
    let thid = id(&th);
    assert_eq!(app.post(t, &format!("/api/bots/{bot}/threads"), json!({"title": "x".repeat(201)})).await.0, 400);
    let (s, v) = app.patch(t, &format!("/api/threads/{thid}"), json!({"title": " Plans "})).await;
    assert_eq!((s, v["title"].as_str()), (200, Some("Plans")));
    assert_eq!(app.patch(t, &format!("/api/threads/{thid}"), json!({"title": " "})).await.0, 400);

    // a user message queues a chat run through the DB trigger
    assert_eq!(app.post(t, &format!("/api/threads/{thid}/messages"), json!({"content": "  "})).await.0, 400);
    assert_eq!(app.post(t, &format!("/api/threads/{}/messages", Uuid::new_v4()), json!({"content": "hi"})).await.0, 404);
    let m = app.ok_post(t, &format!("/api/threads/{thid}/messages"), json!({"content": "hello there"}), 201).await;
    assert_eq!((m["role"].as_str(), m["content"].as_str()), (Some("user"), Some("hello there")));
    let (_, runs) = app.get(t, &format!("/api/threads/{thid}/runs")).await;
    assert_eq!(len(&runs), 1);
    let r = &runs[0];
    assert_eq!((r["status"].as_str(), r["kind"].as_str(), r["prompt"].as_str()), (Some("queued"), Some("chat"), Some("hello there")));
    let rid = id(r);
    assert_eq!(len(&app.get(t, &format!("/api/bots/{bot}/runs?limit=5")).await.1), 1);
    assert_eq!(app.get(t, &format!("/api/runs/{rid}")).await.1["thread_id"].as_str(), Some(thid.as_str()));

    // messages: order, limit, `before` cursor
    app.ok_post(t, &format!("/api/threads/{thid}/messages"), json!({"content": "second"}), 201).await;
    let (_, msgs) = app.get(t, &format!("/api/threads/{thid}/messages")).await;
    assert_eq!(msgs.as_array().unwrap().iter().map(|m| m["content"].as_str().unwrap()).collect::<Vec<_>>(), ["hello there", "second"]);
    let (_, last) = app.get(t, &format!("/api/threads/{thid}/messages?limit=1")).await;
    assert_eq!(last[0]["content"], "second");
    let ts = last[0]["created_at"].as_str().unwrap().replace('+', "%2B");
    let (_, older) = app.get(t, &format!("/api/threads/{thid}/messages?before={ts}")).await;
    assert_eq!(older[0]["content"], "hello there");
    assert_eq!(app.get(t, &format!("/api/threads/{thid}/messages?before=yesterday")).await.0, 400);

    // events: ascending, `after_seq` cursor
    for seq in 0..3 {
        app.exec(&format!("insert into events (owner_id, run_id, seq, kind, payload) values ($1, $2, {seq}, 'text', '{{\"text\":\"t{seq}\"}}')"), &[a.id, rid.parse().unwrap()]).await;
    }
    let (_, ev) = app.get(t, &format!("/api/runs/{rid}/events")).await;
    assert_eq!(ev.as_array().unwrap().iter().map(|e| e["seq"].as_i64().unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(len(&app.get(t, &format!("/api/runs/{rid}/events?after_seq=1")).await.1), 1);

    // cancel state machine: queued|running|waiting_approval -> cancelled; finished -> 409; unknown -> 404
    let (s, v) = app.post(t, &format!("/api/runs/{rid}/cancel"), json!({})).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("cancelled")));
    assert!(v["finished_at"].is_string());
    assert_eq!(app.post(t, &format!("/api/runs/{rid}/cancel"), json!({})).await.0, 409);
    assert_eq!(app.post(t, &format!("/api/runs/{}/cancel", Uuid::new_v4()), json!({})).await.0, 404);
    for (from, want) in [("running", 200), ("waiting_approval", 200), ("succeeded", 409), ("failed", 409)] {
        let (_, run) = app.chat(t, &bot, "again").await;
        app.exec(&format!("update runs set status = '{from}' where id = $1"), &[uid(&run)]).await;
        assert_eq!(app.post(t, &format!("/api/runs/{}/cancel", id(&run)), json!({})).await.0, want, "{from}");
    }

    // deleting a thread takes its messages and runs with it
    assert_eq!(app.del(t, &format!("/api/threads/{thid}")).await.0, 204);
    assert_eq!(app.get(t, &format!("/api/threads/{thid}/messages")).await.0, 404);
    assert_eq!(app.get(t, &format!("/api/runs/{rid}")).await.0, 404);
    assert_eq!(app.del(t, &format!("/api/threads/{thid}")).await.0, 404);
}

// ------------------------------------------------------------------ approvals

#[tokio::test(flavor = "multi_thread")]
async fn approvals_decide_only_when_pending() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Asker").await;
    let (_, run) = app.chat(t, &bot, "do it").await;
    let (b, r) = (bot.parse().unwrap(), uid(&run));
    let (x, y, z) = (app.approval(a.id, r, b, "Bash").await, app.approval(a.id, r, b, "Write").await, app.approval(a.id, r, b, "ask_user").await);

    let (_, all) = app.get(t, "/api/approvals").await;
    assert_eq!(len(&all), 3);
    assert_eq!(all[0]["bot_slug"], "asker");
    assert_eq!(app.get(t, "/api/approvals?status=bogus").await.0, 400);
    assert_eq!(len(&app.get(t, &format!("/api/approvals?status=pending&bot_id={bot}")).await.1), 3);
    assert_eq!(len(&app.get(t, &format!("/api/approvals?bot_id={}", Uuid::new_v4())).await.1), 0);
    assert_eq!(app.get(t, "/api/overview").await.1["pending_approvals"], 3);

    assert_eq!(app.post(t, &format!("/api/approvals/{x}"), json!({"decision": "maybe"})).await.0, 400);
    assert_eq!(app.post(t, &format!("/api/approvals/{x}"), json!({"decision": "approve", "response": "x".repeat(20_001)})).await.0, 400);
    let (s, v) = app.post(t, &format!("/api/approvals/{x}"), json!({"decision": "approve", "response": "go ahead"})).await;
    assert_eq!((s, v["status"].as_str(), v["decided_by"].as_str(), v["response"].as_str()), (200, Some("approved"), Some("user"), Some("go ahead")));
    assert!(v["decided_at"].is_string());
    assert_eq!(app.post(t, &format!("/api/approvals/{x}"), json!({"decision": "deny"})).await.0, 409, "can't flip a decision");
    let (_, v) = app.post(t, &format!("/api/approvals/{y}"), json!({"decision": "deny"})).await;
    assert_eq!(v["status"], "denied");
    app.exec("update approvals set status = 'expired' where id = $1", &[z]).await;
    assert_eq!(app.post(t, &format!("/api/approvals/{z}"), json!({"decision": "approve"})).await.0, 409, "expired is final");
    assert_eq!(app.post(t, &format!("/api/approvals/{}", Uuid::new_v4()), json!({"decision": "approve"})).await.0, 404);
    assert_eq!(len(&app.get(t, "/api/approvals?status=pending").await.1), 0);
}

/// A pending approval as the daemon makes it: with what the owner may edit and the rule "Always allow" would add.
async fn offered(app: &App, a: &User, run: Uuid, bot: Uuid, tool: &str, input: Value, editable: &[&str], rule: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "insert into approvals (owner_id, run_id, bot_id, tool_name, input, editable, allow_rule)
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(a.id)
    .bind(run)
    .bind(bot)
    .bind(tool)
    .bind(sqlx::types::Json(input))
    .bind(editable.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    .bind(rule)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_edit_note_revise_and_always_allow() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Editor").await;
    let (_, run) = app.chat(t, &bot, "do it").await;
    let (b, r) = (bot.parse::<Uuid>().unwrap(), uid(&run));
    let bash = json!({ "command": "ls -la", "description": "look around" });
    let decide = |id: Uuid| format!("/api/approvals/{id}");

    // Edit & approve: only the offered field, only with approve, never together with "always".
    let x = offered(&app, &a, r, b, "Bash", bash.clone(), &["command"], None).await;
    for bad in [
        json!({ "decision": "approve", "edits": { "description": "x" } }),
        json!({ "decision": "approve", "edits": { "command": " " } }),
        json!({ "decision": "deny", "edits": { "command": "ls" } }),
        json!({ "decision": "approve", "always": true, "edits": { "command": "ls" } }),
        json!({ "decision": "revise", "response": "shorter" }),
    ] {
        assert_eq!(app.post(t, &decide(x), bad.clone()).await.0, 400, "{bad}");
    }
    let (s, v) = app.post(t, &decide(x), json!({ "decision": "approve", "edits": { "command": "ls -la notes" } })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "approved");
    assert_eq!(v["edited_input"], json!({ "command": "ls -la notes", "description": "look around" }));
    assert_eq!(v["input"], bash, "the proposal is kept as it was");
    assert!(v.get("rule").is_none());

    // Deny with a note: the note is the response.
    let y = offered(&app, &a, r, b, "Bash", bash.clone(), &["command"], None).await;
    let (_, v) = app.post(t, &decide(y), json!({ "decision": "deny", "response": "use the dashboard instead" })).await;
    assert_eq!((v["status"].as_str(), v["response"].as_str()), (Some("denied"), Some("use the dashboard instead")));
    assert!(v["edited_input"].is_null());

    // Always allow (read-only connector tools only): approves and adds the offered rule for this teammate only; the
    // same rule is not added twice.
    let read = json!({ "owner": "acme", "repo": "app", "issue_number": 7 });
    for _ in 0..2 {
        let z = offered(&app, &a, r, b, "mcp__github__get_issue", read.clone(), &[], Some("mcp__github__get_issue")).await;
        let (s, v) = app.post(t, &decide(z), json!({ "decision": "approve", "always": true })).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["status"], "approved");
        assert_eq!((v["rule"]["pattern"].as_str(), v["rule"]["decision"].as_str()), (Some("mcp__github__get_issue"), Some("allow")));
        assert_eq!(v["rule"]["bot_id"].as_str(), Some(bot.as_str()));
    }
    let (_, rules) = app.get(t, &format!("/api/rules?bot_id={bot}")).await;
    assert_eq!(len(&rules), 1, "{rules}");
    assert_eq!(len(&app.get(t, "/api/rules").await.1), 0, "no global rule");
    let other = app.bot(t, "Other").await;
    assert_eq!(len(&app.get(t, &format!("/api/rules?bot_id={other}")).await.1), 0, "only the teammate that asked");

    // The stored rule is re-derived from the stored call: one the checks would not offer is refused.
    for (tool, input, rule) in [
        ("Bash", json!({ "command": "ls -la" }), "Bash(ls -la)"),
        ("Bash", json!({ "command": "python report.py" }), "Bash(python report.py)"),
        ("Bash", json!({ "command": "ls -la" }), "Bash"),
        ("mcp__github__create_issue", json!({ "title": "x" }), "mcp__github__create_issue"),
        ("mcp__browser__browser_click", json!({ "ref": "e1" }), "mcp__browser__browser_click"),
        ("mcp__github__get_issue", read.clone(), "mcp__github__*"),
    ] {
        let x = offered(&app, &a, r, b, tool, input, &[], Some(rule)).await;
        assert_eq!(app.post(t, &decide(x), json!({ "decision": "approve", "always": true })).await.0, 400, "{rule}");
        app.exec("update approvals set status = 'denied' where id = $1", &[x]).await;
    }
    let w = offered(&app, &a, r, b, "Write", json!({ "file_path": ".claude/settings.json", "content": "{}" }), &["content"], Some("Write")).await;
    assert_eq!(app.post(t, &decide(w), json!({ "decision": "approve", "always": true })).await.0, 400, "no file tools");
    app.exec("update approvals set status = 'denied' where id = $1", &[w]).await;

    // Never for what the daemon didn't offer (always-human actions), nor for questions and drafts.
    let h = offered(&app, &a, r, b, "Bash", json!({ "command": "rm -rf build" }), &["command"], None).await;
    assert_eq!(app.post(t, &decide(h), json!({ "decision": "approve", "always": true })).await.0, 400);
    let d = offered(&app, &a, r, b, "propose_draft", json!({ "kind": "post", "channel": "X", "body": "Hi" }), &[], Some("propose_draft")).await;
    assert_eq!(app.post(t, &decide(d), json!({ "decision": "approve", "always": true })).await.0, 400);
    assert_eq!(len(&app.get(t, &format!("/api/rules?bot_id={bot}")).await.1), 1);
    let (_, pending) = app.get(t, "/api/approvals?status=pending").await;
    assert_eq!(len(&pending), 2, "refused decisions leave the approval pending");
    assert_eq!(pending[0]["editable"], json!([]));

    // Stale requests: past their expiry, or of a run that is over, they can't be approved (nor add a rule) and are
    // marked expired.
    let late = offered(&app, &a, r, b, "mcp__github__get_issue", read.clone(), &[], Some("mcp__github__get_issue")).await;
    app.exec("update approvals set expires_at = now() - interval '1 minute' where id = $1", &[late]).await;
    let (s, v) = app.post(t, &decide(late), json!({ "decision": "approve", "always": true })).await;
    assert_eq!(s, 409, "{v}");
    let soon = offered(&app, &a, r, b, "Bash", bash.clone(), &["command"], None).await;
    app.exec("update approvals set expires_at = now() + interval '10 minutes' where id = $1", &[soon]).await;
    let (_, gone_run) = app.chat(t, &bot, "another").await;
    let over = offered(&app, &a, uid(&gone_run), b, "Bash", bash.clone(), &["command"], None).await;
    app.exec("update runs set status = 'failed' where id = $1", &[uid(&gone_run)]).await;
    assert_eq!(app.post(t, &decide(over), json!({ "decision": "approve" })).await.0, 409);
    let statuses: Vec<String> = sqlx::query_scalar("select status from approvals where id = any($1) order by created_at")
        .bind(vec![late, over])
        .fetch_all(&app.pool)
        .await
        .unwrap();
    assert_eq!(statuses, ["expired", "expired"]);
    assert_eq!(len(&app.get(t, &format!("/api/rules?bot_id={bot}")).await.1), 1, "no rule from a stale request");
    // Not yet expired: fine.
    assert_eq!(app.post(t, &decide(soon), json!({ "decision": "deny" })).await.0, 200);
    // The owner sees teammate rules in the full list too, to remove them.
    let (_, all) = app.get(t, "/api/rules?all=1").await;
    assert!(all.as_array().unwrap().iter().any(|r| r["pattern"] == "mcp__github__get_issue" && r["bot_id"].as_str() == Some(bot.as_str())), "{all}");

    // Drafts: edit and approve (the proposal stays as it was), ask for changes, reject with a note.
    let draft = json!({ "kind": "reply", "channel": "X", "to": "https://x.com/a/status/1", "body": "Thanks!" });
    let fields = ["body", "subject", "to"];
    let d1 = offered(&app, &a, r, b, "propose_draft", draft.clone(), &fields, None).await;
    assert_eq!(app.post(t, &decide(d1), json!({ "decision": "approve", "edits": { "channel": "LinkedIn" } })).await.0, 400);
    let (s, v) = app.post(t, &decide(d1), json!({ "decision": "approve", "edits": { "body": " Thank you! \n", "subject": "" } })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["edited_input"]["body"], "Thank you!");
    assert_eq!(v["input"]["body"], "Thanks!");
    let d2 = offered(&app, &a, r, b, "propose_draft", draft.clone(), &fields, None).await;
    let (_, v) = app.post(t, &decide(d2), json!({ "decision": "revise", "response": "shorter, no exclamation mark" })).await;
    assert_eq!((v["status"].as_str(), v["response"].as_str()), (Some("revise"), Some("shorter, no exclamation mark")));
    assert_eq!(len(&app.get(t, "/api/approvals?status=revise").await.1), 1);
    // A draft with hidden characters is never approved as is; its cleaned-up text is.
    let sneaky = json!({ "kind": "post", "channel": "X", "body": "Pay at moc.live\u{202E} now" });
    let d4 = offered(&app, &a, r, b, "propose_draft", sneaky, &fields, None).await;
    let (s, v) = app.post(t, &decide(d4), json!({ "decision": "approve" })).await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(len(&app.get(t, "/api/approvals?status=pending").await.1), 3, "still pending after the refusal");
    let (s, v) = app.post(t, &decide(d4), json!({ "decision": "approve", "edits": { "body": "Pay at moc.live now" } })).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["edited_input"]["body"], "Pay at moc.live now");
    let d3 = offered(&app, &a, r, b, "propose_draft", draft, &fields, None).await;
    let (_, v) = app.post(t, &decide(d3), json!({ "decision": "deny", "response": "not this one" })).await;
    assert_eq!(v["status"], "denied");
    assert_eq!(app.post(t, &decide(d3), json!({ "decision": "approve" })).await.0, 409);

    // A draft doesn't wait inside its run: the run being over doesn't make it stale, only its week-long expiry does.
    let queued = json!({ "kind": "post", "channel": "X", "body": "Later" });
    let d5 = offered(&app, &a, uid(&gone_run), b, "propose_draft", queued.clone(), &fields, None).await;
    app.exec("update approvals set expires_at = now() + interval '7 days' where id = $1", &[d5]).await;
    let (s, v) = app.post(t, &decide(d5), json!({ "decision": "revise", "response": "warmer" })).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("revise")), "{v}");
    let d6 = offered(&app, &a, uid(&gone_run), b, "propose_draft", queued, &fields, None).await;
    app.exec("update approvals set expires_at = now() - interval '1 second' where id = $1", &[d6]).await;
    assert_eq!(app.post(t, &decide(d6), json!({ "decision": "approve" })).await.0, 409, "expired drafts are final");
}

// ------------------------------------------------------------------ folders

#[tokio::test(flavor = "multi_thread")]
async fn folders_are_checked_and_scoped() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Desk").await;
    let url = format!("/api/bots/{bot}/folders");
    assert_eq!(app.get(t, &url).await.1, json!([]));
    let docs = test_dir("Documents");
    let (s, f) = app.post(t, &url, json!({ "path": format!("{}/", docs.display()) })).await;
    assert_eq!(s, 201, "{f}");
    assert_eq!(f["mode"], "read", "read only by default");
    let real = docs.canonicalize().unwrap().display().to_string();
    assert_eq!(f["path"].as_str(), Some(real.trim_start_matches(r"\\?\")), "stored resolved");
    assert!(f.get("owner_id").is_none());
    assert_eq!(app.post(t, &url, json!({ "path": docs.display().to_string(), "mode": "write" })).await.0, 409);

    let refused = |v: &Value, why: &str| assert!(v["error"].as_str().is_some_and(|e| e.contains(why)), "{why}: {v}");
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap();
    for (path, why) in [
        (home.clone(), "whole home folder"),
        (format!("{home}/AppData"), if cfg!(windows) { "app data" } else { "" }),
        (test_dir("bots").display().to_string(), "teammates' workspaces"),
        ("relative/folder".to_owned(), "full path"),
        (r"\\server\share".to_owned(), "Network folders"),
        (test_dir("x").join("missing").display().to_string(), "doesn't exist"),
        (if cfg!(windows) { r"C:\".to_owned() } else { "/".to_owned() }, "whole drive"),
        (if cfg!(windows) { r"C:\Windows".to_owned() } else { "/etc".to_owned() }, if cfg!(windows) { "Windows system files" } else { "system files" }),
    ] {
        if why.is_empty() {
            continue;
        }
        let (s, v) = app.post(t, &url, json!({ "path": path })).await;
        assert_eq!(s, 400, "{path}: {v}");
        refused(&v, why);
    }
    assert_eq!(app.post(t, &url, json!({ "path": docs.display().to_string(), "mode": "admin" })).await.0, 400);

    let id = f["id"].as_str().unwrap();
    let (s, v) = app.patch(t, &format!("/api/folders/{id}"), json!({ "mode": "write" })).await;
    assert_eq!((s, v["mode"].as_str()), (200, Some("write")));
    assert_eq!(app.patch(t, &format!("/api/folders/{id}"), json!({ "mode": "all" })).await.0, 400);
    assert_eq!(app.get(t, &url).await.1[0]["mode"], "write");

    // Only the owner's: another account sees and changes nothing.
    let b = app.second().await;
    assert_eq!(app.get(&b.tok, &url).await.0, 404);
    assert_eq!(app.post(&b.tok, &url, json!({ "path": docs.display().to_string() })).await.0, 404);
    assert_eq!(app.patch(&b.tok, &format!("/api/folders/{id}"), json!({ "mode": "read" })).await.0, 404);
    assert_eq!(app.del(&b.tok, &format!("/api/folders/{id}")).await.0, 404);

    assert_eq!(app.del(t, &format!("/api/folders/{id}")).await.0, 204);
    assert_eq!(app.del(t, &format!("/api/folders/{id}")).await.0, 404);
    assert_eq!(app.get(t, &url).await.1, json!([]));
}

// ------------------------------------------------------------------ rules

#[tokio::test(flavor = "multi_thread")]
async fn rules_global_vs_bot() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Ruled").await;
    let g = app.ok_post(t, "/api/rules", json!({"pattern": "Bash(git status)", "decision": "allow", "note": " read only "}), 201).await;
    assert!(g["bot_id"].is_null());
    assert_eq!(g["note"], "read only");
    let s = app.ok_post(t, "/api/rules", json!({"bot_id": bot, "pattern": "WebFetch", "decision": "review"}), 201).await;
    assert_eq!(s["bot_id"].as_str(), Some(bot.as_str()));

    let names = |v: &Value| v.as_array().unwrap().iter().map(|r| r["pattern"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names(&app.get(t, "/api/rules").await.1), ["Bash(git status)"]);
    assert_eq!(names(&app.get(t, &format!("/api/rules?bot_id={bot}")).await.1), ["WebFetch"]);
    assert_eq!(names(&app.get(t, "/api/rules?all=1").await.1), ["Bash(git status)", "WebFetch"]);

    for bad in [
        json!({"pattern": "x", "decision": "maybe"}),
        json!({"pattern": " ", "decision": "ask"}),
        json!({"pattern": "x".repeat(501), "decision": "ask"}),
        json!({"pattern": "x", "decision": "ask", "note": "n".repeat(1001)}),
    ] {
        assert_eq!(app.post(t, "/api/rules", bad.clone()).await.0, 400, "{bad}");
    }
    for d in ["allow", "deny", "ask", "review"] {
        app.ok_post(t, "/api/rules", json!({"pattern": d, "decision": d}), 201).await;
    }
    assert_eq!(app.post(t, "/api/rules", json!({"bot_id": Uuid::new_v4(), "pattern": "x", "decision": "ask"})).await.0, 404);
    assert_eq!(app.del(t, &format!("/api/rules/{}", id(&g))).await.0, 204);
    assert_eq!(app.del(t, &format!("/api/rules/{}", id(&g))).await.0, 404);
    // deleting the bot takes its own rules along, global ones stay
    app.del(t, &format!("/api/bots/{bot}")).await;
    assert_eq!(len(&app.get(t, "/api/rules?all=1").await.1), 4);
}

// ------------------------------------------------------------------ schedules

#[tokio::test(flavor = "multi_thread")]
async fn schedules_cron_and_gate() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Clock").await;
    let p = format!("/api/bots/{bot}/schedules");

    for bad in ["", "* * * *", "* * * * * *", "61 * * * *", "a b c d e", "* 25 * * *"] {
        assert_eq!(app.post(t, &p, json!({"cron": bad, "prompt": "x"})).await.0, 400, "cron {bad:?}");
    }
    assert_eq!(app.post(t, &p, json!({"cron": "* * * * *", "prompt": " "})).await.0, 400);
    assert_eq!(app.post(t, &p, json!({"cron": "* * * * *", "prompt": "x", "kind": "chat"})).await.0, 400);
    assert_eq!(app.post(t, &p, json!({"cron": "* * * * *", "prompt": "x", "gate_command": "g".repeat(2001)})).await.0, 400);
    let s = app.ok_post(t, &p, json!({"cron": "  0   9 * *  1-5 ", "prompt": "standup", "gate_command": " test -f x "}), 201).await;
    assert_eq!((s["cron"].as_str(), s["kind"].as_str(), s["enabled"].clone(), s["gate_command"].as_str()), (Some("0 9 * * 1-5"), Some("scheduled"), json!(true), Some("test -f x")));
    assert!(s["thread_id"].is_string(), "DB trigger gives the schedule a thread");
    let sid = id(&s);
    assert_eq!(app.get(t, &format!("/api/bots/{bot}/threads")).await.1[0]["source"], "schedule");

    let sp = format!("/api/schedules/{sid}");
    assert_eq!(app.patch(t, &sp, json!({"cron": "nope"})).await.0, 400);
    let (s, v) = app.patch(t, &sp, json!({"cron": "*/5 * * * *", "enabled": false, "kind": "proactive", "prompt": "p2"})).await;
    assert_eq!((s, v["cron"].as_str(), v["enabled"].clone(), v["kind"].as_str(), v["gate_command"].as_str()), (200, Some("*/5 * * * *"), json!(false), Some("proactive"), Some("test -f x")));
    let (_, v) = app.patch(t, &sp, json!({"gate_command": ""})).await;
    assert!(v["gate_command"].is_null(), "empty string clears the gate");
    assert_eq!(len(&app.get(t, &p).await.1), 1);
    assert_eq!(app.del(t, &sp).await.0, 204);
    assert_eq!(app.del(t, &sp).await.0, 404);
    assert_eq!(app.patch(t, &sp, json!({"enabled": true})).await.0, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn schedules_page_label_and_run_now() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let (beta, alpha) = (app.bot(t, "Beta").await, app.bot(t, "Alpha").await);
    let s1 = app.ok_post(t, &format!("/api/bots/{beta}/schedules"), json!({"cron": "0 9 * * 1-5", "prompt": "standup"}), 201).await;
    let s2 = app.ok_post(t, &format!("/api/bots/{alpha}/schedules"), json!({"cron": "0 8 * * *", "prompt": "news", "enabled": false}), 201).await;

    // Every teammate's schedules in one list, by teammate, each with its label and teammate.
    let (s, all) = app.get(t, "/api/schedules").await;
    assert_eq!((s, len(&all)), (200, 2));
    assert_eq!((all[0]["bot_name"].as_str(), all[0]["id"].as_str()), (Some("Alpha"), s2["id"].as_str()));
    assert_eq!(all[1]["label"], "Schedule: standup");
    assert!(all[1]["last_status"].is_null());

    let sp = format!("/api/schedules/{}", id(&s1));
    assert_eq!(app.patch(t, &sp, json!({"label": "  "})).await.0, 400);
    assert_eq!(app.patch(t, &sp, json!({"label": "l".repeat(101)})).await.0, 400);
    let (s, v) = app.patch(t, &sp, json!({"label": " Morning standup ", "cron": "30 9 * * 1-5"})).await;
    assert_eq!((s, v["cron"].as_str()), (200, Some("30 9 * * 1-5")));
    let (_, all) = app.get(t, "/api/schedules").await;
    assert_eq!(all[1]["label"], "Morning standup");

    // Run now: one queued run in the schedule's own thread, its fire time untouched; not twice while queued.
    let (s, run) = app.post(t, &format!("{sp}/run"), json!({})).await;
    assert_eq!(s, 201, "{run}");
    assert_eq!((run["kind"].as_str(), run["status"].as_str(), run["prompt"].as_str()), (Some("scheduled"), Some("queued"), Some("standup")));
    assert_eq!(run["thread_id"], s1["thread_id"]);
    assert_eq!(app.post(t, &format!("{sp}/run"), json!({})).await.0, 409);
    let (_, all) = app.get(t, "/api/schedules").await;
    assert_eq!(all[1]["last_status"], "queued");
    assert!(all[1]["last_run_at"].is_string());
    // A turned-off schedule can still be run by hand.
    assert_eq!(app.post(t, &format!("/api/schedules/{}/run", id(&s2)), json!({})).await.0, 201);
    assert_eq!(app.post(t, &format!("/api/schedules/{}/run", Uuid::new_v4()), json!({})).await.0, 404);

    // Another user sees and runs none of them.
    let b = app.second().await;
    assert_eq!(len(&app.get(&b.tok, "/api/schedules").await.1), 0);
    assert_eq!(app.post(&b.tok, &format!("{sp}/run"), json!({})).await.0, 404);
    assert_eq!(app.patch(&b.tok, &sp, json!({"label": "mine"})).await.0, 404);
}

// ------------------------------------------------------------------ memories + dream

#[tokio::test(flavor = "multi_thread")]
async fn memories_and_dream() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Remember").await;
    let p = format!("/api/bots/{bot}/memories");

    let m = app.ok_post(t, &p, json!({"content": " likes tea "}), 201).await;
    assert_eq!((m["content"].as_str(), m["source"].as_str(), m["status"].as_str()), (Some("likes tea"), Some("user"), Some("active")));
    assert_eq!(app.post(t, &p, json!({"content": ""})).await.0, 400);
    assert_eq!(app.post(t, &p, json!({"content": "x".repeat(10_001)})).await.0, 400);
    app.exec("insert into memories (owner_id, bot_id, content, source, status) values ($1, $2, 'proposed one', 'bot', 'proposed'), ($1, $2, 'rejected one', 'bot', 'rejected')", &[a.id, bot.parse().unwrap()]).await;

    let (_, all) = app.get(t, &p).await;
    assert_eq!((len(&all), all[0]["status"].as_str()), (3, Some("proposed")), "proposed sorts first");
    for st in ["active", "proposed", "rejected"] {
        let (_, v) = app.get(t, &format!("{p}?status={st}")).await;
        assert_eq!((len(&v), v[0]["status"].as_str()), (1, Some(st)));
    }
    assert_eq!(app.get(t, &format!("{p}?status=weird")).await.0, 400);

    let prop = id(&app.get(t, &format!("{p}?status=proposed")).await.1[0]);
    let mp = format!("/api/memories/{prop}");
    let (s, v) = app.patch(t, &mp, json!({"status": "active", "content": "edited"})).await;
    assert_eq!((s, v["status"].as_str(), v["content"].as_str()), (200, Some("active"), Some("edited")));
    assert_eq!(app.patch(t, &mp, json!({"status": "weird"})).await.0, 400);
    assert_eq!(app.patch(t, &mp, json!({"content": " "})).await.0, 400);
    assert_eq!(app.patch(t, &format!("/api/memories/{}", Uuid::new_v4()), json!({"status": "active"})).await.0, 404);
    assert_eq!(len(&app.get(t, &format!("{p}?status=active")).await.1), 2);
    assert_eq!(app.del(t, &mp).await.0, 204);
    assert_eq!(app.del(t, &mp).await.0, 404);

    // dream: 202 + queued run; 409 while one is pending; allowed again once it is over
    let d = format!("/api/bots/{bot}/dream");
    let run = app.ok_post(t, &d, json!({}), 202).await["run_id"].as_str().unwrap().to_string();
    let r = app.get(t, &format!("/api/runs/{run}")).await.1;
    assert_eq!((r["kind"].as_str(), r["status"].as_str()), (Some("dream"), Some("queued")));
    assert_eq!(app.post(t, &d, json!({})).await.0, 409);
    app.post(t, &format!("/api/runs/{run}/cancel"), json!({})).await;
    app.ok_post(t, &d, json!({}), 202).await;
    assert_eq!(app.post(t, &format!("/api/bots/{}/dream", Uuid::new_v4()), json!({})).await.0, 404);
}

// ------------------------------------------------------------------ connectors

#[tokio::test(flavor = "multi_thread")]
async fn templates_hire_a_teammate() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;

    let (s, list) = app.get(t, "/api/templates").await;
    assert_eq!(s, 200);
    let tpl = list.as_array().unwrap().iter().find(|x| x["id"] == "social-media-manager").expect("catalog has it").clone();
    assert!(tpl["questions"].is_array() && tpl["logins"].is_array());
    assert_eq!(app.call(Method::GET, "/api/templates", None, None).await.0, 401);
    assert_eq!(app.post(t, "/api/templates/nope/create", json!({})).await.0, 404);
    assert_eq!(app.post(t, "/api/templates/social-media-manager/create", json!({"engine": "codex"})).await.0, 400, "codex needs a model");
    assert_eq!(app.post(t, "/api/templates/social-media-manager/create", json!({"model": "gpt-9"})).await.0, 400);
    assert_eq!(app.post(t, "/api/templates/social-media-manager/create", json!({"avatar": {"shape": "x".repeat(40)}})).await.0, 400);
    assert_eq!(app.post(t, "/api/bots", json!({"name": "x"})).await.0, 201, "nothing half-made by the failures above");
    assert_eq!(len(&app.get(t, "/api/bots").await.1), 1);

    // a connector the owner already has from a suggested preset gets linked; others don't
    sqlx::query("insert into connectors (owner_id, name, preset, transport, command) values ($1, 'search', 'brave-search', 'stdio', 'npx'), ($1, 'gh', 'github', 'stdio', 'npx')")
        .bind(a.id)
        .execute(&app.pool)
        .await
        .unwrap();

    let v = app
        .ok_post(t, "/api/templates/community-listener/create", json!({"answers": {"product": "Familiar", "keywords": " ai teammate "}, "name": "Scout"}), 201)
        .await;
    let bot = &v["bot"];
    let bid = id(bot);
    assert_eq!((bot["name"].as_str(), bot["slug"].as_str(), bot["model"].as_str(), bot["engine"].as_str()), (Some("Scout"), Some("scout"), Some("sonnet"), Some("claude")));
    let persona = bot["persona"].as_str().unwrap();
    assert!(persona.contains("problems Familiar solves") && persona.contains("Listen for: ai teammate."), "{persona}");
    assert!(persona.contains("(not set yet)") && !persona.contains("{{"), "unanswered questions are marked");
    assert_eq!(bot["avatar"]["accessory"], "headphones");
    assert!(v["first_task"].as_str().unwrap().contains("ai teammate"));
    let setup = &bot["setup"];
    assert_eq!(setup["template"], "community-listener");
    assert_eq!(setup["dismissed"], false);
    assert_eq!(setup["logins"][0], json!({"site": "Reddit", "url": "https://www.reddit.com/login/", "done": false}));

    // schedules exist, are off, have their own labelled threads and filled prompts
    let (_, scheds) = app.get(t, &format!("/api/bots/{bid}/schedules")).await;
    assert_eq!(len(&scheds), 2);
    assert!(scheds.as_array().unwrap().iter().all(|s| s["enabled"] == false && !s["prompt"].as_str().unwrap().contains("{{")));
    let mut ids: Vec<&str> = scheds.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    let mut setup_ids: Vec<&str> = setup["schedules"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    ids.sort();
    setup_ids.sort();
    assert_eq!(ids, setup_ids);
    let (_, threads) = app.get(t, &format!("/api/bots/{bid}/threads")).await;
    let titles: Vec<&str> = threads.as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Morning sweep") && threads.as_array().unwrap().iter().all(|t| t["source"] == "schedule"), "{titles:?}");
    let (_, linked) = app.get(t, &format!("/api/bots/{bid}/connectors")).await;
    assert_eq!(linked.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), ["search"]);

    // hiring the same template twice gets a fresh slug; edited instructions still get their placeholders filled
    let v2 = app
        .ok_post(t, "/api/templates/community-listener/create", json!({"name": "Scout", "instructions": "Watch {{communities}} only.", "answers": {"communities": "r/rust"}}), 201)
        .await;
    assert!(v2["bot"]["slug"].as_str().unwrap().starts_with("scout-"));
    assert_eq!(v2["bot"]["persona"], "Watch r/rust only.");

    // the Set up checklist
    let sp = format!("/api/bots/{bid}/setup");
    let (s, b) = app.patch(t, &sp, json!({"login": "Reddit"})).await;
    assert_eq!((s, b["setup"]["logins"][0]["done"].clone(), b["setup"]["logins"][1]["done"].clone()), (200, json!(true), json!(false)));
    let (_, b) = app.patch(t, &sp, json!({"login": "Reddit", "done": false})).await;
    assert_eq!(b["setup"]["logins"][0]["done"], false);
    assert_eq!(app.patch(t, &sp, json!({"login": "MySpace"})).await.0, 400);
    let (_, b) = app.patch(t, &sp, json!({"dismissed": true})).await;
    assert_eq!(b["setup"]["dismissed"], true);
    let plain = app.bot(t, "Plain").await;
    assert_eq!(app.patch(t, &format!("/api/bots/{plain}/setup"), json!({"dismissed": true})).await.0, 400);
    assert_eq!(app.patch(t, &format!("/api/bots/{}/setup", Uuid::new_v4()), json!({"dismissed": true})).await.0, 404);
    let b2 = app.second().await;
    assert_eq!(app.patch(&b2.tok, &sp, json!({"dismissed": false})).await.0, 404, "owner-scoped");
}

#[tokio::test(flavor = "multi_thread")]
async fn connectors_secrets_and_presets() {
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;

    let (s, presets) = app.get(t, "/api/connectors/presets").await;
    assert_eq!(s, 200);
    assert!(presets.as_array().unwrap().iter().any(|p| p["id"] == "github" && p["secret_fields"].is_array()));
    assert_eq!(app.call(Method::GET, "/api/connectors/presets", None, None).await.0, 401);

    for bad in [
        json!({"name": "Bad Name", "transport": "stdio", "command": "x"}),
        json!({"name": "familiar", "transport": "stdio", "command": "x"}),
        json!({"name": "browser", "transport": "stdio", "command": "x"}),
        json!({"name": "desktop", "transport": "stdio", "command": "x"}),
        json!({"name": "a", "transport": "ws", "command": "x"}),
        json!({"name": "a", "transport": "stdio"}),
        json!({"name": "a", "transport": "http"}),
        json!({"name": "a", "transport": "http", "url": "ftp://x"}),
        json!({"name": "a", "transport": "stdio", "command": "x", "secrets": {"env": {"BAD KEY": "v"}}}),
    ] {
        assert_eq!(app.post(t, "/api/connectors", bad.clone()).await.0, 400, "{bad}");
    }

    let c = app
        .ok_post(t, "/api/connectors", json!({"name": "gh", "preset": "github", "transport": "stdio", "command": "npx", "args": ["-y", "pkg"],
            "secrets": {"env": {"GITHUB_TOKEN": "ghp_supersecret"}, "headers": {"Authorization": "Bearer hunter2"}}}), 201)
        .await;
    let raw = c.to_string();
    assert!(!raw.contains("supersecret") && !raw.contains("hunter2") && c.get("secrets_enc").is_none(), "{raw}");
    assert_eq!((c["has_secrets"].clone(), c["secret_names"].clone(), c["args"].clone()), (json!(true), json!({"env": ["GITHUB_TOKEN"], "headers": ["Authorization"]}), json!(["-y", "pkg"])));
    let cid = id(&c);
    let enc: String = sqlx::query_scalar("select secrets_enc from connectors").fetch_one(&app.pool).await.unwrap();
    assert!(enc.starts_with("v1:") && !enc.contains("supersecret"), "ciphertext at rest");
    assert_eq!(app.post(t, "/api/connectors", json!({"name": "gh", "transport": "http", "url": "https://x.io/mcp"})).await.0, 409);
    let h = app.ok_post(t, "/api/connectors", json!({"name": "web", "transport": "http", "url": "https://x.io/mcp"}), 201).await;
    assert_eq!(h["has_secrets"], false);

    let (_, list) = app.get(t, "/api/connectors").await;
    assert_eq!(len(&list), 2);
    assert!(!list.to_string().contains("supersecret"));

    let cp = format!("/api/connectors/{cid}");
    let (s, v) = app.patch(t, &cp, json!({"enabled": false, "command": "uvx"})).await;
    assert_eq!((s, v["enabled"].clone(), v["command"].as_str(), v["has_secrets"].clone()), (200, json!(false), Some("uvx"), json!(true)), "secrets kept when absent from the patch");
    assert_eq!(app.patch(t, &cp, json!({"transport": "carrier-pigeon"})).await.0, 400);
    assert_eq!(app.patch(t, &cp, json!({"url": "javascript:alert(1)"})).await.0, 400);
    let (_, v) = app.patch(t, &cp, json!({"secrets": {"env": {"OTHER": "v2"}}})).await;
    assert_eq!(v["secret_names"], json!({"env": ["OTHER"], "headers": []}), "secrets replace all");
    let (_, v) = app.patch(t, &cp, json!({"secrets": {}})).await;
    assert_eq!(v["has_secrets"], false);
    assert_eq!(app.patch(t, &format!("/api/connectors/{}", Uuid::new_v4()), json!({"enabled": true})).await.0, 404);

    // bot links replace the set; unknown ids are rejected as a whole
    let bot = app.bot(t, "Linked").await;
    let lp = format!("/api/bots/{bot}/connectors");
    assert_eq!(len(&app.get(t, &lp).await.1), 0);
    let (s, v) = app.put(t, &lp, json!({"connector_ids": [cid, id(&h), cid]})).await;
    assert_eq!((s, len(&v)), (200, 2));
    assert_eq!(app.put(t, &lp, json!({"connector_ids": [Uuid::new_v4()]})).await.0, 404);
    assert_eq!(len(&app.get(t, &lp).await.1), 2);
    assert_eq!(len(&app.put(t, &lp, json!({"connector_ids": [id(&h)]})).await.1), 1);
    assert_eq!(app.del(t, &cp).await.0, 204);
    assert_eq!(app.del(t, &cp).await.0, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_need_a_key() {
    let app = app!(false);
    let a = app.owner().await;
    let t = &a.tok;
    let (s, v) = app.post(t, "/api/connectors", json!({"name": "gh", "transport": "stdio", "command": "x", "secrets": {"env": {"K": "v"}}})).await;
    assert_eq!(s, 503, "{v}");
    app.ok_post(t, "/api/connectors", json!({"name": "gh", "transport": "stdio", "command": "x"}), 201).await;
    assert_eq!(app.post(t, "/api/channels", json!({"kind": "telegram", "token": "1:abc"})).await.0, 503);
}

// ------------------------------------------------------------------ channels (no network)

#[tokio::test(flavor = "multi_thread")]
async fn channels_validation_without_network() {
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;
    assert_eq!(len(&app.get(t, "/api/channels").await.1), 0);
    assert_eq!(app.post(t, "/api/channels", json!({"kind": "sms", "token": "1:abc"})).await.0, 400);
    for tok in ["no-colon", "has space:abc", "1:a/b", "1:a?b", &format!("1:{}", "x".repeat(200))] {
        assert_eq!(app.post(t, "/api/channels", json!({"kind": "telegram", "token": tok})).await.0, 400, "{tok}");
    }
    assert_eq!(app.post(t, "/api/channels", json!({"kind": "telegram", "token": "1:abc", "default_bot_id": Uuid::new_v4()})).await.0, 404);
    assert_eq!(app.patch(t, &format!("/api/channels/{}", Uuid::new_v4()), json!({"enabled": false})).await.0, 404);
    assert_eq!(app.del(t, &format!("/api/channels/{}", Uuid::new_v4())).await.0, 404);

    // an existing channel (inserted directly: creating one needs Telegram) never leaks its token
    let bot = app.bot(t, "Chan").await;
    let enc = familiar_crypto::SecretBox::from_base64(KEY).unwrap().encrypt(r#"{"token":"123:secrettoken"}"#).unwrap();
    app.exec(&format!("insert into channels (owner_id, kind, config_enc, pair_code) values ($1, 'telegram', '{enc}', 'ABCD2345')"), &[a.id]).await;
    let (_, list) = app.get(t, "/api/channels").await;
    assert!(!list.to_string().contains("secrettoken") && !list.to_string().contains("config_enc"));
    assert_eq!((list[0]["bound"].clone(), list[0]["pair_code"].as_str()), (json!(false), Some("ABCD2345")));
    let cp = format!("/api/channels/{}", id(&list[0]));
    let (s, v) = app.patch(t, &cp, json!({"enabled": false, "default_bot_id": bot})).await;
    assert_eq!((s, v["enabled"].clone(), v["default_bot_id"].as_str()), (200, json!(false), Some(bot.as_str())));
    assert_eq!(app.patch(t, &cp, json!({"default_bot_id": Uuid::new_v4()})).await.0, 404);
    assert_eq!(app.patch(t, &cp, json!({"token": "nocolon"})).await.0, 400);
    app.exec("update channels set chat_id = '42'", &[]).await;
    assert!(app.get(t, "/api/channels").await.1[0]["pair_code"].is_null(), "pair code hidden once bound");
    assert_eq!(app.del(t, &cp).await.0, 204);
}

// ------------------------------------------------------------------ triggers / webhooks

fn token_of(url: &Value) -> String {
    url.as_str().unwrap().rsplit('/').next().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn triggers_and_webhooks() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Hooked").await;
    let p = format!("/api/bots/{bot}/triggers");

    assert_eq!(app.post(t, &p, json!({"name": " ", "prompt": "x"})).await.0, 400);
    assert_eq!(app.post(t, &p, json!({"name": "n", "prompt": "x", "kind": "chat"})).await.0, 400);
    let c = app.ok_post(t, &p, json!({"name": "deploys", "prompt": "summarize"}), 201).await;
    assert!(c["url"].as_str().unwrap().starts_with(&format!("{}/hooks/", app.base)), "{c}");
    assert!(c.get("token_hash").is_none());
    let token = token_of(&c["url"]);
    assert!(token.len() >= 40);
    // the URL is shown once only
    let (_, list) = app.get(t, &p).await;
    assert_eq!(len(&list), 1);
    assert!(list[0].get("url").is_none() && list[0].get("token_hash").is_none() && !list.to_string().contains(&token));

    // public endpoint: no auth, any content type, queues a run in the trigger's thread
    let r = app.http.post(format!("{}/hooks/{token}", app.base)).header("content-type", "text/plain").body("build #7 failed").send().await.unwrap();
    assert_eq!(r.status(), 202);
    let run_id = r.json::<Value>().await.unwrap()["run_id"].as_str().unwrap().to_string();
    let (_, run) = app.get(t, &format!("/api/runs/{run_id}")).await;
    assert_eq!((run["status"].as_str(), run["kind"].as_str()), (Some("queued"), Some("scheduled")));
    assert_eq!(run["prompt"], "summarize\n\n--- webhook payload ---\nbuild #7 failed");
    assert_eq!(run["thread_id"], c["thread_id"]);
    assert_eq!(app.hook("nope", vec![]).await.status(), 404);

    // body limit is 64 KB (an oversized body is rejected before it counts against the rate limit)
    assert_eq!(app.hook(&token, vec![b'a'; 64 * 1024 + 1]).await.status(), 413);
    assert_eq!(app.hook(&token, vec![b'a'; 64 * 1024]).await.status(), 202);

    // rate limit: 30 per minute per trigger (2 used so far)
    for i in 2..30 {
        assert_eq!(app.hook(&token, b"x".to_vec()).await.status(), 202, "hit {i}");
    }
    assert_eq!(app.hook(&token, b"x".to_vec()).await.status(), 429);

    // a second trigger (own rate limit): rotate, disable, patch, thread recreation, delete
    let c2 = app.ok_post(t, &p, json!({"name": "second", "prompt": "p2", "kind": "proactive"}), 201).await;
    let (tid, old) = (id(&c2), token_of(&c2["url"]));
    assert_eq!(app.hook(&old, b"1".to_vec()).await.status(), 202);
    assert_eq!(app.post(t, &format!("/api/triggers/{}/rotate", Uuid::new_v4()), json!({})).await.0, 404);
    let rot = app.ok_post(t, &format!("/api/triggers/{tid}/rotate"), json!({}), 200).await;
    let new = token_of(&rot["url"]);
    assert_ne!(new, old);
    assert!(rot.get("token_hash").is_none());
    assert_eq!(app.hook(&old, b"1".to_vec()).await.status(), 404, "rotate invalidates the old URL");
    let r = app.hook(&new, b"2".to_vec()).await;
    assert_eq!(r.status(), 202);
    let kind = app.get(t, &format!("/api/runs/{}", r.json::<Value>().await.unwrap()["run_id"].as_str().unwrap())).await.1["kind"].clone();
    assert_eq!(kind, "proactive");

    let tp = format!("/api/triggers/{tid}");
    let (_, v) = app.patch(t, &tp, json!({"enabled": false, "name": "renamed"})).await;
    assert_eq!((v["enabled"].clone(), v["name"].as_str()), (json!(false), Some("renamed")));
    assert_eq!(app.hook(&new, b"3".to_vec()).await.status(), 404, "disabled triggers don't fire");
    assert_eq!(app.patch(t, &tp, json!({"kind": "bad"})).await.0, 400);
    app.patch(t, &tp, json!({"enabled": true})).await;
    // the thread was deleted: the next hit starts a fresh one
    app.exec("delete from threads where id = $1", &[c2["thread_id"].as_str().unwrap().parse().unwrap()]).await;
    assert_eq!(app.hook(&new, b"4".to_vec()).await.status(), 202);
    assert_eq!(app.del(t, &tp).await.0, 204);
    assert_eq!(app.hook(&new, b"5".to_vec()).await.status(), 404);
    assert_eq!(app.del(t, &tp).await.0, 404);
}

// ------------------------------------------------------------------ artifacts

#[tokio::test(flavor = "multi_thread")]
async fn artifacts_download() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Maker").await;
    let (_, run) = app.chat(t, &bot, "make a file").await;
    let art: Uuid = sqlx::query_scalar(
        "insert into artifacts (owner_id, run_id, bot_id, name, mime, bytes, storage, data) values ($1,$2,$3,'rep\"ort é.html','text/html',15,'db',$4) returning id",
    )
    .bind(a.id)
    .bind(uid(&run))
    .bind(bot.parse::<Uuid>().unwrap())
    .bind(b"<b>hi there</b>".to_vec())
    .fetch_one(&app.pool)
    .await
    .unwrap();

    for p in [format!("/api/runs/{}/artifacts", id(&run)), format!("/api/bots/{bot}/artifacts")] {
        let (s, v) = app.get(t, &p).await;
        assert_eq!((s, len(&v)), (200, 1));
        assert!(v[0].get("data").is_none() && v[0].get("r2_key").is_none() && v[0].get("owner_id").is_none());
        assert_eq!(v[0]["name"], "rep\"ort é.html");
    }
    let dl = format!("/api/artifacts/{art}/download");
    for r in [app.http.get(format!("{}{dl}", app.base)).bearer_auth(t), app.http.get(format!("{}{dl}?token={t}", app.base))] {
        let r = r.send().await.unwrap();
        assert_eq!(r.status(), 200);
        let h = r.headers().clone();
        assert_eq!(h["content-type"], "text/html");
        assert_eq!(h["content-security-policy"], "sandbox");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["content-disposition"], "inline; filename=\"rep_ort _.html\"");
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"<b>hi there</b>");
    }
    assert_eq!(app.call(Method::GET, &dl, None, None).await.0, 401);
    assert_eq!(app.http.get(format!("{}{dl}?token=bad", app.base)).send().await.unwrap().status(), 401);
    assert_eq!(app.get(t, &format!("/api/artifacts/{}/download", Uuid::new_v4())).await.0, 404);
    assert_eq!(app.get(t, &format!("/api/runs/{}/artifacts", Uuid::new_v4())).await.0, 404);
    app.exec("update artifacts set data = null", &[]).await;
    assert_eq!(app.get(t, &dl).await.0, 404, "db-stored artifact without bytes");
}

// ------------------------------------------------------------------ live view

#[tokio::test(flavor = "multi_thread")]
async fn live_frames_and_input() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Browser").await;
    let li = format!("/api/bots/{bot}/live");

    assert_eq!(app.get(t, &li).await.0, 404);
    assert_eq!(app.get(t, &format!("{li}.jpg")).await.0, 404);
    let jpeg = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9];
    sqlx::query("insert into live_frames (bot_id, owner_id, jpeg, url, title, width, height) values ($1,$2,$3,'https://example.com','Example',800,600)")
        .bind(bot.parse::<Uuid>().unwrap())
        .bind(a.id)
        .bind(&jpeg)
        .execute(&app.pool)
        .await
        .unwrap();
    let (s, v) = app.get(t, &li).await;
    assert_eq!((s, v["url"].as_str(), v["title"].as_str(), v["width"].clone(), v["height"].clone()), (200, Some("https://example.com"), Some("Example"), json!(800), json!(600)));
    assert!(v["updated_at"].is_string());
    for r in [app.http.get(format!("{}{li}.jpg", app.base)).bearer_auth(t), app.http.get(format!("{}{li}.jpg?token={t}", app.base))] {
        let r = r.send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "image/jpeg");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.bytes().await.unwrap().as_ref(), jpeg.as_slice());
    }
    assert_eq!(app.http.get(format!("{}{li}.jpg", app.base)).send().await.unwrap().status(), 401);

    let ip = format!("{li}/input");
    for bad in [
        json!({"type": "wiggle"}),
        json!({"type": "click"}),
        json!({"type": "click", "x": -1, "y": 0}),
        json!({"type": "type"}),
        json!({"type": "type", "text": "x".repeat(2001)}),
        json!({"type": "key", "key": ""}),
        json!({"type": "key", "key": "k".repeat(33)}),
        json!({"type": "scroll"}),
        json!({"type": "navigate", "url": "file:///etc/passwd"}),
        json!({"type": "navigate"}),
    ] {
        assert_eq!(app.post(t, &ip, bad.clone()).await.0, 400, "{bad}");
    }
    assert_eq!(app.post(t, &format!("/api/bots/{}/live/input", Uuid::new_v4()), json!({"type": "key", "key": "a"})).await.0, 404);

    // valid input -> NOTIFY familiar_input {owner, bot, ...}
    let mut l = PgListener::connect(&app.db_url).await.unwrap();
    l.listen("familiar_input").await.unwrap();
    for (body, expect) in [
        (json!({"type": "click", "x": 10, "y": 20}), json!({"type": "click", "x": 10, "y": 20})),
        (json!({"type": "type", "text": "hello"}), json!({"type": "type", "text": "hello"})),
        (json!({"type": "key", "key": "Enter"}), json!({"type": "key", "key": "Enter"})),
        (json!({"type": "scroll", "dy": -120}), json!({"type": "scroll", "dy": -120})),
        (json!({"type": "navigate", "url": "https://example.org"}), json!({"type": "navigate", "url": "https://example.org/"})),
    ] {
        let (s, v) = app.post(t, &ip, body).await;
        assert_eq!((s, v["ok"].clone()), (202, json!(true)));
        let n = tokio::time::timeout(Duration::from_secs(10), l.recv()).await.expect("no notify").unwrap();
        let got: Value = serde_json::from_str(n.payload()).unwrap();
        assert_eq!((got["owner"].as_str(), got["bot"].as_str()), (Some(a.id.to_string().as_str()), Some(bot.as_str())));
        for (k, want) in expect.as_object().unwrap() {
            assert_eq!(&got[k], want, "{got}");
        }
    }
}

// ------------------------------------------------------------------ SSE

/// Read the SSE body into `buf` until `pred(buf)` holds.
async fn read_until(r: &mut reqwest::Response, buf: &mut String, pred: impl Fn(&str) -> bool) {
    let res = tokio::time::timeout(Duration::from_secs(15), async {
        while !pred(buf) {
            let chunk = r.chunk().await.unwrap().expect("stream ended");
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await;
    assert!(res.is_ok(), "timed out; got so far:\n{buf}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_notices_and_deltas_are_owner_filtered() {
    let app = app!();
    let a = app.owner().await;
    let b = app.second().await;
    assert_eq!(app.call(Method::GET, "/api/stream", None, None).await.0, 401);
    assert_eq!(app.http.get(format!("{}/api/stream?token=bad", app.base)).send().await.unwrap().status(), 401);

    let mut sa = app.http.get(format!("{}/api/stream?token={}", app.base, a.tok)).send().await.unwrap();
    assert_eq!(sa.status(), 200);
    assert!(sa.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let mut sb = app.http.get(format!("{}/api/stream", app.base)).bearer_auth(&b.tok).send().await.unwrap();
    assert_eq!(sb.status(), 200);

    // Deltas first: it also proves the server's LISTEN task is up (a NOTIFY before that is lost, so keep sending).
    let (mut abuf, mut bbuf) = (String::new(), String::new());
    let (pool, ida, idb) = (app.pool.clone(), a.id, b.id);
    let pump = tokio::spawn(async move {
        loop {
            for (owner, text) in [(idb, "for-b"), (ida, "for-a")] {
                let payload = json!({"owner": owner, "run": "r1", "kind": "text", "text": text}).to_string();
                let _ = sqlx::query("select pg_notify('familiar_delta', $1)").bind(payload).execute(&pool).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    read_until(&mut sa, &mut abuf, |s| s.contains("event: delta")).await;
    read_until(&mut sb, &mut bbuf, |s| s.contains("event: delta")).await;
    pump.abort();
    assert!(abuf.contains(r#""text":"for-a""#) && !abuf.contains("for-b"), "{abuf}");
    assert!(bbuf.contains(r#""text":"for-b""#) && !bbuf.contains("for-a"), "{bbuf}");
    assert!(abuf.contains(r#""kind":"text""#) && !abuf.contains("owner"), "owner is stripped: {abuf}");

    // a new message surfaces as a notice (runs table, via the DB trigger) for its owner only
    let bot = app.bot(&a.tok, "Streamy").await;
    let (_, run) = app.chat(&a.tok, &bot, "ping").await;
    read_until(&mut sa, &mut abuf, |s| s.contains("event: notice") && s.contains(&id(&run))).await;
    assert!(abuf.contains(r#""t":"runs""#), "{abuf}");
    // B hears about its own change, never about A's
    app.bot(&b.tok, "Bs").await;
    read_until(&mut sb, &mut bbuf, |s| s.contains(r#""t":"bots""#)).await;
    assert!(!bbuf.contains(&id(&run)) && !bbuf.contains(&bot), "leak: {bbuf}");
}

// ------------------------------------------------------------------ owner isolation

#[tokio::test(flavor = "multi_thread")]
async fn second_user_sees_nothing() {
    let app = app!(true);
    let a = app.owner().await;
    let ta = &a.tok;
    let b = app.second().await;
    let tb = &b.tok;
    assert_eq!(app.get(tb, "/api/me").await.1["email"], "b@example.com");

    // everything A owns
    let bot = app.bot(ta, "Private").await;
    let buid: Uuid = bot.parse().unwrap();
    let (thread, run) = app.chat(ta, &bot, "secret plans").await;
    let rid = id(&run);
    app.exec("insert into events (owner_id, run_id, seq, kind) values ($1, $2, 0, 'status')", &[a.id, rid.parse().unwrap()]).await;
    let appr = app.approval(a.id, rid.parse().unwrap(), buid, "Bash").await;
    let sched = id(&app.ok_post(ta, &format!("/api/bots/{bot}/schedules"), json!({"cron": "0 * * * *", "prompt": "p"}), 201).await);
    let mem = id(&app.ok_post(ta, &format!("/api/bots/{bot}/memories"), json!({"content": "m"}), 201).await);
    let rule = id(&app.ok_post(ta, "/api/rules", json!({"bot_id": bot, "pattern": "p", "decision": "ask"}), 201).await);
    app.ok_post(ta, "/api/rules", json!({"pattern": "global", "decision": "ask"}), 201).await;
    let conn = id(&app.ok_post(ta, "/api/connectors", json!({"name": "c1", "transport": "http", "url": "https://x.io", "secrets": {"headers": {"A": "b"}}}), 201).await);
    let trig = id(&app.ok_post(ta, &format!("/api/bots/{bot}/triggers"), json!({"name": "t", "prompt": "p"}), 201).await);
    let art: Uuid = sqlx::query_scalar("insert into artifacts (owner_id, run_id, bot_id, name, storage, data) values ($1,$2,$3,'f','db','x') returning id")
        .bind(a.id)
        .bind(uid(&run))
        .bind(buid)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    sqlx::query("insert into live_frames (bot_id, owner_id, jpeg) values ($1, $2, '\\x00')").bind(buid).bind(a.id).execute(&app.pool).await.unwrap();
    let ch: Uuid = sqlx::query_scalar("insert into channels (owner_id, kind, config_enc) values ($1, 'telegram', 'v1:x') returning id")
        .bind(a.id)
        .fetch_one(&app.pool)
        .await
        .unwrap();

    // B: every addressable thing is a 404
    for (m, p, body) in [
        (Method::GET, format!("/api/bots/{bot}"), None),
        (Method::PATCH, format!("/api/bots/{bot}"), Some(json!({"name": "pwned"}))),
        (Method::DELETE, format!("/api/bots/{bot}"), None),
        (Method::GET, format!("/api/bots/{bot}/threads"), None),
        (Method::POST, format!("/api/bots/{bot}/threads"), Some(json!({}))),
        (Method::PATCH, format!("/api/threads/{thread}"), Some(json!({"title": "x"}))),
        (Method::DELETE, format!("/api/threads/{thread}"), None),
        (Method::GET, format!("/api/threads/{thread}/messages"), None),
        (Method::POST, format!("/api/threads/{thread}/messages"), Some(json!({"content": "hi"}))),
        (Method::GET, format!("/api/threads/{thread}/runs"), None),
        (Method::GET, format!("/api/bots/{bot}/runs"), None),
        (Method::GET, format!("/api/runs/{rid}"), None),
        (Method::GET, format!("/api/runs/{rid}/events"), None),
        (Method::POST, format!("/api/runs/{rid}/cancel"), Some(json!({}))),
        (Method::POST, format!("/api/approvals/{appr}"), Some(json!({"decision": "approve"}))),
        (Method::GET, format!("/api/bots/{bot}/schedules"), None),
        (Method::POST, format!("/api/bots/{bot}/schedules"), Some(json!({"cron": "* * * * *", "prompt": "x"}))),
        (Method::PATCH, format!("/api/schedules/{sched}"), Some(json!({"enabled": false}))),
        (Method::DELETE, format!("/api/schedules/{sched}"), None),
        (Method::GET, format!("/api/bots/{bot}/memories"), None),
        (Method::POST, format!("/api/bots/{bot}/memories"), Some(json!({"content": "x"}))),
        (Method::PATCH, format!("/api/memories/{mem}"), Some(json!({"status": "rejected"}))),
        (Method::DELETE, format!("/api/memories/{mem}"), None),
        (Method::POST, format!("/api/bots/{bot}/dream"), Some(json!({}))),
        (Method::GET, format!("/api/bots/{bot}/live"), None),
        (Method::GET, format!("/api/bots/{bot}/live.jpg"), None),
        (Method::POST, format!("/api/bots/{bot}/live/input"), Some(json!({"type": "key", "key": "a"}))),
        (Method::POST, "/api/rules".into(), Some(json!({"bot_id": bot, "pattern": "p", "decision": "allow"}))),
        (Method::DELETE, format!("/api/rules/{rule}"), None),
        (Method::GET, format!("/api/bots/{bot}/skills"), None),
        (Method::GET, format!("/api/runs/{rid}/artifacts"), None),
        (Method::GET, format!("/api/bots/{bot}/artifacts"), None),
        (Method::GET, format!("/api/artifacts/{art}/download"), None),
        (Method::PATCH, format!("/api/connectors/{conn}"), Some(json!({"enabled": false}))),
        (Method::DELETE, format!("/api/connectors/{conn}"), None),
        (Method::GET, format!("/api/bots/{bot}/connectors"), None),
        (Method::PUT, format!("/api/bots/{bot}/connectors"), Some(json!({"connector_ids": [conn]}))),
        (Method::PATCH, format!("/api/channels/{ch}"), Some(json!({"enabled": false}))),
        (Method::DELETE, format!("/api/channels/{ch}"), None),
        (Method::GET, format!("/api/bots/{bot}/triggers"), None),
        (Method::POST, format!("/api/bots/{bot}/triggers"), Some(json!({"name": "t", "prompt": "p"}))),
        (Method::PATCH, format!("/api/triggers/{trig}"), Some(json!({"enabled": false}))),
        (Method::POST, format!("/api/triggers/{trig}/rotate"), Some(json!({}))),
        (Method::DELETE, format!("/api/triggers/{trig}"), None),
    ] {
        let (s, v) = app.call(m.clone(), &p, Some(tb), body).await;
        assert_eq!(s, 404, "{m} {p} -> {v}");
    }
    // B can't link its own bot to A's connector, nor fetch A's files with its token in the query
    let bbot = app.bot(tb, "Mine").await;
    assert_eq!(app.put(tb, &format!("/api/bots/{bbot}/connectors"), json!({"connector_ids": [conn]})).await.0, 404);
    assert_eq!(app.http.get(format!("{}/api/artifacts/{art}/download?token={tb}", app.base)).send().await.unwrap().status(), 404);
    assert_eq!(app.http.get(format!("{}/api/bots/{bot}/live.jpg?token={tb}", app.base)).send().await.unwrap().status(), 404);
    for p in ["/api/approvals", "/api/connectors", "/api/channels", "/api/rules?all=1", "/api/rules"] {
        assert_eq!(len(&app.get(tb, p).await.1), 0, "{p}");
    }
    let (_, ov) = app.get(tb, "/api/overview").await;
    assert_eq!((len(&ov["bots"]), ov["pending_approvals"].clone()), (1, json!(0)));
    assert_eq!(len(&app.get(tb, "/api/bots").await.1), 1);

    // ... and none of it changed for A
    assert_eq!(app.get(ta, &format!("/api/bots/{bot}")).await.1["name"], "Private");
    assert_eq!(app.get(ta, &format!("/api/runs/{rid}")).await.1["status"], "queued");
    assert_eq!(len(&app.get(ta, &format!("/api/threads/{thread}/messages")).await.1), 1);
    assert_eq!(app.get(ta, "/api/approvals?status=pending").await.1[0]["id"].as_str(), Some(appr.to_string().as_str()));
    assert_eq!(len(&app.get(ta, &format!("/api/bots/{bot}/memories")).await.1), 1);
    assert_eq!(len(&app.get(ta, "/api/connectors").await.1), 1);
    assert_eq!(app.get(ta, &format!("/api/artifacts/{art}/download")).await.0, 200);
    assert_eq!(app.get(ta, &format!("/api/bots/{bot}/live")).await.0, 200);
    assert_eq!(len(&app.get(ta, &format!("/api/bots/{bot}/triggers")).await.1), 1);
}

// ------------------------------------------------------------------ local owner session (native app)

#[tokio::test(flavor = "multi_thread")]
async fn mint_owner_session_only_for_a_single_owner() {
    use familiar_server::mint_owner_session;
    let app = app!();
    assert_eq!(mint_owner_session(&app.pool).await.unwrap(), None, "no user yet");

    let a = app.owner().await;
    let tok = mint_owner_session(&app.pool).await.unwrap().expect("one owner");
    assert_ne!(tok, a.tok);
    let (s, me) = app.get(&tok, "/api/me").await;
    assert_eq!((s, me["id"].as_str()), (200, Some(a.id.to_string().as_str())));
    // same 30-day lifetime as a login session
    let days: f64 = sqlx::query_scalar(
        "select extract(epoch from expires_at - now())::float8 / 86400 from sessions where token_hash = encode(digest($1,'sha256'),'hex')",
    )
    .bind(&tok)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!((29.9..=30.0).contains(&days), "expires in {days} days");

    app.second().await;
    assert_eq!(mint_owner_session(&app.pool).await.unwrap(), None, "two users: no unambiguous owner");
    assert_eq!(app.get(&tok, "/api/me").await.0, 200, "existing sessions are untouched");
}

// ------------------------------------------------------------------ models (from the newest computer's heartbeat)

#[tokio::test(flavor = "multi_thread")]
async fn models_come_from_the_newest_device() {
    let app = app!();
    let a = app.owner().await;
    let t = a.tok.as_str();
    let (s, v) = app.get(t, "/api/models").await;
    assert_eq!(s, 200);
    assert_eq!(v, json!({"claude": {"plan": null, "models": []}, "codex": {"models": []}}), "nothing reported yet");

    let old = json!({"models": {"claude": {"plan": "pro", "models": [{"id": "sonnet", "label": "Sonnet 5.5", "alias": true, "available": true}]}}});
    let new = json!({"utilization": 0.1, "models": {
        "claude": {"plan": "max", "models": [{"id": "claude-opus-5-5", "label": "Opus 5.5", "alias": false, "available": true}]},
        "codex": {"models": [{"id": "gpt-5-codex", "label": "GPT-5-Codex"}]}}});
    for (name, info, ago) in [("old", old, "2 hours"), ("new", new, "1 minute"), ("none", json!({"utilization": 0.2}), "0 seconds")] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into devices (id, owner_id, name, version, last_seen_at, info) values ($1, $2, $3, 'test', now() - interval '{ago}', $4)"
        )))
        .bind(Uuid::new_v4())
        .bind(a.id)
        .bind(name)
        .bind(sqlx::types::Json(info))
        .execute(&app.pool)
        .await
        .unwrap();
    }
    let (_, v) = app.get(t, "/api/models").await;
    assert_eq!(v["claude"]["plan"], "max", "the newest device that reported models wins");
    assert_eq!(v["claude"]["models"][0]["id"], "claude-opus-5-5");
    assert_eq!(v["codex"]["models"][0]["id"], "gpt-5-codex");
    let b = app.second().await;
    assert_eq!(app.get(&b.tok, "/api/models").await.1["claude"]["models"], json!([]), "owner-scoped");
}

// ------------------------------------------------------------------ CRM

/// A CSV body (raw text, not JSON) to the import endpoint.
async fn csv_post(app: &App, t: &str, path: &str, body: Vec<u8>) -> (u16, Value) {
    let r = app.http.post(format!("{}{path}", app.base)).bearer_auth(t).header("content-type", "text/csv").body(body).send().await.unwrap();
    let s = r.status().as_u16();
    (s, serde_json::from_slice(&r.bytes().await.unwrap()).unwrap_or(Value::Null))
}

async fn csv_get(app: &App, t: &str, kind: &str) -> String {
    let r = app.http.get(format!("{}/api/crm/export.csv?kind={kind}", app.base)).bearer_auth(t).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/csv"));
    r.text().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_crud_and_dedupe() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;

    // companies: the same domain twice is one company (the second post updates it)
    let c = app.ok_post(t, "/api/crm/companies", json!({"name": "Acme", "website": "https://www.Acme.com/pricing"}), 201).await;
    assert_eq!((c["domain"].as_str(), c["name"].as_str()), (Some("acme.com"), Some("Acme")));
    assert!(c.get("owner_id").is_none());
    let cid = id(&c);
    let c2 = app.ok_post(t, "/api/crm/companies", json!({"name": "Acme Inc", "domain": "ACME.com", "industry": "Rockets"}), 200).await;
    assert_eq!((id(&c2), c2["name"].as_str(), c2["industry"].as_str()), (cid.clone(), Some("Acme Inc"), Some("Rockets")));
    assert_eq!(len(&app.get(t, "/api/crm/companies").await.1), 1);
    // no domain: matched by exact name (any case)
    let n1 = app.ok_post(t, "/api/crm/companies", json!({"name": "Nameless Co"}), 201).await;
    assert_eq!(id(&app.ok_post(t, "/api/crm/companies", json!({"name": "nameless co", "size": "10"}), 200).await), id(&n1));
    assert_eq!(len(&app.get(t, "/api/crm/companies").await.1), 2);
    // validation
    for bad in [
        json!({}), json!({"name": ""}), json!({"name": "x", "domain": "not a domain"}), json!({"name": "x", "website": "ftp://x.com"}),
        json!({"name": "x", "fit_score": 101}), json!({"name": "x", "tags": (0..21).map(|i| format!("t{i}")).collect::<Vec<_>>()}),
        json!({"name": "x", "source_urls": ["javascript:1"]}), json!({"name": "x".repeat(201)}), json!({"name": "x", "custom": [1]}),
        json!({"name": 5}),
    ] {
        assert_eq!(app.post(t, "/api/crm/companies", bad.clone()).await.0, 400, "{bad}");
    }
    // get / patch
    let (s, v) = app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"fit_score": 80, "fit_reason": "ships weekly", "tags": ["ICP", "icp", " b2b "]})).await;
    assert_eq!((s, v["fit_score"].clone(), v["tags"].clone()), (200, json!(80), json!(["ICP", "b2b"])));
    assert_eq!(app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"fit_score": -1})).await.0, 400);
    assert_eq!(app.patch(t, &format!("/api/crm/companies/{}", Uuid::new_v4()), json!({"name": "x"})).await.0, 404);
    assert_eq!(app.get(t, "/api/crm/companies/not-a-uuid").await.0, 400);
    assert_eq!(app.get(t, "/api/crm/nothing").await.0, 404);
    assert_eq!(app.get(t, &format!("/api/crm/activities/{}", Uuid::new_v4())).await.0, 404);
    let (_, v) = app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"industry": ""})).await;
    assert_eq!(v["industry"], Value::Null, "an empty text clears the field");
    // list: search, tag, sort, paging
    assert_eq!(len(&app.get(t, "/api/crm/companies?q=acme").await.1), 1);
    assert_eq!(len(&app.get(t, "/api/crm/companies?q=zzz").await.1), 0);
    assert_eq!(len(&app.get(t, "/api/crm/companies?tag=B2B").await.1), 1);
    let (_, l) = app.get(t, "/api/crm/companies?sort=name").await;
    // (the upsert that matched by name also set the name as it was written)
    assert_eq!((l[0]["name"].as_str(), l[1]["name"].as_str()), (Some("Acme Inc"), Some("nameless co")));
    let (_, l) = app.get(t, "/api/crm/companies?sort=fit&limit=1").await;
    assert_eq!((len(&l), id(&l[0])), (1, cid.clone()));
    assert_eq!(len(&app.get(t, "/api/crm/companies?limit=1&offset=1").await.1), 1);
    assert_eq!(app.get(t, "/api/crm/companies?sort=nope").await.0, 400);
    // a number: absent leaves it, null clears it, a value sets it
    let (s, v) = app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"location": "Berlin"})).await;
    assert_eq!((s, v["fit_score"].clone()), (200, json!(80)));
    let (s, v) = app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"fit_score": null})).await;
    assert_eq!((s, v["fit_score"].clone(), v["location"].as_str()), (200, Value::Null, Some("Berlin")));
    assert_eq!(app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"fit_score": "high"})).await.0, 400);
    let (s, v) = app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"fit_score": 80})).await;
    assert_eq!((s, v["fit_score"].clone()), (200, json!(80)));

    // contacts: the same email in another case is one contact; rows carry the company's name
    let p = app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam", "email": " Sam@Acme.COM ", "company_id": cid, "x_handle": "@sam"}), 201).await;
    assert_eq!((p["email"].as_str(), p["company_name"].as_str(), p["x_handle"].as_str()), (Some("sam@acme.com"), Some("Acme Inc"), Some("sam")));
    let pid = id(&p);
    let p2 = app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam Smith", "email": "SAM@acme.com", "title": "CTO"}), 200).await;
    assert_eq!((id(&p2), p2["title"].as_str(), p2["name"].as_str()), (pid.clone(), Some("CTO"), Some("Sam Smith")));
    assert_eq!(len(&app.get(t, "/api/crm/contacts").await.1), 1);
    // no email: matched by name + company
    let q1 = app.ok_post(t, "/api/crm/contacts", json!({"name": "Pat", "company_id": cid}), 201).await;
    assert_eq!(id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "pat", "company_id": cid, "title": "VP"}), 200).await), id(&q1));
    assert_ne!(id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Pat"}), 201).await), id(&q1), "no company = another contact");
    for bad in [json!({"name": "x", "email": "nope"}), json!({"name": "x", "linkedin_url": "acme.com"}), json!({"email": "a@b.io"}), json!({"name": "x", "company_id": Uuid::new_v4()})] {
        assert_eq!(app.post(t, "/api/crm/contacts", bad.clone()).await.0, 400, "{bad}");
    }
    assert_eq!(len(&app.get(t, &format!("/api/crm/contacts?company_id={cid}")).await.1), 2);
    assert_eq!(len(&app.get(t, "/api/crm/contacts?q=acme").await.1), 2, "the search covers the company name");
    assert_eq!(len(&app.get(t, "/api/crm/contacts?dnc=true").await.1), 0);
    assert_eq!(app.patch(t, &format!("/api/crm/contacts/{pid}"), json!({"email": "pat@acme.com"})).await.0, 200);
    let other = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Other", "email": "other@acme.com"}), 201).await);
    assert_eq!(app.patch(t, &format!("/api/crm/contacts/{other}"), json!({"email": "PAT@acme.com"})).await.0, 409, "email taken");

    // deals
    let d = app.ok_post(t, "/api/crm/deals", json!({"company_id": cid, "contact_id": pid, "title": "Pilot", "value_cents": 5000}), 201).await;
    assert_eq!((d["stage"].as_str(), d["currency"].as_str(), d["company_name"].as_str(), d["contact_name"].as_str()), (Some("new"), Some("USD"), Some("Acme Inc"), Some("Sam Smith")));
    let did = id(&d);
    assert_eq!(id(&app.ok_post(t, "/api/crm/deals", json!({"company_id": cid, "title": "pilot", "currency": "eur"}), 200).await), did);
    for bad in [
        json!({"company_id": cid}), json!({"title": "x"}), json!({"company_id": cid, "title": "x", "stage": "nope"}),
        json!({"company_id": cid, "title": "x", "value_cents": -1}), json!({"company_id": cid, "title": "x", "currency": "dollars"}),
        json!({"company_id": Uuid::new_v4(), "title": "x"}), json!({"company_id": cid, "title": "x", "next_step": "y".repeat(501)}),
    ] {
        assert_eq!(app.post(t, "/api/crm/deals", bad.clone()).await.0, 400, "{bad}");
    }
    let (s, v) = app.patch(t, &format!("/api/crm/deals/{did}"), json!({"stage": "meeting", "next_step": "demo", "next_step_at": "2026-11-01T10:00:00Z"})).await;
    assert_eq!((s, v["stage"].as_str(), v["next_step"].as_str()), (200, Some("meeting"), Some("demo")));
    assert_eq!(len(&app.get(t, "/api/crm/deals?stage=meeting").await.1), 1);
    assert_eq!(len(&app.get(t, "/api/crm/deals?stage=won").await.1), 0);
    assert_eq!(app.get(t, "/api/crm/deals?stage=nope").await.0, 400);
    // the value and the next step's date: absent leaves them, null clears them, values set them
    let (s, v) = app.patch(t, &format!("/api/crm/deals/{did}"), json!({"next_step": "demo on Monday"})).await;
    assert_eq!((s, v["value_cents"].clone(), v["next_step_at"].is_string()), (200, json!(5000), true));
    let (s, v) = app.patch(t, &format!("/api/crm/deals/{did}"), json!({"value_cents": null, "next_step_at": null})).await;
    assert_eq!((s, v["value_cents"].clone(), v["next_step_at"].clone(), v["next_step"].as_str()), (200, Value::Null, Value::Null, Some("demo on Monday")));
    assert_eq!(app.patch(t, &format!("/api/crm/deals/{did}"), json!({"value_cents": -5})).await.0, 400);
    assert_eq!(app.patch(t, &format!("/api/crm/deals/{did}"), json!({"next_step_at": "next week"})).await.0, 400);
    let (s, v) = app.patch(t, &format!("/api/crm/deals/{did}"), json!({"value_cents": 5000, "next_step_at": "2026-11-01T10:00:00Z"})).await;
    assert_eq!((s, v["value_cents"].clone(), v["next_step_at"].is_string()), (200, json!(5000), true));
    // each clear is a change the owner can undo
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity=deal&entity_id={did}")).await;
    assert!(ch.as_array().unwrap().iter().any(|c| c["before"]["value_cents"] == 5000 && c["after"]["value_cents"].is_null()), "{ch}");

    // activities: a deal's timeline entry also shows on its company and contact
    let act = app.ok_post(t, "/api/crm/activities", json!({"deal_id": did, "kind": "note", "summary": "Called", "body": "went well", "url": "https://x.io/1"}), 201).await;
    assert_eq!((act["company_id"].as_str(), act["actor_kind"].as_str()), (Some(cid.as_str()), Some("user")));
    // (with the stage_change the move to meeting above logged)
    assert_eq!(len(&app.get(t, &format!("/api/crm/activities?company_id={cid}")).await.1), 2);
    assert_eq!(len(&app.get(t, &format!("/api/crm/activities?deal_id={did}")).await.1), 2);
    assert_eq!(len(&app.get(t, &format!("/api/crm/activities?contact_id={}", Uuid::new_v4())).await.1), 0);
    for bad in [
        json!({"kind": "note", "summary": "x"}), json!({"deal_id": did, "kind": "sms", "summary": "x"}), json!({"deal_id": did, "kind": "note"}),
        json!({"deal_id": did, "kind": "note", "summary": "x".repeat(501)}), json!({"deal_id": did, "kind": "note", "summary": "x", "body": "y".repeat(20_001)}),
        json!({"deal_id": did, "kind": "note", "summary": "x", "url": "ftp://a"}), json!({"deal_id": Uuid::new_v4(), "kind": "note", "summary": "x"}),
        json!({"deal_id": did, "kind": "note", "summary": "x", "approval_id": Uuid::new_v4()}),
    ] {
        assert_eq!(app.post(t, "/api/crm/activities", bad.clone()).await.0, 400, "{bad}");
    }
    // bodies are size-limited (the shared `Body` extractor answers an over-long one as a 400)
    assert_eq!(app.post(t, "/api/crm/companies", json!({"name": "x", "description": "d".repeat(300_000)})).await.0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_soft_delete_hides_and_frees_keys() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let c = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Acme", "domain": "acme.com"}), 201).await);
    let p = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam", "email": "sam@acme.com", "company_id": c}), 201).await);
    let d = id(&app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "Pilot"}), 201).await);

    assert_eq!(app.del(t, &format!("/api/crm/contacts/{p}")).await.0, 204);
    assert_eq!(len(&app.get(t, "/api/crm/contacts").await.1), 0);
    assert_eq!(app.get(t, &format!("/api/crm/contacts/{p}")).await.0, 404);
    assert_eq!(app.del(t, &format!("/api/crm/contacts/{p}")).await.0, 404);
    assert_eq!(app.patch(t, &format!("/api/crm/contacts/{p}"), json!({"title": "x"})).await.0, 404);
    // the email is free again, and the old row stays in the table
    let p2 = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam again", "email": "sam@acme.com"}), 201).await);
    assert_ne!(p, p2);
    let gone: bool = sqlx::query_scalar("select deleted_at is not null from crm_contacts where id = $1").bind(p.parse::<Uuid>().unwrap()).fetch_one(&app.pool).await.unwrap();
    assert!(gone);

    // deleting a company hides its deals with it; the domain is free again
    assert_eq!(len(&app.get(t, "/api/crm/deals").await.1), 1);
    assert_eq!(app.del(t, &format!("/api/crm/companies/{c}")).await.0, 204);
    assert_eq!(len(&app.get(t, "/api/crm/companies").await.1), 0);
    assert_eq!(len(&app.get(t, "/api/crm/deals").await.1), 0);
    assert_eq!(app.get(t, &format!("/api/crm/deals/{d}")).await.0, 404);
    let pipe = app.get(t, "/api/crm/pipeline").await.1;
    assert!(pipe.as_array().unwrap().iter().all(|s| s["count"] == 0));
    let c2 = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Acme again", "domain": "acme.com"}), 201).await);
    assert_ne!(c, c2);
    // a deal can't be put on a deleted company
    assert_eq!(app.post(t, "/api/crm/deals", json!({"company_id": c, "title": "x"})).await.0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_pipeline_counts_and_value() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let c = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Acme"}), 201).await);
    let p = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam", "company_id": c}), 201).await);
    let d1 = id(&app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "A", "value_cents": 1000, "contact_id": p, "next_step": "email"}), 201).await);
    app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "B", "value_cents": 2500}), 201).await;
    app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "C", "stage": "meeting", "value_cents": 4000}), 201).await;
    app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "D", "stage": "meeting"}), 201).await;

    let (s, pipe) = app.get(t, "/api/crm/pipeline").await;
    assert_eq!(s, 200);
    let stages: Vec<&str> = pipe.as_array().unwrap().iter().map(|s| s["stage"].as_str().unwrap()).collect();
    assert_eq!(stages, ["new", "researching", "contacted", "replied", "meeting", "proposal", "won", "lost"]);
    let by = |v: &Value, st: &str| v.as_array().unwrap().iter().find(|s| s["stage"] == st).unwrap().clone();
    let new = by(&pipe, "new");
    assert_eq!((new["count"].clone(), new["value_cents"].clone(), len(&new["deals"])), (json!(2), json!(3500), 2));
    let meeting = by(&pipe, "meeting");
    assert_eq!((meeting["count"].clone(), meeting["value_cents"].clone()), (json!(2), json!(4000)));
    let card = new["deals"].as_array().unwrap().iter().find(|d| id(d) == d1).unwrap();
    assert_eq!((card["title"].as_str(), card["company_name"].as_str(), card["contact_name"].as_str(), card["next_step"].as_str()), (Some("A"), Some("Acme"), Some("Sam"), Some("email")));
    assert_eq!(by(&pipe, "won")["count"], 0);

    // moving a deal changes both stages, and its stage_changed_at
    let before = app.get(t, &format!("/api/crm/deals/{d1}")).await.1["stage_changed_at"].clone();
    app.patch(t, &format!("/api/crm/deals/{d1}"), json!({"stage": "won"})).await;
    let pipe = app.get(t, "/api/crm/pipeline").await.1;
    assert_eq!((by(&pipe, "new")["count"].clone(), by(&pipe, "new")["value_cents"].clone()), (json!(1), json!(2500)));
    assert_eq!((by(&pipe, "won")["count"].clone(), by(&pipe, "won")["value_cents"].clone()), (json!(1), json!(1000)));
    assert_ne!(app.get(t, &format!("/api/crm/deals/{d1}")).await.1["stage_changed_at"], before);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_changes_and_undo() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let c = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Acme", "domain": "acme.com"}), 201).await);
    app.patch(t, &format!("/api/crm/companies/{c}"), json!({"name": "Acme Corp"})).await;
    // an upsert that changes nothing writes nothing
    app.ok_post(t, "/api/crm/companies", json!({"name": "Acme Corp", "domain": "acme.com"}), 200).await;

    let (s, ch) = app.get(t, &format!("/api/crm/changes?entity=company&entity_id={c}")).await;
    assert_eq!(s, 200);
    assert_eq!(len(&ch), 2);
    assert_eq!((ch[0]["op"].as_str(), ch[1]["op"].as_str()), (Some("update"), Some("create")));
    assert_eq!((ch[0]["before"]["name"].as_str(), ch[0]["after"]["name"].as_str()), (Some("Acme"), Some("Acme Corp")));
    assert_eq!((ch[0]["actor_kind"].as_str(), ch[1]["before"].clone()), (Some("user"), Value::Null));
    assert_eq!(app.get(t, "/api/crm/changes?entity=nope").await.0, 400);
    let (update, create) = (id(&ch[0]), id(&ch[1]));

    // only the newest change can be undone
    assert_eq!(app.post(t, &format!("/api/crm/changes/{create}/undo"), json!({})).await.0, 409);
    let u = app.ok_post(t, &format!("/api/crm/changes/{update}/undo"), json!({}), 200).await;
    assert_eq!((u["op"].as_str(), u["entity_id"].as_str()), (Some("undo"), Some(c.as_str())));
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c}")).await.1["name"], "Acme");
    assert_eq!(app.post(t, &format!("/api/crm/changes/{update}/undo"), json!({})).await.0, 409, "already undone");
    assert_eq!(app.post(t, &format!("/api/crm/changes/{}/undo", id(&u)), json!({})).await.0, 409, "an undo is not undone");
    assert_eq!(app.post(t, &format!("/api/crm/changes/{}/undo", Uuid::new_v4()), json!({})).await.0, 404);
    assert_eq!(len(&app.get(t, &format!("/api/crm/changes?entity_id={c}")).await.1), 3);
    // ... and now the create is the newest: undoing it removes the company
    app.ok_post(t, &format!("/api/crm/changes/{create}/undo"), json!({}), 200).await;
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c}")).await.0, 404);

    // delete -> undo brings it back
    let c2 = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Beta", "domain": "beta.io"}), 201).await);
    assert_eq!(app.del(t, &format!("/api/crm/companies/{c2}")).await.0, 204);
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity_id={c2}&limit=1")).await;
    assert_eq!(ch[0]["op"], "delete");
    app.ok_post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({}), 200).await;
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c2}")).await.1["domain"], "beta.io");
    // delete again, take the domain with a new company: the undo can't restore it (409), nothing changed
    assert_eq!(app.del(t, &format!("/api/crm/companies/{c2}")).await.0, 204);
    app.ok_post(t, "/api/crm/companies", json!({"name": "Beta 2", "domain": "beta.io"}), 201).await;
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity_id={c2}&limit=1")).await;
    assert_eq!(app.post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({})).await.0, 409);
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c2}")).await.0, 404);

    // undo of a contact update restores every field it replaced; of a deal create hides the deal
    let p = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam", "email": "sam@x.io", "tags": ["a"]}), 201).await);
    app.patch(t, &format!("/api/crm/contacts/{p}"), json!({"name": "Samuel", "tags": ["b"], "title": "CEO"})).await;
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity=contact&entity_id={p}&limit=1")).await;
    app.ok_post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({}), 200).await;
    let r = app.get(t, &format!("/api/crm/contacts/{p}")).await.1;
    assert_eq!((r["name"].clone(), r["tags"].clone(), r["title"].clone()), (json!("Sam"), json!(["a"]), Value::Null));
    let c3 = id(&app.ok_post(t, "/api/crm/companies", json!({"name": "Gamma"}), 201).await);
    let d = id(&app.ok_post(t, "/api/crm/deals", json!({"company_id": c3, "title": "Big"}), 201).await);
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity=deal&entity_id={d}")).await;
    app.ok_post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({}), 200).await;
    assert_eq!(app.get(t, &format!("/api/crm/deals/{d}")).await.0, 404);
    // timeline entries are not undone
    let act = app.ok_post(t, "/api/crm/activities", json!({"company_id": c3, "kind": "note", "summary": "x"}), 201).await;
    let (_, ch) = app.get(t, &format!("/api/crm/changes?entity=activity&entity_id={}", id(&act))).await;
    assert_eq!(len(&ch), 1);
    assert_eq!(app.post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({})).await.0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_teammate_writes_and_do_not_contact() {
    use familiar_core::{
        crm::{self, ActivityFilters, Actor, ChangeFilters, CompanyInput, ContactInput, DealInput},
        db::Db,
    };
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Scout").await;
    let (_, run) = app.chat(t, &bot, "go").await;
    let run_id = id(&run);
    let db = Db { pool: app.pool.clone(), owner: a.id };
    let who = Actor::Bot { bot: bot.parse().unwrap(), run: Some(uid(&run)) };

    let (co, created) = crm::upsert_company(&db, &who, &CompanyInput { name: Some("Acme".into()), domain: Some("https://acme.com".into()), source_urls: Some(vec!["https://acme.com/about".into()]), ..Default::default() }).await.unwrap();
    assert!(created);
    assert_eq!(co["created_by_bot"].as_str(), Some(bot.as_str()));
    let cid = uid(&co);
    let (p, _) = crm::upsert_contact(&db, &who, &ContactInput { name: Some("Sam".into()), email: Some("sam@acme.com".into()), company_id: Some(cid), source_urls: Some(vec!["https://acme.com/team".into()]), ..Default::default() }).await.unwrap();
    let pid = uid(&p);
    // the change log says who (and in which run)
    let ch = crm::changes(&db, &ChangeFilters { bot_id: Some(bot.parse().unwrap()), ..Default::default() }, None).await.unwrap();
    assert_eq!(ch.len(), 2);
    assert_eq!((ch[0]["actor_kind"].as_str(), ch[0]["bot_name"].as_str(), ch[0]["run_id"].as_str()), (Some("bot"), Some("Scout"), Some(run_id.as_str())));

    // anyone can set do-not-contact; only the owner can clear it
    let dnc = |v: bool| ContactInput { do_not_contact: Some(v), dnc_reason: v.then(|| "asked to stop".to_string()), ..Default::default() };
    let r = crm::patch_contact(&db, &who, pid, &dnc(true)).await.unwrap();
    assert_eq!((r["do_not_contact"].clone(), r["dnc_reason"].as_str(), r["dnc_at"].is_string()), (json!(true), Some("asked to stop"), true));
    let err = crm::patch_contact(&db, &who, pid, &dnc(false)).await.unwrap_err();
    assert!(matches!(err, crm::CrmError::Forbidden(_)), "{err:?}");
    let err = crm::upsert_contact(&db, &who, &ContactInput { email: Some("sam@acme.com".into()), do_not_contact: Some(false), ..Default::default() }).await.unwrap_err();
    assert!(matches!(err, crm::CrmError::Forbidden(_)), "{err:?}");
    assert_eq!(app.get(t, &format!("/api/crm/contacts/{pid}")).await.1["do_not_contact"], true);
    assert_eq!(len(&app.get(t, "/api/crm/contacts?dnc=true").await.1), 1);
    let (s, v) = app.patch(t, &format!("/api/crm/contacts/{pid}"), json!({"do_not_contact": false})).await;
    assert_eq!((s, v["do_not_contact"].clone(), v["dnc_at"].clone()), (200, json!(false), Value::Null));

    // moving a deal logs a stage_change activity by the teammate
    let (d, _) = crm::upsert_deal(&db, &who, &DealInput { company_id: Some(cid), contact_id: Some(pid), title: Some("Pilot".into()), ..Default::default() }).await.unwrap();
    let did = uid(&d);
    let moved = crm::move_deal(&db, &who, did, "contacted", Some("sent the intro")).await.unwrap();
    assert_eq!(moved["stage"], "contacted");
    let of_deal = ActivityFilters { deal_id: Some(did), ..Default::default() };
    let acts = crm::activities(&db, &of_deal, None, None).await.unwrap();
    assert_eq!(acts.len(), 1);
    assert_eq!((acts[0]["kind"].as_str(), acts[0]["body"].as_str(), acts[0]["actor_kind"].as_str(), acts[0]["bot_name"].as_str()), (Some("stage_change"), Some("sent the intro"), Some("bot"), Some("Scout")));
    assert_eq!((acts[0]["company_id"].as_str(), acts[0]["contact_id"].as_str()), (Some(cid.to_string().as_str()), Some(pid.to_string().as_str())));
    // the same stage again does nothing
    crm::move_deal(&db, &who, did, "contacted", None).await.unwrap();
    assert_eq!(crm::activities(&db, &of_deal, None, None).await.unwrap().len(), 1);
    assert!(matches!(crm::move_deal(&db, &who, did, "limbo", None).await, Err(crm::CrmError::Invalid(_))));
    assert!(matches!(crm::move_deal(&db, &who, Uuid::new_v4(), "won", None).await, Err(crm::CrmError::NotFound)));
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_csv_export_import() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let evil = "=HYPERLINK(\"http://evil\",\"x\")";
    let c = id(&app.ok_post(t, "/api/crm/companies", json!({"name": evil, "domain": "acme.com", "description": "line one\nline, two \"quoted\"", "fit_score": 70, "tags": ["a", "b"], "source_urls": ["https://x.io/1", "https://x.io/2"]}), 201).await);
    app.ok_post(t, "/api/crm/companies", json!({"name": "Beta"}), 201).await;
    let p = id(&app.ok_post(t, "/api/crm/contacts", json!({"name": "+Sam", "email": "sam@acme.com", "company_id": c, "notes": "@home", "do_not_contact": true, "dnc_reason": "said stop"}), 201).await);
    app.ok_post(t, "/api/crm/contacts", json!({"name": "Pat"}), 201).await;
    app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "contact_id": p, "title": "-Pilot", "value_cents": 1234, "stage": "meeting", "next_step_at": "2026-11-01T10:00:00Z"}), 201).await;
    app.ok_post(t, "/api/crm/deals", json!({"company_id": c, "title": "Second"}), 201).await;

    for kind in ["companies", "contacts", "deals"] {
        let csv = csv_get(&app, t, kind).await;
        assert!(csv.starts_with("name,") || csv.starts_with("title,"), "{csv}");
        // formulas are neutralised
        for line in csv.split("\r\n") {
            assert!(!line.starts_with(['=', '+', '@']), "{line}");
        }
        // an export reads back as a dry run that creates nothing, changes nothing
        let (s, r) = csv_post(&app, t, &format!("/api/crm/import?kind={kind}&dry_run=true"), csv.clone().into_bytes()).await;
        assert_eq!(s, 200, "{kind}: {r}");
        assert_eq!((r["created"].clone(), r["updated"].clone(), r["skipped"].clone(), r["errors"].clone()), (json!(0), json!(0), json!(2), json!([])), "{kind}: {r}");
        // ... and so does the real thing
        let (_, r) = csv_post(&app, t, &format!("/api/crm/import?kind={kind}"), csv.into_bytes()).await;
        assert_eq!((r["created"].clone(), r["updated"].clone()), (json!(0), json!(0)), "{kind}: {r}");
    }
    assert!(csv_get(&app, t, "companies").await.contains("'=HYPERLINK("));
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c}")).await.1["name"], evil);
    assert_eq!(len(&app.get(t, "/api/crm/changes?limit=200").await.1), 6, "an import that changes nothing writes nothing");

    // a mixed file: one new, one update, one unchanged, one blank, two bad rows; a dry run first
    let csv = "Name,Domain,Fit Score,Tags,ignored\r\nNew Co,new.io,50,x; y,zzz\r\nAcme Renamed,acme.com,71,,\r\nBeta,,,,\r\nBad,bad.io,lots,,\r\n,,,,\r\nNot A Domain,nope,,,\r\n";
    let (s, r) = csv_post(&app, t, "/api/crm/import?kind=companies&dry_run=true", csv.as_bytes().to_vec()).await;
    assert_eq!((s, r["created"].clone(), r["updated"].clone(), r["skipped"].clone()), (200, json!(1), json!(1), json!(2)), "{r}");
    let errs: Vec<(i64, String)> = r["errors"].as_array().unwrap().iter().map(|e| (e["row"].as_i64().unwrap(), e["message"].as_str().unwrap().to_string())).collect();
    assert_eq!(errs.iter().map(|e| e.0).collect::<Vec<_>>(), [5, 7]);
    assert!(errs[0].1.contains("fit_score"), "{errs:?}");
    assert_eq!(len(&app.get(t, "/api/crm/companies").await.1), 2, "a dry run changes nothing");
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c}")).await.1["fit_score"], 70);
    let (_, r) = csv_post(&app, t, "/api/crm/import?kind=companies&dry_run=false", csv.as_bytes().to_vec()).await;
    assert_eq!((r["created"].clone(), r["updated"].clone(), r["skipped"].clone(), len(&r["errors"])), (json!(1), json!(1), json!(2), 2));
    assert_eq!(len(&app.get(t, "/api/crm/companies").await.1), 3);
    assert_eq!(app.get(t, &format!("/api/crm/companies/{c}")).await.1["fit_score"], 71);
    assert_eq!(app.get(t, "/api/crm/companies?q=new.io").await.1[0]["tags"], json!(["x", "y"]));

    // contacts and deals name their company; one that doesn't exist yet is created
    let csv = "name,email,company,company_domain\r\nZed,zed@omega.io,Omega,omega.io\r\nSam Again,sam@acme.com,,acme.com\r\n";
    let (_, r) = csv_post(&app, t, "/api/crm/import?kind=contacts", csv.as_bytes().to_vec()).await;
    assert_eq!((r["created"].clone(), r["updated"].clone(), len(&r["errors"])), (json!(1), json!(1), 0), "{r}");
    assert_eq!(app.get(t, "/api/crm/companies?q=omega").await.1[0]["domain"], "omega.io");
    let csv = "title,company,stage,value_cents,contact_email\r\nNew deal,Omega,proposal,900,zed@omega.io\r\nBad stage,Omega,limbo,,\r\nNo contact,Omega,,,ghost@omega.io\r\n";
    let (_, r) = csv_post(&app, t, "/api/crm/import?kind=deals", csv.as_bytes().to_vec()).await;
    assert_eq!((r["created"].clone(), len(&r["errors"])), (json!(1), 2), "{r}");
    assert_eq!(app.get(t, "/api/crm/deals?stage=proposal").await.1[0]["contact_name"], "Zed");

    // limits and bad files
    let big = format!("name\r\n{}", "a\r\n".repeat(5001));
    assert_eq!(csv_post(&app, t, "/api/crm/import?kind=companies&dry_run=true", big.into_bytes()).await.0, 400);
    let ok = format!("name\r\n{}", "b\r\n".repeat(5000));
    let (s, r) = csv_post(&app, t, "/api/crm/import?kind=companies&dry_run=true", ok.into_bytes()).await;
    assert_eq!((s, r["created"].clone()), (200, json!(1)), "5000 rows is fine (all the same company here)");
    assert_eq!(csv_post(&app, t, "/api/crm/import?kind=companies", format!("name\r\n{}", "x".repeat(5 * 1024 * 1024 + 10)).into_bytes()).await.0, 400);
    for (q, body) in [
        ("kind=companies", b"".to_vec()), ("kind=companies", b"domain\r\nacme.com\r\n".to_vec()), ("kind=deals", b"title\r\nx\r\n".to_vec()),
        ("kind=companies", b"name\r\n\"oops\r\n".to_vec()), ("kind=companies", vec![0xff, 0xfe, 0x41]), ("kind=nope", b"name\r\n".to_vec()),
        ("kind=activities", b"name\r\n".to_vec()), ("", b"name\r\n".to_vec()),
    ] {
        assert_eq!(csv_post(&app, t, &format!("/api/crm/import?{q}"), body.clone()).await.0, 400, "{q} {}", String::from_utf8_lossy(&body));
    }
    assert_eq!(app.http.get(format!("{}/api/crm/export.csv?kind=nope", app.base)).bearer_auth(t).send().await.unwrap().status(), 400);
    assert_eq!(app.http.get(format!("{}/api/crm/export.csv?kind=companies", app.base)).send().await.unwrap().status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_second_user_sees_nothing() {
    let app = app!();
    let a = app.owner().await;
    let ta = &a.tok;
    let b = app.second().await;
    let tb = &b.tok;

    let c = id(&app.ok_post(ta, "/api/crm/companies", json!({"name": "Private Co", "domain": "private.io"}), 201).await);
    let p = id(&app.ok_post(ta, "/api/crm/contacts", json!({"name": "Sam", "email": "sam@private.io", "company_id": c}), 201).await);
    let d = id(&app.ok_post(ta, "/api/crm/deals", json!({"company_id": c, "contact_id": p, "title": "Secret", "value_cents": 99}), 201).await);
    let act = id(&app.ok_post(ta, "/api/crm/activities", json!({"deal_id": d, "kind": "note", "summary": "hush"}), 201).await);
    let (_, ch) = app.get(ta, &format!("/api/crm/changes?entity_id={c}")).await;
    let change = id(&ch[0]);

    for path in ["/api/crm/companies", "/api/crm/contacts", "/api/crm/deals", "/api/crm/activities", "/api/crm/changes"] {
        assert_eq!(len(&app.get(tb, path).await.1), 0, "{path}");
    }
    for q in [format!("/api/crm/activities?deal_id={d}"), format!("/api/crm/activities?company_id={c}"), format!("/api/crm/changes?entity_id={c}"), "/api/crm/companies?q=private".into()] {
        assert_eq!(len(&app.get(tb, &q).await.1), 0, "{q}");
    }
    let pipe = app.get(tb, "/api/crm/pipeline").await.1;
    assert!(pipe.as_array().unwrap().iter().all(|s| s["count"] == 0 && s["value_cents"] == 0 && len(&s["deals"]) == 0));
    for (m, path, body) in [
        (Method::GET, format!("/api/crm/companies/{c}"), None),
        (Method::PATCH, format!("/api/crm/companies/{c}"), Some(json!({"name": "pwned"}))),
        (Method::DELETE, format!("/api/crm/companies/{c}"), None),
        (Method::GET, format!("/api/crm/contacts/{p}"), None),
        (Method::PATCH, format!("/api/crm/contacts/{p}"), Some(json!({"do_not_contact": true}))),
        (Method::DELETE, format!("/api/crm/contacts/{p}"), None),
        (Method::GET, format!("/api/crm/deals/{d}"), None),
        (Method::PATCH, format!("/api/crm/deals/{d}"), Some(json!({"stage": "won"}))),
        (Method::DELETE, format!("/api/crm/deals/{d}"), None),
        (Method::POST, format!("/api/crm/changes/{change}/undo"), Some(json!({}))),
    ] {
        let (s, v) = app.call(m.clone(), &path, Some(tb), body).await;
        assert_eq!(s, 404, "{m} {path} -> {v}");
    }
    // B can't hang its own records on A's
    assert_eq!(app.post(tb, "/api/crm/contacts", json!({"name": "x", "company_id": c})).await.0, 400);
    assert_eq!(app.post(tb, "/api/crm/deals", json!({"title": "x", "company_id": c})).await.0, 400);
    assert_eq!(app.post(tb, "/api/crm/activities", json!({"kind": "note", "summary": "x", "deal_id": d})).await.0, 400);
    assert_eq!(app.post(tb, "/api/crm/activities", json!({"kind": "note", "summary": "x", "company_id": c})).await.0, 400);
    // the same domain and email are free for B; B's export has none of A's data; A's export, imported by B, matches B's own
    app.ok_post(tb, "/api/crm/companies", json!({"name": "Mine", "domain": "private.io"}), 201).await;
    app.ok_post(tb, "/api/crm/contacts", json!({"name": "Sam B", "email": "sam@private.io"}), 201).await;
    assert!(!csv_get(&app, tb, "companies").await.contains("Private Co"));
    let theirs = csv_get(&app, ta, "contacts").await;
    let (_, r) = csv_post(&app, tb, "/api/crm/import?kind=contacts&dry_run=true", theirs.into_bytes()).await;
    assert_eq!((r["created"].clone(), r["updated"].clone()), (json!(0), json!(1)), "{r}");

    // ... and none of it changed for A
    assert_eq!(app.get(ta, &format!("/api/crm/companies/{c}")).await.1["name"], "Private Co");
    assert_eq!(app.get(ta, &format!("/api/crm/contacts/{p}")).await.1["do_not_contact"], false);
    assert_eq!(app.get(ta, &format!("/api/crm/deals/{d}")).await.1["stage"], "new");
    assert_eq!(len(&app.get(ta, &format!("/api/crm/activities?company_id={c}")).await.1), 1);
    assert_eq!(id(&app.get(ta, &format!("/api/crm/activities?deal_id={d}")).await.1[0]), act);
}

// ------------------------------------------------------------------ CRM: teammates' trust rules

#[tokio::test(flavor = "multi_thread")]
async fn crm_teammate_owner_edits_win_and_limits() {
    use familiar_core::{
        crm::{self, Actor, CompanyInput, ContactInput, DealInput, Outcome},
        db::Db,
    };
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let bot = app.bot(t, "Scout").await;
    let (_, run) = app.chat(t, &bot, "go").await;
    let db = Db { pool: app.pool.clone(), owner: a.id };
    let who = Actor::Bot { bot: bot.parse().unwrap(), run: Some(uid(&run)) };
    let src = |u: &str| Some(vec![u.to_string()]);

    // a teammate's new company or contact needs sources
    let err = crm::upsert_company(&db, &who, &CompanyInput { name: Some("Nosource".into()), ..Default::default() }).await.unwrap_err();
    assert!(matches!(&err, crm::CrmError::Invalid(m) if m.contains("source_urls")), "{err:?}");

    // the owner adds a company with a name and a description
    let co = app.ok_post(t, "/api/crm/companies", json!({"name": "Acme", "domain": "acme.com", "description": "Owner's words", "tags": ["vip"]}), 201).await;
    let cid = uid(&co);
    let write = |i: CompanyInput| {
        let (db, who) = (db.clone(), who.clone());
        async move {
            let mut tx = db.pool.begin().await.unwrap();
            let s = crm::write_company_in(&mut tx, db.owner, &who, None, &i).await.unwrap();
            tx.commit().await.unwrap();
            s
        }
    };
    // a teammate: the owner's name and description stay; empty fields are filled; fit is the teammate's; tags add up
    let s = write(CompanyInput {
        domain: Some("acme.com".into()),
        name: Some("ACME Corp".into()),
        description: Some("Bot's words".into()),
        industry: Some("SaaS".into()),
        fit_score: Some(Some(85)),
        fit_reason: Some("Raised a round".into()),
        tags: Some(vec!["fintech".into()]),
        source_urls: src("https://news.example/acme"),
        ..Default::default()
    })
    .await;
    assert_eq!((s.outcome, s.kept.clone()), (Outcome::Updated, vec!["description".to_string(), "name".to_string()]));
    let row = app.get(t, &format!("/api/crm/companies/{cid}")).await.1;
    assert_eq!((row["name"].as_str(), row["description"].as_str(), row["industry"].as_str()), (Some("Acme"), Some("Owner's words"), Some("SaaS")));
    assert_eq!(
        (row["fit_score"].as_i64(), row["tags"].clone(), row["source_urls"].clone()),
        (Some(85), json!(["vip", "fintech"]), json!(["https://news.example/acme"]))
    );
    // the teammate may change what a teammate set...
    let s = write(CompanyInput { domain: Some("acme.com".into()), industry: Some("Fintech".into()), ..Default::default() }).await;
    assert!(s.kept.is_empty() && s.outcome == Outcome::Updated);
    // ...until the owner sets it: then the owner's value wins
    app.patch(t, &format!("/api/crm/companies/{cid}"), json!({"industry": "Payments"})).await;
    let s = write(CompanyInput { domain: Some("acme.com".into()), industry: Some("Banking".into()), ..Default::default() }).await;
    assert_eq!((s.outcome, s.kept), (Outcome::Unchanged, vec!["industry".to_string()]));
    // an owner undo of a teammate change counts as the owner's choice
    let s = write(CompanyInput { domain: Some("acme.com".into()), location: Some("Berlin".into()), ..Default::default() }).await;
    assert_eq!(s.outcome, Outcome::Updated);
    let ch = app.get(t, &format!("/api/crm/changes?entity=company&entity_id={cid}&limit=1")).await.1;
    app.ok_post(t, &format!("/api/crm/changes/{}/undo", id(&ch[0])), json!({}), 200).await;
    let s = write(CompanyInput { domain: Some("acme.com".into()), location: Some("Paris".into()), ..Default::default() }).await;
    assert_eq!(s.kept, vec!["location".to_string()]);

    // a deal the owner closed stays closed; the teammate can still keep next_step
    let deal = app.ok_post(t, "/api/crm/deals", json!({"company_id": cid, "title": "Pilot", "stage": "won"}), 201).await;
    let did = uid(&deal);
    let mut tx = db.pool.begin().await.unwrap();
    let s = crm::move_deal_in(&mut tx, a.id, &who, did, "contacted", Some("follow up")).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!((s.outcome, s.kept), (Outcome::Unchanged, vec!["stage".to_string()]));
    crm::patch_deal(&db, &who, did, &DealInput { next_step: Some("send the invoice".into()), ..Default::default() }).await.unwrap();
    let d = app.get(t, &format!("/api/crm/deals/{did}")).await.1;
    assert_eq!(
        (d["stage"].as_str(), d["next_step"].as_str(), d["contact_do_not_contact"].clone()),
        (Some("won"), Some("send the invoice"), json!(false))
    );
    // clearing: a teammate never empties a value the owner set, not even the next step's date it otherwise keeps up;
    // through the tool, only with an explicit `clear` (a null is "leave it")
    app.patch(t, &format!("/api/crm/deals/{did}"), json!({"value_cents": 900, "next_step_at": "2026-11-01T10:00:00Z"})).await;
    let tool_args = |v: Value| serde_json::from_value::<familiar_core::crm::teammate::DealArgs>(v).unwrap();
    let out = familiar_core::crm::teammate::upsert_deal(&db, &who, tool_args(json!({ "id": did, "value_cents": null, "next_step_at": null })))
        .await
        .unwrap();
    assert!(out.contains("\"changed\":false"), "{out}");
    let out = familiar_core::crm::teammate::upsert_deal(&db, &who, tool_args(json!({ "id": did, "clear": ["value_cents", "next_step_at"] })))
        .await
        .unwrap();
    assert!(out.contains("\"kept_owner_values\":[\"next_step_at\",\"value_cents\"]"), "{out}");
    let d = app.get(t, &format!("/api/crm/deals/{did}")).await.1;
    assert_eq!((d["value_cents"].clone(), d["next_step_at"].is_string()), (json!(900), true));
    // a date the teammate set itself, it may clear
    familiar_core::crm::teammate::upsert_deal(&db, &who, tool_args(json!({ "id": did, "next_step_at": "2026-11-08T10:00:00Z" })))
        .await
        .unwrap();
    familiar_core::crm::teammate::upsert_deal(&db, &who, tool_args(json!({ "id": did, "clear": ["next_step_at"] })))
        .await
        .unwrap();
    let d = app.get(t, &format!("/api/crm/deals/{did}")).await.1;
    assert_eq!(d["next_step_at"].clone(), Value::Null);
    let err = familiar_core::crm::teammate::upsert_deal(&db, &who, tool_args(json!({ "id": did, "clear": ["next_step_at"], "next_step_at": "2026-12-01T10:00:00Z" })))
        .await
        .unwrap_err();
    assert!(err.contains("both set and cleared"), "{err}");
    let company_args = serde_json::from_value::<familiar_core::crm::teammate::CompanyArgs>(json!({ "id": cid, "clear": ["fit_score"] })).unwrap();
    familiar_core::crm::teammate::upsert_company(&db, &who, company_args).await.unwrap();
    assert_eq!(app.get(t, &format!("/api/crm/companies/{cid}")).await.1["fit_score"], Value::Null);

    // do-not-contact: found by email, X handle or LinkedIn, even after the contact is deleted
    let (p, _) = crm::upsert_contact(
        &db,
        &who,
        &ContactInput {
            name: Some("Sam".into()),
            email: Some("sam@acme.com".into()),
            x_handle: Some("samacme".into()),
            company_id: Some(cid),
            linkedin_url: Some("https://www.linkedin.com/in/sam-acme/".into()),
            source_urls: src("https://acme.com/team"),
            do_not_contact: Some(true),
            dnc_reason: Some("replied STOP".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for to in ["Sam <SAM@acme.com>", "@SamAcme", "https://linkedin.com/in/sam-acme"] {
        assert_eq!(crm::teammate::do_not_contact(&db, to).await.unwrap(), Some(uid(&p)), "{to}");
    }
    assert_eq!(crm::teammate::do_not_contact(&db, "kim@acme.com").await.unwrap(), None);
    app.del(t, &format!("/api/crm/contacts/{}", id(&p))).await;
    assert_eq!(crm::teammate::do_not_contact(&db, "sam@acme.com").await.unwrap(), Some(uid(&p)));
    let stranger = Db { pool: app.pool.clone(), owner: Uuid::new_v4() };
    assert_eq!(crm::teammate::do_not_contact(&stranger, "sam@acme.com").await.unwrap(), None, "owner-scoped");

    // a runaway run stops at the write budget, with a clear message; another run carries on
    sqlx::query(
        "insert into crm_changes (owner_id, entity, entity_id, op, actor_kind, bot_id, run_id)
         select $1, 'company', $2, 'update', 'bot', $3, $4 from generate_series(1, $5)",
    )
    .bind(a.id)
    .bind(cid)
    .bind(Uuid::parse_str(&bot).unwrap())
    .bind(uid(&run))
    .bind(crm::MAX_WRITES_PER_RUN as i32)
    .execute(&app.pool)
    .await
    .unwrap();
    let err = crm::upsert_company(&db, &who, &CompanyInput { domain: Some("acme.com".into()), size: Some("50".into()), ..Default::default() })
        .await
        .unwrap_err();
    assert!(matches!(&err, crm::CrmError::Invalid(m) if m.contains("200 CRM changes")), "{err:?}");
    let act = crm::ActivityInput { company_id: Some(cid), kind: Some("note".into()), summary: Some("x".into()), ..Default::default() };
    assert!(crm::log_activity(&db, &who, &act).await.is_err());
    crm::log_activity(&db, &Actor::User, &act).await.unwrap();
    let (_, run2) = app.chat(t, &bot, "again").await;
    let next = Actor::Bot { bot: bot.parse().unwrap(), run: Some(uid(&run2)) };
    crm::log_activity(&db, &next, &act).await.unwrap();
}

// ------------------------------------------------------------------ CRM webhooks

type Got = tokio::sync::mpsc::UnboundedReceiver<(String, axum::http::HeaderMap, Vec<u8>)>;

/// A webhook receiver on this computer: answers 200, except 500 on /fail, 410 on /gone and a redirect on /moved; hands every request on.
async fn receiver() -> (String, Got) {
    use axum::{
        body::Bytes,
        http::{HeaderMap, StatusCode, Uri, header},
        response::IntoResponse,
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = axum::Router::new().fallback(move |uri: Uri, h: HeaderMap, body: Bytes| {
        let tx = tx.clone();
        async move {
            let path = uri.path().to_string();
            let _ = tx.send((path.clone(), h, body.to_vec()));
            match path.as_str() {
                "/fail" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                "/gone" => StatusCode::GONE.into_response(),
                "/moved" => (StatusCode::FOUND, [(header::LOCATION, "http://127.0.0.1:1/elsewhere")]).into_response(),
                _ => StatusCode::OK.into_response(),
            }
        }
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (base, rx)
}

/// What a receiver does to check a delivery: `t=<unix>,v1=<hex HMAC-SHA256("<t>.<body>")>` with the secret.
fn verify(secret: &str, h: &axum::http::HeaderMap, body: &[u8]) -> bool {
    let sig = h["familiar-signature"].to_str().unwrap();
    let (t, v1) = sig.split_once(",v1=").unwrap();
    let t: i64 = t.strip_prefix("t=").unwrap().parse().unwrap();
    (chrono::Utc::now().timestamp() - t).abs() < 300 && familiar_core::crm::webhooks::sign(secret, t, body) == v1
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_webhooks_owner_only_and_secret_once() {
    // without a secret key nothing can be signed: 503
    {
        let app = app!();
        let a = app.owner().await;
        let (s, _) = app.post(&a.tok, "/api/crm/webhooks", json!({"url": "http://127.0.0.1:9/x", "events": ["deal.created"]})).await;
        assert_eq!(s, 503);
    }
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;
    let hook = json!({"url": "http://127.0.0.1:9/x", "events": ["deal.created"]});
    assert_eq!(app.call(Method::GET, "/api/crm/webhooks", None, None).await.0, 401);
    assert_eq!(app.call(Method::POST, "/api/crm/webhooks", None, Some(hook.clone())).await.0, 401);
    for (url, events) in [
        ("http://8.8.8.8/hook", json!(["deal.created"])),
        ("https://10.0.0.5/hook", json!(["deal.created"])),
        ("https://169.254.169.254/latest/meta-data", json!(["deal.created"])),
        ("https://[fe80::1]/", json!(["deal.created"])),
        ("https://[::ffff:192.168.1.1]/", json!(["deal.created"])),
        ("https://no-such-host.invalid/", json!(["deal.created"])),
        ("https://user:pw@8.8.8.8/", json!(["deal.created"])),
        ("ftp://8.8.8.8/", json!(["deal.created"])),
        ("http://127.0.0.1:9/x", json!([])),
        ("http://127.0.0.1:9/x", json!(["deal.deleted"])),
    ] {
        let (s, v) = app.post(t, "/api/crm/webhooks", json!({"url": url, "events": events})).await;
        assert_eq!(s, 400, "{url} {events}: {v}");
    }
    let w = app
        .ok_post(t, "/api/crm/webhooks", json!({"url": "http://127.0.0.1:9/x", "events": ["deal.created", "deal.created", "contact.do_not_contact"]}), 201)
        .await;
    let wid = id(&w);
    let secret = w["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("whsec_") && secret.len() == 70);
    assert_eq!(
        (w["events"].clone(), w["enabled"].clone(), w["last_delivery"].clone()),
        (json!(["deal.created", "contact.do_not_contact"]), json!(true), Value::Null)
    );
    assert!(w.get("secret_enc").is_none() && w.get("owner_id").is_none());
    // a public https address is fine too (no DNS for an address)
    assert_eq!(app.post(t, "/api/crm/webhooks", json!({"url": "https://93.184.215.14/hook", "events": ["deal.created"]})).await.0, 201);
    // the secret is shown once: never in a list, a read or an update, and it is stored sealed
    let (_, list) = app.get(t, "/api/crm/webhooks").await;
    assert_eq!(len(&list), 2);
    let (_, one) = app.get(t, &format!("/api/crm/webhooks/{wid}")).await;
    let (s, up) = app.patch(t, &format!("/api/crm/webhooks/{wid}"), json!({"events": ["deal.stage_changed"], "url": "http://localhost:9/y"})).await;
    assert_eq!(s, 200, "{up}");
    for v in [&list[0], &list[1], &one, &up] {
        assert!(v.get("secret").is_none() && v.get("secret_enc").is_none(), "{v}");
    }
    assert_eq!((up["events"].clone(), up["url"].as_str()), (json!(["deal.stage_changed"]), Some("http://localhost:9/y")));
    assert_eq!(app.patch(t, &format!("/api/crm/webhooks/{wid}"), json!({"url": "https://192.168.0.10/"})).await.0, 400);
    let sealed: String = sqlx::query_scalar("select secret_enc from crm_webhooks where id = $1").bind(uid(&w)).fetch_one(&app.pool).await.unwrap();
    assert!(sealed.starts_with("v1:") && !sealed.contains(&secret[6..]));

    // owner only: another account sees and touches nothing
    let b = app.second().await;
    assert_eq!(len(&app.get(&b.tok, "/api/crm/webhooks").await.1), 0);
    for (m, p) in [
        (Method::GET, format!("/api/crm/webhooks/{wid}")),
        (Method::PATCH, format!("/api/crm/webhooks/{wid}")),
        (Method::DELETE, format!("/api/crm/webhooks/{wid}")),
        (Method::POST, format!("/api/crm/webhooks/{wid}/test")),
        (Method::GET, format!("/api/crm/webhooks/{wid}/deliveries")),
    ] {
        assert_eq!(app.call(m.clone(), &p, Some(&b.tok), Some(json!({"enabled": false}))).await.0, 404, "{m} {p}");
    }
    // a change in the other account's CRM never reaches this account's webhooks
    app.ok_post(&b.tok, "/api/crm/companies", json!({"name": "Other"}), 201).await;
    app.ok_post(&b.tok, "/api/crm/deals", json!({"company_id": id(&app.get(&b.tok, "/api/crm/companies").await.1[0]), "title": "D"}), 201).await;
    let n: i64 = sqlx::query_scalar("select count(*) from crm_webhook_deliveries").fetch_one(&app.pool).await.unwrap();
    assert_eq!(n, 0);

    assert_eq!(app.del(t, &format!("/api/crm/webhooks/{wid}")).await.0, 204);
    assert_eq!(app.get(t, &format!("/api/crm/webhooks/{wid}")).await.0, 404);
    // at most 20 per owner
    for i in 0..19 {
        let (s, v) = app.post(t, "/api/crm/webhooks", json!({"url": format!("http://127.0.0.1:9/{i}"), "events": ["deal.created"]})).await;
        assert_eq!(s, 201, "{v}");
    }
    assert_eq!(app.post(t, "/api/crm/webhooks", hook).await.0, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_webhook_test_delivery_is_signed() {
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;
    let (base, mut got) = receiver().await;
    let w = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/hook"), "events": ["company.created"]}), 201).await;
    let secret = w["secret"].as_str().unwrap();
    let d = app.ok_post(t, &format!("/api/crm/webhooks/{}/test", id(&w)), json!({}), 200).await;
    assert_eq!(
        (d["status"].as_str(), d["event"].as_str(), d["attempts"].as_i64(), d["last_error"].clone()),
        (Some("delivered"), Some("ping"), Some(1), Value::Null)
    );
    let (path, h, body) = got.recv().await.unwrap();
    assert_eq!(path, "/hook");
    assert_eq!(h["familiar-event"], "ping");
    assert_eq!(h["familiar-delivery"].to_str().unwrap(), id(&d));
    assert_eq!(h["content-type"], "application/json");
    assert!(verify(secret, &h, &body), "the signature checks out");
    assert!(!verify("whsec_wrong", &h, &body));
    let mut tampered = body.clone();
    tampered.push(b' ');
    assert!(!verify(secret, &h, &tampered));
    let p: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((p["id"].as_str(), p["event"].as_str()), (Some(id(&d).as_str()), Some("ping")));

    // a failing receiver and a redirect: failed, with the reason; the redirect isn't followed
    for (path, why) in [("fail", "HTTP 500"), ("moved", "redirects aren't followed")] {
        let w2 = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/{path}"), "events": ["company.created"]}), 201).await;
        let d = app.ok_post(t, &format!("/api/crm/webhooks/{}/test", id(&w2)), json!({}), 200).await;
        assert_eq!(d["status"], "failed", "{d}");
        assert!(d["last_error"].as_str().unwrap().contains(why), "{d}");
        assert_eq!(got.recv().await.unwrap().0, format!("/{path}"));
        let (_, all) = app.get(t, &format!("/api/crm/webhooks/{}/deliveries", id(&w2))).await;
        assert_eq!((len(&all), all[0]["payload"]["event"].as_str()), (1, Some("ping")));
    }
    let (_, listed) = app.get(t, &format!("/api/crm/webhooks/{}", id(&w))).await;
    assert_eq!(listed["last_delivery"]["status"], "delivered");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(got.try_recv().is_err(), "the redirect was not followed");
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_webhook_deliveries_retry_then_fail() {
    use familiar_core::{
        crm::{self, Actor, CompanyInput, webhooks},
        db::Db,
    };
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;
    let db = Db { pool: app.pool.clone(), owner: a.id };
    let sb = familiar_crypto::SecretBox::from_base64(KEY).unwrap();
    let (base, mut got) = receiver().await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://127.0.0.1:{}/hook", closed.local_addr().unwrap().port());
    drop(closed);
    let down = app.ok_post(t, "/api/crm/webhooks", json!({"url": dead, "events": ["company.created"]}), 201).await;
    let up = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/hook"), "events": ["company.created", "company.updated"]}), 201).await;
    let deals_only = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/deals"), "events": ["deal.created"]}), 201).await;
    let off = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/off"), "events": ["company.created"], "enabled": false}), 201).await;
    let count = |w: &Value| {
        let (pool, w) = (app.pool.clone(), uid(w));
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from crm_webhook_deliveries where webhook_id = $1").bind(w).fetch_one(&pool).await.unwrap()
        }
    };

    // a teammate adds a company: one delivery per matching, enabled webhook, in the same transaction
    let bot = app.bot(t, "Scout").await;
    let who = Actor::Bot { bot: bot.parse().unwrap(), run: None };
    let input = CompanyInput { name: Some("Acme".into()), domain: Some("acme.com".into()), source_urls: Some(vec!["https://acme.com".into()]), ..Default::default() };
    let (co, _) = crm::upsert_company(&db, &who, &input).await.unwrap();
    assert_eq!((count(&down).await, count(&up).await, count(&deals_only).await, count(&off).await), (1, 1, 0, 0));
    // a write that fails leaves no delivery behind
    assert!(crm::upsert_company(&db, &who, &CompanyInput { name: Some("No".into()), ..Default::default() }).await.is_err());
    assert_eq!(count(&up).await, 1);

    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 2);
    let (path, h, body) = got.recv().await.unwrap();
    assert_eq!((path.as_str(), h["familiar-event"].to_str().unwrap()), ("/hook", "company.created"));
    assert!(verify(up["secret"].as_str().unwrap(), &h, &body));
    let p: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((p["event"].as_str(), p["data"]["id"].clone(), p["data"]["name"].as_str()), (Some("company.created"), co["id"].clone(), Some("Acme")));
    assert_eq!(
        (p["actor"]["kind"].as_str(), p["actor"]["bot_slug"].as_str(), p["actor"]["bot_id"].as_str()),
        (Some("bot"), Some("scout"), Some(bot.as_str()))
    );
    assert_eq!(p["id"].as_str(), Some(h["familiar-delivery"].to_str().unwrap()));
    assert!(p["at"].is_string() && p.get("previous").is_none() && p["data"].get("owner_id").is_none(), "{p}");
    let (_, ups) = app.get(t, &format!("/api/crm/webhooks/{}/deliveries", id(&up))).await;
    assert_eq!((ups[0]["status"].as_str(), ups[0]["attempts"].as_i64()), (Some("delivered"), Some(1)));

    // the dead receiver: retried after 1 m, 5 m, 30 m, 2 h and 6 h, then failed
    let state = || {
        let (pool, w) = (app.pool.clone(), uid(&down));
        async move {
            sqlx::query_as::<_, (String, i32, f64, Option<String>)>(
                "select status, attempts, extract(epoch from next_attempt_at - now())::float8, last_error
                 from crm_webhook_deliveries where webhook_id = $1",
            )
            .bind(w)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    for (attempt, wait) in [(1, 60.0), (2, 300.0), (3, 1800.0), (4, 7200.0), (5, 21600.0)] {
        let (status, attempts, left, err) = state().await;
        assert_eq!((status.as_str(), attempts), ("pending", attempt), "after attempt {attempt}");
        assert!((left - wait).abs() < 30.0, "attempt {attempt}: next in {left} s, want {wait}");
        assert!(err.as_deref().is_some_and(|e| e.contains("connect")), "{err:?}");
        // not due yet: nothing is sent
        assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 0);
        sqlx::query("update crm_webhook_deliveries set next_attempt_at = now() where webhook_id = $1")
            .bind(uid(&down))
            .execute(&app.pool)
            .await
            .unwrap();
        assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 1);
    }
    let (status, attempts, _, _) = state().await;
    assert_eq!((status.as_str(), attempts), ("failed", 6));
    sqlx::query("update crm_webhook_deliveries set next_attempt_at = now() - interval '1 day'").execute(&app.pool).await.unwrap();
    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 0, "failed stays failed");

    // turning a webhook off fails what still waits for it
    app.patch(t, &format!("/api/crm/companies/{}", id(&co)), json!({"industry": "SaaS"})).await;
    assert_eq!(count(&up).await, 2);
    let (_, w) = app.patch(t, &format!("/api/crm/webhooks/{}", id(&up)), json!({"enabled": false})).await;
    assert_eq!(w["last_delivery"]["status"], "failed");
    assert_eq!(w["last_delivery"]["last_error"], "the webhook was turned off");
    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 0);
}

// ------------------------------------------------------------------ the GTM crew

#[tokio::test(flavor = "multi_thread")]
async fn gtm_crew_bundle() {
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    assert_eq!(app.call(Method::GET, "/api/templates/bundles", None, None).await.0, 401);
    let (s, bundles) = app.get(t, "/api/templates/bundles").await;
    assert_eq!(s, 200);
    let gtm = bundles.as_array().unwrap().iter().find(|b| b["id"] == "gtm-crew").unwrap().clone();
    let members: Vec<&str> = gtm["templates"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert_eq!(members, ["lead-researcher", "outbound-drafter", "reply-follow-up-tracker", "weekly-metrics-reporter", "inbox-assistant"]);
    assert_eq!(app.post(t, "/api/templates/bundles/nope/create", json!({})).await.0, 404);
    // all or nothing: a bad answer hires nobody
    assert_eq!(app.post(t, "/api/templates/bundles/gtm-crew/create", json!({"answers": {"product": "x".repeat(4001)}})).await.0, 400);
    assert_eq!(len(&app.get(t, "/api/bots").await.1), 0);

    let answers = json!({"product": "Familiar", "icp": "two-person startups", "offer": "a 20-minute call", "sender": "Sam",
                         "voice": "plain", "follow_up_days": "4"});
    let v = app.ok_post(t, "/api/templates/bundles/gtm-crew/create", json!({"answers": answers}), 201).await;
    let hired = v["hired"].as_array().unwrap();
    assert_eq!(hired.len(), 5);
    assert_eq!(len(&app.get(t, "/api/bots").await.1), 5);
    for (h, template) in hired.iter().zip(&members) {
        let bot = &h["bot"];
        assert_eq!(bot["setup"]["template"].as_str(), Some(*template));
        let persona = bot["persona"].as_str().unwrap();
        assert!(!persona.contains("{{"), "{template}: {persona}");
        let (_, scheds) = app.get(t, &format!("/api/bots/{}/schedules", id(bot))).await;
        assert!(len(&scheds) >= 1 && scheds.as_array().unwrap().iter().all(|s| s["enabled"] == false), "{template}: every schedule off");
    }
    let tracker = &hired[2]["bot"];
    let persona = tracker["persona"].as_str().unwrap();
    assert!(persona.contains("outreach for Familiar") && persona.contains("for 4 days") && persona.contains("as Sam"), "{persona}");
    assert!(hired[2]["first_task"].as_str().unwrap().contains("after 4 days"));
    let (_, threads) = app.get(t, &format!("/api/bots/{}/threads", id(tracker))).await;
    assert!(threads.as_array().unwrap().iter().any(|t| t["title"] == "Check replies"));
    // a question the bundle doesn't ask stays "not set yet", for the teammate to ask
    assert!(hired[3]["bot"]["persona"].as_str().unwrap().contains("(not set yet)"));
    // hiring the crew again gets fresh slugs
    let again = app.ok_post(t, "/api/templates/bundles/gtm-crew/create", json!({"answers": answers}), 201).await;
    let slug = again["hired"][0]["bot"]["slug"].as_str().unwrap();
    assert!(slug.starts_with("lead-researcher-"), "{slug}");
    let b = app.second().await;
    assert_eq!(len(&app.get(&b.tok, "/api/bots").await.1), 0);
}

/// Do-not-contact survives the ways around it: an old address (from the change log) still counts after the owner
/// changes it, a re-imported old export never lifts it, a teammate adding the person under a new address finds the same
/// record (and can't change who they are), and the board shows the flag. Any stage move goes on the timeline.
#[tokio::test(flavor = "multi_thread")]
async fn crm_do_not_contact_survives_edits_and_imports() {
    use familiar_core::{
        crm::{self, Actor, ContactInput, teammate},
        db::Db,
    };
    let app = app!();
    let a = app.owner().await;
    let t = &a.tok;
    let db = Db { pool: app.pool.clone(), owner: a.id };
    let co = app.ok_post(t, "/api/crm/companies", json!({"name": "Acme", "domain": "acme.com"}), 201).await;
    let p = app.ok_post(t, "/api/crm/contacts", json!({"name": "Sam", "email": "sam@acme.com", "company_id": id(&co)}), 201).await;
    let pid = id(&p);
    // an export taken before the opt-out
    let old_export = csv_get(&app, t, "contacts").await;
    app.patch(t, &format!("/api/crm/contacts/{pid}"), json!({"do_not_contact": true, "dnc_reason": "asked to stop"})).await;
    let deal = app.ok_post(t, "/api/crm/deals", json!({"company_id": id(&co), "contact_id": pid, "title": "Pilot"}), 201).await;

    // re-importing the old export (do_not_contact = false, no reason) changes nothing about it
    let (s, r) = csv_post(&app, t, "/api/crm/import?kind=contacts&dry_run=false", old_export.into_bytes()).await;
    assert_eq!(s, 200, "{r}");
    let now = app.get(t, &format!("/api/crm/contacts/{pid}")).await.1;
    assert_eq!((now["do_not_contact"].clone(), now["dnc_reason"].as_str()), (json!(true), Some("asked to stop")));
    assert!(now["dnc_at"].is_string());
    // an import can still add one
    let (s, r) = csv_post(&app, t, "/api/crm/import?kind=contacts&dry_run=false", b"name,email,do_not_contact\r\nKim,kim@acme.com,true\r\n".to_vec()).await;
    assert_eq!((s, r["created"].as_i64()), (200, Some(1)), "{r}");
    assert!(teammate::do_not_contact(&db, "kim@acme.com").await.unwrap().is_some());

    // the owner corrects Sam's address: the old one still counts (the change log keeps it)
    app.patch(t, &format!("/api/crm/contacts/{pid}"), json!({"email": "samuel@acme.com"})).await;
    for addr in ["samuel@acme.com", "sam@acme.com", "SAM+x@acme.com"] {
        assert_eq!(teammate::do_not_contact(&db, addr).await.unwrap(), Some(uid(&p)), "{addr}");
    }

    // a teammate adding "Sam at Acme" under a new address finds Sam, and can't change who Sam is
    let bot = app.bot(t, "Scout").await;
    let who = Actor::Bot { bot: bot.parse().unwrap(), run: None };
    let mut tx = app.pool.begin().await.unwrap();
    let s = crm::write_contact_in(&mut tx, a.id, &who, None, &ContactInput {
        name: Some("Sam".into()), email: Some("sam.other@acme.com".into()), company_id: Some(uid(&co)),
        source_urls: Some(vec!["https://acme.com/team".into()]), ..Default::default()
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((s.row["id"].as_str(), s.kept.clone()), (Some(pid.as_str()), vec!["email".to_string()]));
    assert_eq!(app.get(t, &format!("/api/crm/contacts/{pid}")).await.1["email"], "samuel@acme.com");

    // the board shows the flag on the deal
    let board = app.get(t, "/api/crm/pipeline").await.1;
    let new = board.as_array().unwrap().iter().find(|s| s["stage"] == "new").unwrap();
    assert_eq!(new["deals"][0]["contact_do_not_contact"], true);
    assert_eq!(app.get(t, &format!("/api/crm/deals/{}", id(&deal))).await.1["contact_do_not_contact"], true);

    // a stage move through a plain update goes on the timeline too (once)
    app.patch(t, &format!("/api/crm/deals/{}", id(&deal)), json!({"stage": "lost"})).await;
    app.patch(t, &format!("/api/crm/deals/{}", id(&deal)), json!({"stage": "lost", "next_step": "none"})).await;
    let acts = app.get(t, &format!("/api/crm/activities?deal_id={}", id(&deal))).await.1;
    assert_eq!(len(&acts), 1);
    assert_eq!((acts[0]["kind"].as_str(), acts[0]["summary"].as_str()), (Some("stage_change"), Some("Stage: new -> lost")));
}

#[tokio::test(flavor = "multi_thread")]
async fn crm_webhook_refusals_are_not_retried() {
    use familiar_core::{
        crm::{self, Actor, CompanyInput, webhooks},
        db::Db,
    };
    let app = app!(true);
    let a = app.owner().await;
    let t = &a.tok;
    let db = Db { pool: app.pool.clone(), owner: a.id };
    let sb = familiar_crypto::SecretBox::from_base64(KEY).unwrap();
    let (base, mut got) = receiver().await;
    let gone = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/gone"), "events": ["company.created"]}), 201).await;
    crm::upsert_company(&db, &Actor::User, &CompanyInput { name: Some("Acme".into()), ..Default::default() }).await.unwrap();
    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 1);
    assert_eq!(got.recv().await.unwrap().0, "/gone");
    let (_, d) = app.get(t, &format!("/api/crm/webhooks/{}/deliveries", id(&gone))).await;
    assert_eq!((d[0]["status"].as_str(), d[0]["attempts"].as_i64()), (Some("failed"), Some(1)), "{d}");
    assert!(d[0]["last_error"].as_str().unwrap().contains("HTTP 410 (not retried"), "{d}");
    // a secret this key can't open fails at once too
    let other = app.ok_post(t, "/api/crm/webhooks", json!({"url": format!("{base}/hook"), "events": ["company.created"]}), 201).await;
    sqlx::query("update crm_webhooks set secret_enc = 'v1:AAAA' where id = $1").bind(uid(&other)).execute(&app.pool).await.unwrap();
    crm::upsert_company(&db, &Actor::User, &CompanyInput { name: Some("Beta".into()), ..Default::default() }).await.unwrap();
    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 2);
    let (_, d) = app.get(t, &format!("/api/crm/webhooks/{}/deliveries", id(&other))).await;
    assert_eq!((d[0]["status"].as_str(), d[0]["attempts"].as_i64()), (Some("failed"), Some(1)), "{d}");
    assert!(d[0]["last_error"].as_str().unwrap().contains("secret"), "{d}");
    // a Test is stored already settled: never pending, never picked up later
    let ping = app.ok_post(t, &format!("/api/crm/webhooks/{}/test", id(&gone)), json!({}), 200).await;
    assert_eq!((ping["status"].as_str(), ping["attempts"].as_i64()), (Some("failed"), Some(1)));
    sqlx::query("update crm_webhook_deliveries set next_attempt_at = now() - interval '1 day'").execute(&app.pool).await.unwrap();
    assert_eq!(webhooks::deliver_due(&db, &sb).await.unwrap(), 0);
}
