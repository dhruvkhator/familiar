//! End-to-end tests of the Familiar daemon (`familiar_core::run`) against a real Postgres and the scripted
//! `fake-claude` binary: no real Claude/Codex, no subscription, deterministic.
//!
//! Needs `TEST_DATABASE_URL` (a Postgres the tests may create databases on, e.g.
//! `postgres://postgres:pw@localhost:55450/postgres`); without it every test is skipped. Each test gets its own fresh
//! database, bots_dir and daemon, so tests run in parallel.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const FAKE_CLAUDE: &str = env!("CARGO_BIN_EXE_fake-claude");
const FAKE_CODEX: &str = env!("CARGO_BIN_EXE_fake-codex");
const WAIT: Duration = Duration::from_secs(15);

// ------------------------------------------------------------------------------------------------ harness

/// One isolated daemon: its own database, bots_dir (scenario + fake-claude log live there) and owner.
struct H {
    admin_url: String,
    db_name: String,
    pool: PgPool,
    dir: PathBuf,
    owner: Uuid,
    shutdown: CancellationToken,
    daemon: Option<JoinHandle<Result<()>>>,
}

/// Process-wide: config home (device id, tools dir) in a temp folder, never the real `~/.familiar`.
fn global_setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join(format!("familiar-testkit-home-{}", std::process::id()));
        // A "pre-installed" Playwright MCP, so the daemon never runs `npm install` (runs only reference its path).
        let pkg = home.join("tools").join("node_modules").join("@playwright").join("mcp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"name":"@playwright/mcp","version":"0.0.83"}"#).unwrap();
        std::fs::write(pkg.join("cli.js"), "process.exit(0)\n").unwrap();
        // SAFETY: runs once, before any test of this binary starts a daemon or spawns a process.
        unsafe { std::env::set_var("FAMILIAR_HOME", &home) };
        if std::env::var_os("RUST_LOG").is_some() {
            let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).try_init();
        }
    });
}

fn with_db(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let base = match base.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| (base.len() - rest.len()) + i)) {
        Some(slash) => &base[..slash],
        None => base,
    };
    match query {
        Some(q) => format!("{base}/{name}?{q}"),
        None => format!("{base}/{name}"),
    }
}

/// None (test skipped) when TEST_DATABASE_URL is not set.
async fn setup(test: &str) -> Option<H> {
    let Ok(admin_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping {test}: TEST_DATABASE_URL is not set (e.g. postgres://postgres:pw@localhost:55450/postgres)");
        return None;
    };
    global_setup();
    let db_name = format!("t_{}", Uuid::new_v4().simple());
    let admin = PgPoolOptions::new().max_connections(1).connect(&admin_url).await.expect("connect TEST_DATABASE_URL");
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {db_name}"))).execute(&admin).await.unwrap();
    admin.close().await;
    let pool = PgPoolOptions::new().max_connections(4).connect(&with_db(&admin_url, &db_name)).await.unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let owner: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('owner@test', 'x') returning id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let dir = std::env::temp_dir().join(format!("familiar-testkit-{db_name}"));
    std::fs::create_dir_all(&dir).unwrap();
    Some(H { admin_url, db_name, pool, dir, owner, shutdown: CancellationToken::new(), daemon: None })
}

impl H {
    fn config(&self, extra: Value) -> familiar_core::Config {
        let mut cfg = json!({
            "database_url": with_db(&self.admin_url, &self.db_name),
            "owner_id": self.owner,
            "claude_bin": FAKE_CLAUDE,
            "codex_bin": FAKE_CODEX,
            "bots_dir": self.dir,
            // Never start a real Chrome: a browser that doesn't exist makes Playwright fall back (unused here).
            "browser_bin": self.dir.join("no-such-browser.exe"),
            "device_name": "testkit",
            "max_parallel": 2,
            "check_models": false,
        });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            cfg[k] = v;
        }
        serde_json::from_value(cfg).unwrap()
    }

    fn start(&mut self) {
        self.start_with(json!({}));
    }

    fn start_with(&mut self, extra: Value) {
        let cfg = self.config(extra);
        self.shutdown = CancellationToken::new();
        self.daemon = Some(tokio::spawn(familiar_core::run(cfg, self.shutdown.clone())));
    }

    async fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(d) = self.daemon.take() {
            match tokio::time::timeout(Duration::from_secs(30), d).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => panic!("daemon failed: {e:#}"),
                Ok(Err(e)) => panic!("daemon panicked: {e}"),
                Err(_) => panic!("daemon did not shut down"),
            }
        }
    }

    /// Stop the daemon, drop the database and the temp folder.
    async fn finish(mut self) {
        self.stop().await;
        // Bounded: a connection a test still holds (e.g. a PgListener) must not hang the suite.
        let _ = tokio::time::timeout(Duration::from_secs(5), self.pool.close()).await;
        if let Ok(admin) = PgPoolOptions::new().max_connections(1).connect(&self.admin_url).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists {} with (force)", self.db_name))).execute(&admin).await;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// The fake's script for every claude spawned by this daemon. `invocations[n]` drives the n-th spawn.
    fn scenario(&self, invocations: Value) {
        std::fs::write(self.dir.join("fake-claude.json"), json!({ "invocations": invocations }).to_string()).unwrap();
    }

    fn codex_scenario(&self, invocations: Value) {
        std::fs::write(self.dir.join("fake-codex.json"), json!({ "invocations": invocations }).to_string()).unwrap();
    }

    fn log(&self) -> Vec<Value> {
        self.log_of("fake-claude")
    }

    fn log_of(&self, fake: &str) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join(format!("{fake}.log.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// What the fake saw in its n-th invocation.
    fn invocation(&self, n: usize) -> Inv {
        let entries: Vec<Value> = self.log().into_iter().filter(|e| e["n"] == n).collect();
        let start = entries.iter().find(|e| e["event"] == "start").unwrap_or_else(|| panic!("no invocation {n}"));
        Inv {
            argv: start["argv"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_owned()).collect(),
            pid: start["pid"].as_u64().unwrap() as u32,
            env: start["env"].clone(),
            entries,
        }
    }

    fn invocations(&self) -> usize {
        self.log().iter().filter(|e| e["event"] == "start").count()
    }

    async fn bot(&self, slug: &str) -> Uuid {
        // last_dreamed_at = now(): no nightly dream gets queued behind the test's back.
        sqlx::query_scalar("insert into bots (owner_id, slug, name, last_dreamed_at) values ($1, $2, $2, now()) returning id")
            .bind(self.owner)
            .bind(slug)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn thread(&self, bot: Uuid) -> Uuid {
        sqlx::query_scalar("insert into threads (owner_id, bot_id, title) values ($1, $2, 'test') returning id")
            .bind(self.owner)
            .bind(bot)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// A user message; its trigger queues the chat run, whose id is returned.
    async fn say(&self, thread: Uuid, content: &str) -> Uuid {
        sqlx::query("insert into messages (owner_id, thread_id, role, content) values ($1, $2, 'user', $3)")
            .bind(self.owner)
            .bind(thread)
            .bind(content)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query_scalar("select id from runs where thread_id = $1 order by created_at desc limit 1")
            .bind(thread)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn insert_run(&self, bot: Uuid, thread: Uuid, kind: &str, prompt: &str, status: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into runs (owner_id, bot_id, thread_id, kind, prompt, status) values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(self.owner)
        .bind(bot)
        .bind(thread)
        .bind(kind)
        .bind(prompt)
        .bind(status)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn run_row(&self, run: Uuid) -> (String, Option<String>, bool) {
        sqlx::query_as("select status, error, finished_at is not null from runs where id = $1")
            .bind(run)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Wait until the run is finished (finished_at set) and return (status, error).
    async fn finished(&self, run: Uuid) -> (String, Option<String>) {
        wait_for(&format!("run {run} to finish"), || async {
            let (status, error, done) = self.run_row(run).await;
            Ok(done.then_some((status, error)))
        })
        .await
    }

    async fn wait_status(&self, run: Uuid, status: &str) {
        wait_for(&format!("run {run} to be {status}"), || async {
            Ok((self.run_row(run).await.0 == status).then_some(()))
        })
        .await
    }

    /// The run's only pending approval: (id, tool_name, input, reason).
    async fn pending_approval(&self, run: Uuid) -> (Uuid, String, Value, Option<String>) {
        wait_for("a pending approval", || async {
            Ok(sqlx::query_as::<_, (Uuid, String, sqlx::types::Json<Value>, Option<String>)>(
                "select id, tool_name, input, reason from approvals where run_id = $1 and status = 'pending'",
            )
            .bind(run)
            .fetch_optional(&self.pool)
            .await?
            .map(|(id, t, i, r)| (id, t, i.0, r)))
        })
        .await
    }

    /// The owner decides, as the API does.
    async fn decide(&self, approval: Uuid, status: &str, response: Option<&str>) {
        sqlx::query("update approvals set status = $1, response = $2, decided_by = 'user', decided_at = now() where id = $3")
            .bind(status)
            .bind(response)
            .bind(approval)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// The owner approves with their edit of the input (what `POST /api/approvals/{id}` stores for "Edit & approve").
    async fn decide_edited(&self, approval: Uuid, edited: Value) {
        sqlx::query(
            "update approvals set status = 'approved', edited_input = $1, decided_by = 'user', decided_at = now() where id = $2",
        )
        .bind(sqlx::types::Json(edited))
        .bind(approval)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// What the card offers: (editable fields, the "Always allow" rule).
    async fn offer(&self, approval: Uuid) -> (Vec<String>, Option<String>) {
        sqlx::query_as("select editable, allow_rule from approvals where id = $1").bind(approval).fetch_one(&self.pool).await.unwrap()
    }

    async fn events(&self, run: Uuid) -> Vec<(i32, String, Value)> {
        sqlx::query_as::<_, (i32, String, sqlx::types::Json<Value>)>("select seq, kind, payload from events where run_id = $1 order by seq")
            .bind(run)
            .fetch_all(&self.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|(s, k, p)| (s, k, p.0))
            .collect()
    }

    async fn approvals(&self, run: Uuid) -> Vec<(String, String, Option<String>)> {
        sqlx::query_as("select tool_name, status, decided_by from approvals where run_id = $1 order by created_at")
            .bind(run)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    async fn session(&self, thread: Uuid) -> Option<Uuid> {
        sqlx::query_scalar("select claude_session_id from threads where id = $1").bind(thread).fetch_one(&self.pool).await.unwrap()
    }

    async fn assistant_messages(&self, thread: Uuid) -> Vec<(String, Option<Uuid>)> {
        sqlx::query_as("select content, run_id from messages where thread_id = $1 and role = 'assistant' order by created_at")
            .bind(thread)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
}

/// Poll `f` every 50 ms until it yields a value; panic after [`WAIT`].
async fn wait_for<T, F, Fut>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match f().await {
            Ok(Some(v)) => return v,
            Ok(None) => {}
            Err(e) => panic!("waiting for {what}: {e:#}"),
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One fake-claude invocation, from its log.
struct Inv {
    argv: Vec<String>,
    pid: u32,
    env: Value,
    entries: Vec<Value>,
}

impl Inv {
    fn flag(&self, name: &str) -> Option<&str> {
        self.argv.iter().position(|a| a == name).and_then(|i| self.argv.get(i + 1)).map(String::as_str)
    }

    fn has(&self, name: &str) -> bool {
        self.argv.iter().any(|a| a == name)
    }

    /// The `--settings` permissions object.
    fn permissions(&self) -> Value {
        let s: Value = serde_json::from_str(self.flag("--settings").expect("--settings")).unwrap();
        s["permissions"].clone()
    }

    /// The prompt: content of the first `user` line on stdin.
    fn prompt(&self) -> String {
        self.entries
            .iter()
            .find(|e| e["event"] == "stdin" && e["line"]["type"] == "user")
            .and_then(|e| e["line"]["message"]["content"].as_str())
            .expect("a user message on stdin")
            .to_owned()
    }

    fn decisions(&self) -> Vec<Value> {
        self.entries.iter().filter(|e| e["event"] == "permission").map(|e| e["decision"].clone()).collect()
    }

    fn mcp_results(&self) -> Vec<(String, String, bool)> {
        self.entries
            .iter()
            .filter(|e| e["event"] == "mcp")
            .map(|e| (e["tool"].as_str().unwrap().to_owned(), e["text"].as_str().unwrap().to_owned(), e["is_error"] == true))
            .collect()
    }
}

fn strings(v: &Value) -> Vec<&str> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))).unwrap_or(false)
    } else {
        std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
    }
}

async fn wait_gone(pid: u32) {
    wait_for(&format!("process {pid} to exit"), || async { Ok((!alive(pid)).then_some(())) }).await
}

fn tool(name: &str, input: Value) -> Value {
    json!({ "tool": { "name": name, "input": input } })
}

fn mcp(tool: &str, arguments: Value) -> Value {
    json!({ "mcp": { "tool": tool, "arguments": arguments } })
}

// ------------------------------------------------------------------------------------------------ tests

/// (a) chat message → run succeeded, reply stored, events in order, session id stored; next message resumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_turn_then_resume() {
    let Some(mut h) = setup("chat_turn_then_resume").await else { return };
    h.scenario(json!([
        [{ "init": {} }, { "delta": "Hel" }, { "delta": "lo" }, { "text": "Hello there" }, { "result": "Hello there" }],
        [{ "init": {} }, { "text": "Again" }, { "result": "Again" }],
    ]));
    h.start();
    let bot = h.bot("chatty").await;
    let thread = h.thread(bot).await;
    // Live token deltas go out as NOTIFY familiar_delta (no table writes).
    let mut deltas = sqlx::postgres::PgListener::connect_with(&h.pool).await.unwrap();
    deltas.listen("familiar_delta").await.unwrap();

    let run1 = h.say(thread, "hi").await;
    assert_eq!(h.finished(run1).await, ("succeeded".into(), None));
    let mut streamed = String::new();
    while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(500), deltas.recv()).await {
        let d: Value = serde_json::from_str(n.payload()).unwrap();
        assert_eq!((d["run"].as_str(), d["kind"].as_str()), (Some(run1.to_string().as_str()), Some("text")));
        streamed.push_str(d["text"].as_str().unwrap());
    }
    assert_eq!(streamed, "Hello");
    drop(deltas); // holds a pool connection: pool.close() in finish() would wait for it forever
    let msgs = h.assistant_messages(thread).await;
    assert_eq!(msgs, vec![("Hello there".to_owned(), Some(run1))]);
    let ev = h.events(run1).await;
    assert_eq!(ev.iter().map(|e| e.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(ev.iter().map(|e| e.1.as_str()).collect::<Vec<_>>(), vec!["status", "text", "result"]);
    let session = h.session(thread).await.expect("session id stored");
    assert_eq!(ev[0].2["session_id"], session.to_string());
    assert_eq!(ev[1].2["text"], "Hello there");
    assert_eq!(ev[2].2["subtype"], "success");
    let cost: Option<f64> = sqlx::query_scalar("select cost_usd::float8 from runs where id = $1").bind(run1).fetch_one(&h.pool).await.unwrap();
    assert_eq!(cost, Some(0.0123));

    let inv = h.invocation(0);
    assert_eq!(inv.flag("--session-id"), Some(session.to_string().as_str()));
    assert!(!inv.has("--resume"));
    for f in ["-p", "--verbose", "--restricted", "--strict-mcp-config", "--include-partial-messages"] {
        assert!(inv.has(f), "missing {f}: {:?}", inv.argv);
    }
    assert_eq!(inv.flag("--output-format"), Some("stream-json"));
    assert_eq!(inv.flag("--input-format"), Some("stream-json"));
    assert_eq!(inv.flag("--permission-prompt-tool"), Some("stdio"));
    assert_eq!(inv.flag("--model"), Some("sonnet"));
    assert_eq!(inv.flag("--tools"), Some(familiar_core::permissions::FULL_TOOLS.join(",").as_str()));
    assert_eq!(inv.flag("--system-prompt-snapshot"), Some("off"));
    let prompt_file = inv.flag("--append-system-prompt-file").unwrap();
    assert!(Path::new(prompt_file).starts_with(h.dir.join(".prompts")), "prompt file outside the workspace");
    let mcp_file = PathBuf::from(inv.flag("--mcp-config").unwrap());
    assert!(!mcp_file.exists(), "the per-run MCP config (run token) must be deleted after the run");
    assert!(strings(&inv.permissions()["allow"]).contains(&"mcp__familiar"));
    // propose_draft waits up to a day for the owner: the CLI must not give up on it first.
    assert_eq!(inv.env["MCP_TOOL_TIMEOUT"], (familiar_core::mcp::TOOL_TIMEOUT.as_millis()).to_string());
    assert_eq!(inv.prompt(), "hi");

    let run2 = h.say(thread, "again").await;
    assert_eq!(h.finished(run2).await.0, "succeeded");
    let inv = h.invocation(1);
    assert_eq!(inv.flag("--resume"), Some(session.to_string().as_str()));
    assert!(!inv.has("--session-id"));
    assert_eq!(inv.prompt(), "again");
    assert_eq!(h.session(thread).await, Some(session));
    h.finish().await;
}

/// (b) can_use_tool for Write → pending approval; approve → tool ok, run succeeds; deny → model told, run continues.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_approve_and_deny() {
    let Some(mut h) = setup("permission_approve_and_deny").await else { return };
    let write = tool("Write", json!({ "file_path": "notes.txt", "content": "hello" }));
    h.scenario(json!([
        [{ "init": {} }, write, { "result": "wrote it" }],
        [{ "init": {} }, write, { "text": "ok, I won't" }, { "result": "did not write" }],
    ]));
    h.start();
    let bot = h.bot("writer").await;
    let thread = h.thread(bot).await;

    // approve
    let run = h.say(thread, "write a note").await;
    let (approval, tool_name, input, _) = h.pending_approval(run).await;
    assert_eq!(tool_name, "Write");
    assert_eq!(input["file_path"], "notes.txt");
    h.wait_status(run, "waiting_approval").await;
    h.decide(approval, "approved", None).await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    let d = h.invocation(0).decisions();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["behavior"], "allow");
    assert_eq!(d[0]["updatedInput"]["content"], "hello");
    let ev = h.events(run).await;
    let kinds: Vec<&str> = ev.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(kinds, vec!["status", "tool_call", "approval", "approval", "tool_result", "result"], "{ev:?}");
    assert_eq!(ev[2].2["status"], "pending");
    assert_eq!(ev[3].2["status"], "approved");
    assert_eq!(ev[3].2["decided_by"], "user");
    assert_eq!(ev[4].2["is_error"], false);

    // deny
    let run = h.say(thread, "write it again").await;
    let (approval, ..) = h.pending_approval(run).await;
    h.decide(approval, "denied", None).await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    let d = h.invocation(1).decisions();
    assert_eq!(d[0]["behavior"], "deny");
    assert!(d[0]["message"].as_str().unwrap().contains("denied"), "{d:?}");
    let ev = h.events(run).await;
    let result = ev.iter().find(|e| e.1 == "tool_result").unwrap();
    assert_eq!(result.2["is_error"], true);
    assert_eq!(h.assistant_messages(thread).await.last().unwrap().0, "did not write");
    h.finish().await;
}

/// (c) An owner `Bash` allow rule auto-allows ordinary commands but never always-human ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn always_human_beats_owner_allow_rule() {
    let Some(mut h) = setup("always_human_beats_owner_allow_rule").await else { return };
    h.scenario(json!([[
        { "init": {} },
        tool("Bash", json!({ "command": "ls -la" })),
        tool("Bash", json!({ "command": "sh -c \"rm -rf x\"" })),
        { "result": "done" },
    ]]));
    h.start();
    let bot = h.bot("sheller").await;
    sqlx::query("insert into rules (owner_id, bot_id, pattern, decision) values ($1, null, 'Bash', 'allow')")
        .bind(h.owner)
        .execute(&h.pool)
        .await
        .unwrap();
    let thread = h.thread(bot).await;
    let run = h.say(thread, "clean up").await;

    let (approval, tool_name, input, reason) = h.pending_approval(run).await;
    assert_eq!(tool_name, "Bash");
    assert_eq!(input["command"], "sh -c \"rm -rf x\"");
    assert!(reason.as_deref().unwrap_or_default().starts_with("Always needs you"), "{reason:?}");
    h.decide(approval, "denied", None).await;
    assert_eq!(h.finished(run).await.0, "succeeded");

    let inv = h.invocation(0);
    let d = inv.decisions();
    assert_eq!(d[0]["behavior"], "allow", "ls -la is allowed by the owner rule");
    assert_eq!(d[1]["behavior"], "deny");
    // Bash allow rules are applied by the daemon, never forwarded to Claude Code.
    assert!(!strings(&inv.permissions()["allow"]).iter().any(|p| p.starts_with("Bash")), "{}", inv.permissions());
    assert_eq!(h.approvals(run).await.len(), 1, "only the rm -rf needed the owner");
    let ev = h.events(run).await;
    assert!(ev.iter().any(|e| e.1 == "approval" && e.2["status"] == "approved" && e.2["decided_by"] == "rule"));
    h.finish().await;
}

/// (d) Proactive runs: research tools only, every permission prompt denied without asking the owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_run_denies_without_approval() {
    let Some(mut h) = setup("research_run_denies_without_approval").await else { return };
    h.scenario(json!([[
        { "init": {} },
        tool("Write", json!({ "file_path": "x.txt", "content": "x" })),
        { "result": "looked around" },
    ]]));
    h.start();
    let bot = h.bot("scout").await;
    let thread = h.thread(bot).await;
    let run = h.insert_run(bot, thread, "proactive", "check the news", "queued").await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    assert!(h.approvals(run).await.is_empty(), "research runs never create approvals");
    let inv = h.invocation(0);
    assert_eq!(inv.flag("--tools"), Some("Read,Glob,Grep,WebSearch,WebFetch"));
    assert!(strings(&inv.permissions()["allow"]).contains(&"WebFetch"));
    let d = inv.decisions();
    assert_eq!(d[0]["behavior"], "deny");
    assert!(d[0]["message"].as_str().unwrap().contains("research-only"));
    let ev = h.events(run).await;
    let a = ev.iter().find(|e| e.1 == "approval").unwrap();
    assert_eq!((a.2["status"].as_str(), a.2["decided_by"].as_str()), (Some("denied"), Some("rule")));
    h.finish().await;
}

/// (e) Cancel while waiting for approval: run cancelled, approval expired, claude gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_waiting_for_approval() {
    let Some(mut h) = setup("cancel_while_waiting_for_approval").await else { return };
    h.scenario(json!([[
        { "init": {} },
        tool("Write", json!({ "file_path": "x.txt", "content": "x" })),
        { "sleep_ms": 20000 },
        { "result": "should not get here" },
    ]]));
    h.start();
    let bot = h.bot("canceller").await;
    let thread = h.thread(bot).await;
    let run = h.say(thread, "do something").await;
    let (approval, ..) = h.pending_approval(run).await;
    let pid = h.invocation(0).pid;
    assert!(alive(pid));

    sqlx::query("update runs set status = 'cancelled' where id = $1").bind(run).execute(&h.pool).await.unwrap();
    let (status, _) = h.finished(run).await;
    assert_eq!(status, "cancelled");
    let (a_status, by): (String, Option<String>) =
        sqlx::query_as("select status, decided_by from approvals where id = $1").bind(approval).fetch_one(&h.pool).await.unwrap();
    assert_eq!((a_status.as_str(), by.as_deref()), ("expired", Some("rule")));
    wait_gone(pid).await;
    assert!(h.log().iter().any(|e| e["event"] == "stdin" && e["line"]["request"]["subtype"] == "interrupt"), "interrupt sent");
    assert!(h.assistant_messages(thread).await.is_empty());
    h.finish().await;
}

/// (f) Schedule gates: empty output skips the run without spawning claude; output is appended to the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_skips_or_feeds_the_run() {
    let Some(mut h) = setup("gate_skips_or_feeds_the_run").await else { return };
    h.scenario(json!([[{ "init": {} }, { "result": "handled" }]]));
    let bot = h.bot("gated").await;
    let schedule = |gate: &'static str, prompt: &'static str| {
        let (pool, owner) = (h.pool.clone(), h.owner);
        async move {
            let thread: Uuid = sqlx::query_scalar(
                "insert into schedules (owner_id, bot_id, cron, prompt, kind, gate_command, next_run_at)
                 values ($1, $2, '0 0 1 1 *', $3, 'scheduled', $4, now() - interval '1 minute') returning thread_id",
            )
            .bind(owner)
            .bind(bot)
            .bind(prompt)
            .bind(gate)
            .fetch_one(&pool)
            .await
            .unwrap();
            thread
        }
    };
    let quiet = schedule("exit 0", "quiet check").await;
    let loud = schedule("echo gated-hello", "loud check").await;
    h.start();
    let run_of = |thread: Uuid| {
        let pool = h.pool.clone();
        async move {
            wait_for("the scheduled run", || async {
                Ok(sqlx::query_scalar::<_, Uuid>("select id from runs where thread_id = $1").bind(thread).fetch_optional(&pool).await?)
            })
            .await
        }
    };
    let (quiet_run, loud_run) = (run_of(quiet).await, run_of(loud).await);
    assert_eq!(h.finished(quiet_run).await, ("succeeded".into(), Some("gate: nothing to do".into())));
    assert_eq!(h.finished(loud_run).await, ("succeeded".into(), None));
    assert_eq!(h.invocations(), 1, "the skipped run never spawned claude");
    let prompt = h.invocation(0).prompt();
    assert!(prompt.starts_with("loud check\n\n--- output of `echo gated-hello` ---\n"), "{prompt:?}");
    assert!(prompt.contains("gated-hello"));
    h.finish().await;
}

/// (g) Lost session ("No conversation found") → fresh --session-id with a prompt seeded from the thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_session_restarts_seeded() {
    let Some(mut h) = setup("lost_session_restarts_seeded").await else { return };
    h.scenario(json!([
        [{ "stderr": "No conversation found with session ID: gone" }, { "exit": 1 }],
        [{ "init": {} }, { "result": "back on track" }],
    ]));
    h.start();
    let bot = h.bot("forgetful").await;
    let thread = h.thread(bot).await;
    let old = Uuid::new_v4();
    sqlx::query("update threads set claude_session_id = $1 where id = $2").bind(old).bind(thread).execute(&h.pool).await.unwrap();
    sqlx::query("insert into messages (owner_id, thread_id, role, content) values ($1, $2, 'assistant', 'earlier answer')")
        .bind(h.owner)
        .bind(thread)
        .execute(&h.pool)
        .await
        .unwrap();
    let run = h.say(thread, "continue please").await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));

    assert_eq!(h.invocation(0).flag("--resume"), Some(old.to_string().as_str()));
    let retry = h.invocation(1);
    let fresh = retry.flag("--session-id").expect("fresh session").parse::<Uuid>().unwrap();
    assert_ne!(fresh, old);
    assert!(!retry.has("--resume"));
    assert_eq!(h.session(thread).await, Some(fresh));
    let prompt = retry.prompt();
    assert!(prompt.starts_with("(Your previous session was lost"), "{prompt}");
    assert!(prompt.contains("assistant: earlier answer"), "{prompt}");
    assert!(prompt.ends_with("(New message:)\ncontinue please"), "{prompt}");
    assert_eq!(prompt.matches("continue please").count(), 1, "the new message is not duplicated in the history");
    assert_eq!(h.assistant_messages(thread).await.last().unwrap().0, "back on track");
    h.finish().await;
}

/// (h) Familiar MCP over real HTTP: remember (proposed, deduplicated), notify_user, ask_user.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_remember_notify_ask() {
    let Some(mut h) = setup("mcp_remember_notify_ask").await else { return };
    h.scenario(json!([[
        { "init": {} },
        mcp("remember", json!({ "content": "Owner likes green tea" })),
        mcp("remember", json!({ "content": "  owner likes GREEN tea " })),
        mcp("notify_user", json!({ "message": "heads up: tea time" })),
        mcp("ask_user", json!({ "question": "Which colour?" })),
        { "result": "all set" },
    ]]));
    h.start();
    let bot = h.bot("tooly").await;
    let thread = h.thread(bot).await;
    let run = h.say(thread, "learn things").await;

    let (approval, tool_name, input, _) = h.pending_approval(run).await;
    assert_eq!(tool_name, "ask_user");
    assert_eq!(input["question"], "Which colour?");
    h.decide(approval, "approved", Some("blue")).await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));

    let mems: Vec<(String, String, String)> =
        sqlx::query_as("select content, status, source from memories where bot_id = $1").bind(bot).fetch_all(&h.pool).await.unwrap();
    assert_eq!(mems, vec![("Owner likes green tea".into(), "proposed".into(), "bot".into())]);
    let results = h.invocation(0).mcp_results();
    assert_eq!(results.len(), 4, "{results:?}");
    assert!(results.iter().all(|r| !r.2), "no MCP errors: {results:?}");
    assert!(results[0].1.starts_with("Proposed"));
    assert_eq!(results[2].1, "Delivered.");
    assert_eq!(results[3].1, "Owner answered: blue");
    let msgs: Vec<String> = h.assistant_messages(thread).await.into_iter().map(|m| m.0).collect();
    assert_eq!(msgs, vec!["heads up: tea time".to_owned(), "all set".to_owned()]);
    h.finish().await;
}

/// Drafts: propose_draft blocks until the owner decides; edited and approved → the tool returns the edited text to use
/// exactly; the approval keeps the proposal and the edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_edit_then_approve() {
    let Some(mut h) = setup("draft_edit_then_approve").await else { return };
    h.scenario(json!([[
        { "init": {} },
        mcp("propose_draft", json!({ "kind": "post", "channel": "X", "body": "Shipping drafts today!!", "note": "launch week" })),
        { "result": "posted" },
    ]]));
    h.start();
    let bot = h.bot("poster").await;
    let thread = h.thread(bot).await;
    let run = h.say(thread, "draft a post").await;

    let (approval, tool_name, input, reason) = h.pending_approval(run).await;
    assert_eq!(tool_name, "propose_draft");
    assert_eq!(input, json!({ "kind": "post", "channel": "X", "body": "Shipping drafts today!!" }));
    assert_eq!(reason.as_deref(), Some("launch week"));
    assert_eq!(h.offer(approval).await, (vec!["body".to_owned(), "subject".to_owned(), "to".to_owned()], None));
    h.wait_status(run, "waiting_approval").await;
    let mut edited = input.clone();
    edited["body"] = json!("Drafts ship today.");
    h.decide_edited(approval, edited.clone()).await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));

    let results = h.invocation(0).mcp_results();
    assert_eq!(results.len(), 1, "{results:?}");
    let (tool, text, is_error) = &results[0];
    assert_eq!((tool.as_str(), *is_error), ("propose_draft", false));
    assert!(text.starts_with("APPROVED WITH EDITS"), "{text}");
    assert!(text.contains("use exactly this text"), "{text}");
    assert!(text.contains("\nDrafts ship today.\n----- END APPROVED TEXT -----") && !text.contains("today!!"), "{text}");
    let (proposed, kept): (sqlx::types::Json<Value>, sqlx::types::Json<Value>) =
        sqlx::query_as("select input, edited_input from approvals where id = $1").bind(approval).fetch_one(&h.pool).await.unwrap();
    assert_eq!((proposed.0, kept.0), (input, edited));
    let ev = h.events(run).await;
    assert!(
        ev.iter().any(|e| e.1 == "approval" && e.2["status"] == "approved" && e.2["edited"] == true && e.2["tool_name"] == "propose_draft"),
        "{ev:?}"
    );
    h.finish().await;
}

/// Drafts: a rejection carries the owner's note; "Ask for changes" tells the teammate to revise and propose again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_reject_and_revise() {
    let Some(mut h) = setup("draft_reject_and_revise").await else { return };
    let reply = json!({ "kind": "reply", "channel": "Reddit", "to": "https://reddit.com/r/x/1", "body": "Try Familiar!" });
    h.scenario(json!([[{ "init": {} }, mcp("propose_draft", reply.clone()), mcp("propose_draft", reply), { "result": "ok" }]]));
    h.start();
    let bot = h.bot("listener").await;
    let thread = h.thread(bot).await;
    let run = h.say(thread, "answer the thread").await;

    let (first, ..) = h.pending_approval(run).await;
    h.decide(first, "denied", Some("too salesy")).await;
    let (second, ..) = wait_for("the second draft", || async {
        Ok(sqlx::query_scalar::<_, Uuid>("select id from approvals where run_id = $1 and status = 'pending'")
            .bind(run)
            .fetch_optional(&h.pool)
            .await?
            .map(|id| (id,)))
    })
    .await;
    assert_ne!(first, second);
    h.decide(second, "revise", Some("answer their question first")).await;
    assert_eq!(h.finished(run).await.0, "succeeded");

    let results = h.invocation(0).mcp_results();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results[0].1.starts_with("REJECTED") && results[0].1.contains("\"too salesy\""), "{results:?}");
    assert!(results[0].1.contains("Do not post or send"));
    assert!(results[1].1.starts_with("CHANGES REQUESTED") && results[1].1.contains("answer their question first"), "{results:?}");
    assert!(results[1].1.contains("call propose_draft again"));
    assert!(results.iter().all(|r| !r.2), "decisions are not tool errors: {results:?}");
    h.finish().await;
}

/// "Edit & approve" on a tool call: the edited input reaches Claude as `updatedInput`, but only after the same checks
/// as any call. An edit into an always-human command is asked again (and can be refused); an edit matching an owner
/// deny rule is refused outright; a deny note reaches the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edited_tool_input_is_rechecked() {
    let Some(mut h) = setup("edited_tool_input_is_rechecked").await else { return };
    h.scenario(json!([[
        { "init": {} },
        tool("Bash", json!({ "command": "wc -l notes.md", "description": "count" })),
        tool("Bash", json!({ "command": "ls build" })),
        tool("Bash", json!({ "command": "git status" })),
        tool("Bash", json!({ "command": "curl https://example.com" })),
        { "result": "done" },
    ]]));
    h.start();
    let bot = h.bot("edited").await;
    sqlx::query("insert into rules (owner_id, bot_id, pattern, decision) values ($1, $2, 'Bash(git push*)', 'deny')")
        .bind(h.owner)
        .bind(bot)
        .execute(&h.pool)
        .await
        .unwrap();
    let thread = h.thread(bot).await;
    let run = h.say(thread, "look around").await;
    let next = |prev: Uuid| {
        let h = &h;
        async move {
            wait_for("the next approval", || async {
                Ok(sqlx::query_scalar::<_, Uuid>("select id from approvals where run_id = $1 and status = 'pending' and id <> $2")
                    .bind(run)
                    .bind(prev)
                    .fetch_optional(&h.pool)
                    .await?)
            })
            .await
        }
    };

    // 1. A plain edit: allowed, and the tool runs the owner's version (other fields untouched).
    let (a1, _, input, _) = h.pending_approval(run).await;
    // "Always allow" would cover exactly this command, nothing more.
    assert_eq!(h.offer(a1).await, (vec!["command".to_owned()], Some("Bash(wc -l notes.md)".to_owned())));
    h.decide_edited(a1, json!({ "command": "wc -l notes.md todo.md", "description": input["description"] })).await;

    // 2. Edited into a recursive delete: a second approval of exactly that, with nothing to edit or always allow.
    let a2 = next(a1).await;
    h.decide_edited(a2, json!({ "command": "rm -rf build" })).await;
    let a3 = next(a2).await;
    let (tool_name, input, reason): (String, sqlx::types::Json<Value>, Option<String>) =
        sqlx::query_as("select tool_name, input, reason from approvals where id = $1").bind(a3).fetch_one(&h.pool).await.unwrap();
    assert_eq!((tool_name.as_str(), input.0), ("Bash", json!({ "command": "rm -rf build" })));
    assert!(reason.as_deref().unwrap_or_default().starts_with("Always needs you: after your edit"), "{reason:?}");
    assert_eq!(h.offer(a3).await, (vec![], None));
    h.decide(a3, "denied", None).await;

    // 3. Edited into something an owner deny rule blocks: refused without asking again.
    let a4 = next(a3).await;
    h.decide_edited(a4, json!({ "command": "git push origin main" })).await;

    // 4. Denied with a note: the model reads it.
    let a5 = next(a4).await;
    h.decide(a5, "denied", Some("use the fetch connector instead")).await;
    assert_eq!(h.finished(run).await.0, "succeeded");

    let d = h.invocation(0).decisions();
    assert_eq!(d.len(), 4, "{d:?}");
    assert_eq!(d[0]["behavior"], "allow");
    assert_eq!(d[0]["updatedInput"], json!({ "command": "wc -l notes.md todo.md", "description": "count" }));
    assert_eq!(d[1]["behavior"], "deny", "the edited rm -rf was refused on the second look");
    assert_eq!(d[2]["behavior"], "deny");
    assert!(d[2]["message"].as_str().unwrap().contains("deny rules"), "{d:?}");
    assert_eq!(d[3]["behavior"], "deny");
    assert!(d[3]["message"].as_str().unwrap().contains("use the fetch connector instead"), "{d:?}");
    let approvals: Vec<String> = h.approvals(run).await.into_iter().map(|a| a.1).collect();
    assert_eq!(approvals, ["approved", "approved", "denied", "approved", "denied"]);
    let ev = h.events(run).await;
    assert!(ev.iter().any(|e| e.1 == "approval" && e.2["status"] == "approved" && e.2["edited"] == true), "{ev:?}");
    h.finish().await;
}

/// Owner prefix rules stay narrow: a bot's `Bash(git status *)` lets that command through without asking, but not a
/// compound command built on it; compound and always-human commands never offer "Always allow".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn always_allow_rule_is_narrow() {
    let Some(mut h) = setup("always_allow_rule_is_narrow").await else { return };
    h.scenario(json!([[
        { "init": {} },
        tool("Bash", json!({ "command": "git status --short" })),
        tool("Bash", json!({ "command": "git status && rm notes.txt" })),
        tool("Bash", json!({ "command": "rm -rf notes" })),
        { "result": "done" },
    ]]));
    h.start();
    let bot = h.bot("allowed").await;
    sqlx::query("insert into rules (owner_id, bot_id, pattern, decision) values ($1, $2, 'Bash(git status *)', 'allow')")
        .bind(h.owner)
        .bind(bot)
        .execute(&h.pool)
        .await
        .unwrap();
    let thread = h.thread(bot).await;
    let run = h.say(thread, "status").await;

    let (a1, _, input, _) = h.pending_approval(run).await;
    assert_eq!(input["command"], "git status && rm notes.txt", "the plain git status went through on the rule");
    assert_eq!(h.offer(a1).await, (vec!["command".to_owned()], None), "compound commands get no always-allow rule");
    h.decide(a1, "denied", None).await;
    let a2 = wait_for("the rm -rf approval", || async {
        Ok(sqlx::query_scalar::<_, Uuid>("select id from approvals where run_id = $1 and status = 'pending'")
            .bind(run)
            .fetch_optional(&h.pool)
            .await?)
    })
    .await;
    assert_eq!(h.offer(a2).await.1, None, "never offered for always-human actions");
    h.decide(a2, "denied", None).await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    let d = h.invocation(0).decisions();
    assert_eq!(d.iter().map(|d| d["behavior"].as_str().unwrap()).collect::<Vec<_>>(), ["allow", "deny", "deny"]);
    h.finish().await;
}

/// (i) A due schedule becomes a queued run and gets its next fire time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn due_schedule_enqueues_and_advances() {
    let Some(mut h) = setup("due_schedule_enqueues_and_advances").await else { return };
    h.scenario(json!([[{ "init": {} }, { "result": "scheduled work done" }]]));
    let bot = h.bot("cronny").await;
    let (schedule, thread): (Uuid, Uuid) = sqlx::query_as(
        "insert into schedules (owner_id, bot_id, cron, prompt, kind, next_run_at)
         values ($1, $2, '0 0 1 1 *', 'yearly review', 'scheduled', now() - interval '1 minute') returning id, thread_id",
    )
    .bind(h.owner)
    .bind(bot)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    h.start();
    let run: Uuid = wait_for("the scheduled run", || async {
        Ok(sqlx::query_scalar("select id from runs where thread_id = $1 and kind = 'scheduled'").bind(thread).fetch_optional(&h.pool).await?)
    })
    .await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    assert_eq!(h.invocation(0).prompt(), "yearly review");
    let (advanced, fired): (bool, bool) = wait_for("next_run_at", || async {
        Ok(sqlx::query_as::<_, (Option<bool>, bool)>(
            "select next_run_at > now() + interval '1 day', last_run_at is not null from schedules where id = $1",
        )
        .bind(schedule)
        .fetch_one(&h.pool)
        .await
        .map(|(a, f)| a.map(|a| (a, f)))?)
    })
    .await;
    assert!(advanced && fired);
    let runs: i64 = sqlx::query_scalar("select count(*) from runs where thread_id = $1").bind(thread).fetch_one(&h.pool).await.unwrap();
    assert_eq!(runs, 1, "fired once");
    h.finish().await;
}

/// (j) Startup watchdog: a claude that never prints fails the run instead of hanging, and is killed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_watchdog() {
    let Some(mut h) = setup("startup_watchdog").await else { return };
    h.scenario(json!([[{ "sleep_ms": 30000 }]]));
    h.start_with(json!({ "startup_timeout_secs": 2 }));
    let bot = h.bot("silent").await;
    let thread = h.thread(bot).await;
    let run = h.say(thread, "hello?").await;
    let (status, error) = h.finished(run).await;
    assert_eq!(status, "failed");
    assert!(error.as_deref().unwrap_or_default().contains("did not start within 2 s"), "{error:?}");
    wait_gone(h.invocation(0).pid).await;
    let ev = h.events(run).await;
    assert_eq!(ev.last().map(|e| e.1.as_str()), Some("error"));
    h.finish().await;
}

/// (k) Restart: runs a previous daemon left running/waiting are failed and their approvals expired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_fails_interrupted_runs() {
    let Some(mut h) = setup("restart_fails_interrupted_runs").await else { return };
    h.scenario(json!([[{ "init": {} }, { "result": "fresh run fine" }]]));
    let bot = h.bot("crashy").await;
    let (t1, t2, t3) = (h.thread(bot).await, h.thread(bot).await, h.thread(bot).await);
    let running = h.insert_run(bot, t1, "chat", "was running", "running").await;
    let waiting = h.insert_run(bot, t2, "chat", "was waiting", "waiting_approval").await;
    let approval: Uuid = sqlx::query_scalar(
        "insert into approvals (owner_id, run_id, bot_id, tool_name, input, status) values ($1, $2, $3, 'Write', '{}', 'pending') returning id",
    )
    .bind(h.owner)
    .bind(waiting)
    .bind(bot)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    let queued = h.insert_run(bot, t3, "chat", "queued meanwhile", "queued").await;
    h.start();
    for run in [running, waiting] {
        assert_eq!(h.finished(run).await, ("failed".into(), Some("daemon restarted".into())));
    }
    let status: String = sqlx::query_scalar("select status from approvals where id = $1").bind(approval).fetch_one(&h.pool).await.unwrap();
    assert_eq!(status, "expired");
    // The bot is no longer blocked by its stale runs: queued work proceeds.
    assert_eq!(h.finished(queued).await.0, "succeeded");
    h.finish().await;
}

#[test]
fn db_url_rewrite() {
    assert_eq!(with_db("postgres://u:p@h:1/postgres", "t_x"), "postgres://u:p@h:1/t_x");
    assert_eq!(with_db("postgres://u:p@h:1/postgres?sslmode=disable", "t_x"), "postgres://u:p@h:1/t_x?sslmode=disable");
    assert_eq!(with_db("postgres://u:p@h:1", "t_x"), "postgres://u:p@h:1/t_x");
}

/// Subscription throttle: after a five_hour rate_limit_event ≥ 0.9, scheduled runs are held but chat still runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_holds_scheduled_runs() {
    let Some(mut h) = setup("rate_limit_holds_scheduled_runs").await else { return };
    let resets = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600;
    h.scenario(json!([
        [{ "init": {} }, { "rate_limit": { "rateLimitType": "five_hour", "utilization": 0.95, "resetsAt": resets } }, { "result": "noted" }],
        [{ "init": {} }, { "result": "chat still runs" }],
    ]));
    h.start();
    let bot = h.bot("busy").await;
    let (chat, other) = (h.thread(bot).await, h.thread(bot).await);
    let run = h.say(chat, "first").await;
    assert_eq!(h.finished(run).await.0, "succeeded");
    assert!(h.events(run).await.iter().any(|e| e.1 == "rate_limit"));

    let scheduled = h.insert_run(bot, other, "scheduled", "background work", "queued").await;
    let chat_run = h.say(chat, "are you there").await;
    assert_eq!(h.finished(chat_run).await.0, "succeeded");
    assert_eq!(h.run_row(scheduled).await.0, "queued", "scheduled work waits for the limit to reset");
    assert_eq!(h.invocations(), 2);
    h.finish().await;
}

// ------------------------------------------------------------------------------------------------ codex engine

/// The JSON-RPC requests the daemon sent to fake-codex in invocation `n`, by method.
fn codex_requests(h: &H, n: usize, method: &str) -> Vec<Value> {
    h.log_of("fake-codex")
        .into_iter()
        .filter(|e| e["n"] == n && e["event"] == "stdin" && e["line"]["method"] == method)
        .map(|e| e["line"]["params"].clone())
        .collect()
}

async fn codex_bot(h: &H, slug: &str) -> Uuid {
    let bot = h.bot(slug).await;
    sqlx::query("update bots set engine = 'codex' where id = $1").bind(bot).execute(&h.pool).await.unwrap();
    bot
}

/// Codex engine: thread/start with Familiar's policy, reply stored, codex thread id kept; next message resumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_chat_then_resume() {
    let Some(mut h) = setup("codex_chat_then_resume").await else { return };
    h.codex_scenario(json!([
        [{ "reasoning": "thinking it over" }, { "text": "Hi from codex" }, { "complete": {} }],
        [{ "text": "again from codex" }, { "complete": {} }],
    ]));
    h.start();
    let bot = codex_bot(&h, "codexer").await;
    let thread = h.thread(bot).await;

    let run = h.say(thread, "hi").await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));
    assert_eq!(h.assistant_messages(thread).await, vec![("Hi from codex".to_owned(), Some(run))]);
    let codex_thread: Option<String> =
        sqlx::query_scalar("select codex_thread_id from threads where id = $1").bind(thread).fetch_one(&h.pool).await.unwrap();
    assert_eq!(codex_thread.as_deref(), Some("thr_fake_0"));
    let start = &codex_requests(&h, 0, "thread/start")[0];
    assert_eq!(start["approvalPolicy"], "untrusted");
    assert_eq!(start["sandbox"], "danger-full-access");
    assert_eq!(start["model"], "gpt-fake", "a Claude alias means Codex's default model");
    assert!(start["developerInstructions"].as_str().unwrap().contains("always-on AI teammate"));
    let servers = &start["config"]["mcp_servers"];
    assert_eq!(servers["familiar"]["default_tools_approval_mode"], "approve");
    assert_eq!(servers["browser"]["default_tools_approval_mode"], "prompt");
    assert_eq!(servers["owner_own"]["enabled"], false, "the owner's own Codex MCP servers are off for bots");
    assert_eq!(start["config"]["project_doc_max_bytes"], 0);
    assert_eq!(codex_requests(&h, 0, "turn/start")[0]["input"][0]["text"], "hi");
    let ev = h.events(run).await;
    let kinds: Vec<&str> = ev.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(kinds, vec!["status", "thinking", "text", "result"], "{ev:?}");
    let usage: Value = sqlx::query_scalar::<_, sqlx::types::Json<Value>>("select usage from runs where id = $1")
        .bind(run)
        .fetch_one(&h.pool)
        .await
        .unwrap()
        .0;
    assert_eq!((usage["engine"].as_str(), usage["input_tokens"].as_u64()), (Some("codex"), Some(100)));

    let run2 = h.say(thread, "again").await;
    assert_eq!(h.finished(run2).await.0, "succeeded");
    assert_eq!(codex_requests(&h, 1, "thread/resume")[0]["threadId"], "thr_fake_0");
    assert!(codex_requests(&h, 1, "thread/start").is_empty());
    h.finish().await;
}

/// Codex exec approval: the shell wrapper is stripped, always-human applies, the owner's answer becomes accept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_exec_approval() {
    let Some(mut h) = setup("codex_exec_approval").await else { return };
    h.codex_scenario(json!([[
        { "exec": { "command": "bash -lc 'rm -rf build'", "output": "removed" } },
        { "text": "cleaned up" },
        { "complete": {} },
    ]]));
    h.start();
    let bot = codex_bot(&h, "codex-sheller").await;
    sqlx::query("insert into rules (owner_id, bot_id, pattern, decision) values ($1, null, 'Bash', 'allow')")
        .bind(h.owner)
        .execute(&h.pool)
        .await
        .unwrap();
    let thread = h.thread(bot).await;
    let run = h.say(thread, "clean the build").await;
    let (approval, tool_name, input, reason) = h.pending_approval(run).await;
    assert_eq!(tool_name, "Bash");
    assert_eq!(input["command"], "rm -rf build");
    assert!(reason.as_deref().unwrap_or_default().starts_with("Always needs you"), "{reason:?}");
    // Codex answers accept / decline only: nothing to edit; and an always-human action is never always allowed.
    assert_eq!(h.offer(approval).await, (vec![], None));
    h.decide(approval, "approved", None).await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));
    let decisions: Vec<Value> = h.log_of("fake-codex").into_iter().filter(|e| e["event"] == "approval").map(|e| e["decision"].clone()).collect();
    assert_eq!(decisions, vec![json!("accept")]);
    let ev = h.events(run).await;
    let call = ev.iter().find(|e| e.1 == "tool_call").expect("tool_call event");
    assert_eq!((call.2["name"].as_str(), call.2["input"]["command"].as_str()), (Some("Bash"), Some("rm -rf build")));
    let result = ev.iter().find(|e| e.1 == "tool_result").expect("tool_result event");
    assert_eq!((result.2["content"].as_str(), result.2["is_error"].as_bool()), (Some("removed"), Some(false)));
    h.finish().await;
}

/// Codex lost thread ("no rollout found") → a fresh thread seeded with the conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_lost_thread_restarts_seeded() {
    let Some(mut h) = setup("codex_lost_thread_restarts_seeded").await else { return };
    h.codex_scenario(json!([[{ "no_rollout": {} }], [{ "text": "fresh start" }, { "complete": {} }]]));
    h.start();
    let bot = codex_bot(&h, "codex-forgetful").await;
    let thread = h.thread(bot).await;
    sqlx::query("update threads set codex_thread_id = 'thr_gone' where id = $1").bind(thread).execute(&h.pool).await.unwrap();
    let run = h.say(thread, "where were we").await;
    assert_eq!(h.finished(run).await, ("succeeded".into(), None));
    assert_eq!(codex_requests(&h, 0, "thread/resume")[0]["threadId"], "thr_gone");
    let codex_thread: Option<String> =
        sqlx::query_scalar("select codex_thread_id from threads where id = $1").bind(thread).fetch_one(&h.pool).await.unwrap();
    assert_eq!(codex_thread.as_deref(), Some("thr_fake_1"));
    let prompt = codex_requests(&h, 1, "turn/start")[0]["input"][0]["text"].as_str().unwrap().to_owned();
    assert!(prompt.starts_with("(Your previous session was lost"), "{prompt}");
    assert!(prompt.ends_with("(New message:)\nwhere were we"), "{prompt}");
    h.finish().await;
}
