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
