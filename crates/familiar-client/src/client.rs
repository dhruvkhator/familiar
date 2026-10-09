use crate::cache::Cache;
use crate::error::ApiError;
use crate::types::*;
use bytes::Bytes;
use futures_util::FutureExt;
use reqwest::Method;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use uuid::Uuid;

/// Called with `true` when a request has been outstanding for more than 3 s ("waking server")
/// and with `false` once no slow request remains.
pub type WakingCallback = Arc<dyn Fn(bool) + Send + Sync>;

const WAKING_AFTER: Duration = Duration::from_secs(3);

pub(crate) struct Inner {
    pub base_url: String,
    pub token: RwLock<Option<String>>,
    pub http: reqwest::Client,
    /// No total timeout: used for the SSE connection.
    pub stream_http: reqwest::Client,
    pub cache: Cache,
    slow: AtomicUsize,
    waking: RwLock<Option<WakingCallback>>,
}

/// Cheap to clone (shared state). All methods are async and need a tokio runtime.
#[derive(Clone)]
pub struct Client(pub(crate) Arc<Inner>);

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("base_url", &self.0.base_url).finish_non_exhaustive()
    }
}

fn qs(params: &[(&str, Option<String>)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    let mut any = false;
    for (k, v) in params {
        if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
            s.append_pair(k, v);
            any = true;
        }
    }
    if any { format!("?{}", s.finish()) } else { String::new() }
}

fn crm_list_qs(p: &CrmListParams) -> String {
    qs(&[
        ("q", p.q.clone()),
        ("tag", p.tag.clone()),
        ("stage", p.stage.clone()),
        ("company_id", p.company_id.map(|v| v.to_string())),
        ("dnc", p.dnc.map(|v| v.to_string())),
        ("sort", p.sort.clone()),
        ("limit", p.limit.map(|v| v.to_string())),
        ("offset", p.offset.map(|v| v.to_string())),
    ])
}

fn body<B: Serialize>(b: &B) -> Value {
    serde_json::to_value(b).unwrap_or(Value::Null)
}

impl Client {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("reqwest client");
        let stream_http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build().expect("reqwest client");
        Client(Arc::new(Inner {
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            token: RwLock::new(token),
            http,
            stream_http,
            cache: Cache::default(),
            slow: AtomicUsize::new(0),
            waking: RwLock::new(None),
        }))
    }

    pub fn base_url(&self) -> &str {
        &self.0.base_url
    }
    pub fn token(&self) -> Option<String> {
        self.0.token.read().unwrap().clone()
    }
    /// Replace (or clear) the token. Drops every cache: cached rows belong to the previous user.
    pub fn set_token(&self, token: Option<String>) {
        *self.0.token.write().unwrap() = token;
        self.0.cache.clear_all();
    }
    pub fn set_waking_callback(&self, cb: Option<WakingCallback>) {
        *self.0.waking.write().unwrap() = cb;
    }
    /// True while any request has been outstanding for more than 3 s.
    pub fn is_waking(&self) -> bool {
        self.0.slow.load(Ordering::SeqCst) > 0
    }

    fn bump(&self, up: bool) {
        let now = if up { self.0.slow.fetch_add(1, Ordering::SeqCst) + 1 } else { self.0.slow.fetch_sub(1, Ordering::SeqCst) - 1 };
        if (up && now == 1) || (!up && now == 0) {
            if let Some(cb) = self.0.waking.read().unwrap().clone() {
                cb(up);
            }
        }
    }

    // ---- transport ------------------------------------------------------

    /// `text`: a raw `text/csv` body instead of JSON.
    async fn send_once(&self, method: &Method, path: &str, body: Option<&Value>, text: Option<&str>, auth: bool) -> Result<(u16, Bytes), ApiError> {
        let mut req = self.0.http.request(method.clone(), format!("{}{}", self.0.base_url, path));
        if let Some(t) = self.token() {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        if let Some(t) = text {
            req = req.header(reqwest::header::CONTENT_TYPE, "text/csv").body(t.to_string());
        }
        let fut = req.send();
        tokio::pin!(fut);
        let mut counted = false;
        let res = tokio::select! {
            r = &mut fut => r,
            _ = tokio::time::sleep(WAKING_AFTER) => { counted = true; self.bump(true); fut.await }
        };
        let out = match res {
            Ok(r) => {
                let status = r.status().as_u16();
                r.bytes().await.map(|b| (status, b)).map_err(|e| ApiError::Network(e.to_string()))
            }
            Err(e) => Err(ApiError::Network(e.to_string())),
        };
        if counted {
            self.bump(false);
        }
        let (status, bytes) = out?;
        if (200..300).contains(&status) {
            return Ok((status, bytes));
        }
        let text = String::from_utf8_lossy(&bytes);
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())))
            .or_else(|| (!text.trim().is_empty()).then(|| text.trim().to_string()))
            .unwrap_or_else(|| format!("Request failed ({status})"));
        if status == 401 && auth {
            self.set_token(None);
            return Err(ApiError::Unauthorized);
        }
        Err(ApiError::Http { status, message })
    }

    /// GETs are retried once on a network error.
    async fn send(&self, method: Method, path: &str, body: Option<&Value>, auth: bool) -> Result<(u16, Bytes), ApiError> {
        let attempts = if method == Method::GET { 2 } else { 1 };
        let mut i = 1;
        loop {
            match self.send_once(&method, path, body, None, auth).await {
                Err(ApiError::Network(e)) if i < attempts => {
                    tracing::debug!("retrying {path}: {e}");
                    i += 1;
                }
                other => return other,
            }
        }
    }

    fn parse_value(bytes: &Bytes) -> Value {
        if bytes.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
    }
    fn decode<T: DeserializeOwned>(v: Value) -> Result<T, ApiError> {
        serde_json::from_value(v).map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn mutate<T: DeserializeOwned>(&self, method: Method, path: &str, b: Option<Value>, auth: bool) -> Result<T, ApiError> {
        let (_, bytes) = self.send(method, path, b.as_ref(), auth).await?;
        self.0.cache.clear_inflight();
        Self::decode(Self::parse_value(&bytes))
    }

    // ---- GET cache ------------------------------------------------------

    /// GET with in-flight de-duplication; updates the cache. Returns the raw JSON.
    pub async fn get_value(&self, path: &str) -> Result<Value, ApiError> {
        let flight = {
            let mut inflight = self.0.cache.inflight.lock().unwrap();
            if let Some(f) = inflight.get(path) {
                f.clone()
            } else {
                let me = self.clone();
                let key = path.to_string();
                let f = async move {
                    let r = me.send(Method::GET, &key, None, true).await.map(|(_, b)| Self::parse_value(&b));
                    me.0.cache.inflight.lock().unwrap().remove(&key);
                    if let Ok(v) = &r {
                        me.0.cache.values.lock().unwrap().insert(key, v.clone());
                    }
                    r
                }
                .boxed()
                .shared();
                inflight.insert(path.to_string(), f.clone());
                f
            }
        };
        flight.await
    }

    /// Fresh typed GET (de-duplicated, refreshes the cache).
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        Self::decode(self.get_value(path).await?)
    }

    /// The cached value for `path`, if any (never hits the network).
    pub fn peek<T: DeserializeOwned>(&self, path: &str) -> Option<T> {
        let v = self.0.cache.values.lock().unwrap().get(path).cloned()?;
        serde_json::from_value(v).ok()
    }

    /// Stale-while-revalidate: calls `on_cached` right away with the cached value (if any),
    /// then fetches and returns the fresh one. The caller renders the callback value
    /// immediately and the return value when the future resolves.
    pub async fn cached_get<T: DeserializeOwned>(&self, path: &str, on_cached: impl FnOnce(T)) -> Result<T, ApiError> {
        if let Some(c) = self.peek::<T>(path) {
            on_cached(c);
        }
        self.get(path).await
    }

    /// Drop cached entries and in-flight requests whose path starts with `prefix`.
    pub fn invalidate(&self, prefix: &str) {
        self.0.cache.invalidate_prefix(prefix);
    }
    /// Forget in-flight GETs so the next read hits the server (call on every live notice).
    pub fn invalidate_requests(&self) {
        self.0.cache.clear_inflight();
    }
    pub fn clear_cache(&self) {
        self.0.cache.clear_all();
    }

    /// Warm the cache for these paths concurrently; failures are ignored.
    pub async fn prefetch(&self, paths: &[String]) {
        futures_util::future::join_all(paths.iter().map(|p| self.get_value(p))).await;
    }
    pub fn bot_prefetch_paths(bot: Uuid) -> Vec<String> {
        vec![format!("/api/bots/{bot}/threads"), format!("/api/bots/{bot}/runs?limit=1")]
    }
    pub fn thread_prefetch_paths(thread: Uuid) -> Vec<String> {
        vec![format!("/api/threads/{thread}/messages?limit=200"), format!("/api/threads/{thread}/runs?limit=5")]
    }

    // ---- auth -----------------------------------------------------------

    pub async fn auth_state(&self) -> Result<AuthState, ApiError> {
        Self::decode(self.send(Method::GET, "/api/auth/state", None, false).await.map(|(_, b)| Self::parse_value(&b))?)
    }
    /// First-run owner creation; stores the returned token.
    pub async fn setup(&self, email: &str, password: &str) -> Result<Session, ApiError> {
        self.credentials("/api/auth/setup", email, password).await
    }
    /// Signs in and stores the returned token.
    pub async fn login(&self, email: &str, password: &str) -> Result<Session, ApiError> {
        self.credentials("/api/auth/login", email, password).await
    }
    async fn credentials(&self, path: &str, email: &str, password: &str) -> Result<Session, ApiError> {
        let s: Session = self.mutate(Method::POST, path, Some(json!({"email": email, "password": password})), false).await?;
        self.set_token(Some(s.token.clone()));
        Ok(s)
    }
    pub async fn logout(&self) -> Result<(), ApiError> {
        let r = self.mutate::<Value>(Method::POST, "/api/auth/logout", Some(json!({})), false).await;
        self.set_token(None);
        r.map(|_| ())
    }
    pub async fn me(&self) -> Result<User, ApiError> {
        self.get("/api/me").await
    }
    pub async fn update_account(&self, u: &AccountUpdate) -> Result<User, ApiError> {
        self.mutate(Method::PATCH, "/api/auth/account", Some(body(u)), true).await
    }

    // ---- overview & bots ------------------------------------------------

    pub async fn overview(&self) -> Result<Overview, ApiError> {
        self.get("/api/overview").await
    }
    pub async fn bots(&self) -> Result<Vec<Bot>, ApiError> {
        self.get("/api/bots").await
    }
    pub async fn bot(&self, id: Uuid) -> Result<Bot, ApiError> {
        self.get(&format!("/api/bots/{id}")).await
    }
    pub async fn create_bot(&self, b: &NewBot) -> Result<Bot, ApiError> {
        self.mutate(Method::POST, "/api/bots", Some(body(b)), true).await
    }
    pub async fn update_bot(&self, id: Uuid, p: &BotPatch) -> Result<Bot, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/bots/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_bot(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/bots/{id}"), None, true).await.map(|_| ())
    }

    /// Tick a template login or hide the Set up checklist.
    pub async fn update_bot_setup(&self, id: Uuid, p: &SetupPatch) -> Result<Bot, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/bots/{id}/setup"), Some(body(p)), true).await
    }

    // ---- templates ------------------------------------------------------

    pub async fn templates(&self) -> Result<Vec<Template>, ApiError> {
        self.get("/api/templates").await
    }
    /// Hire a teammate from a template: the bot, its schedules (off) and its Set up checklist, in one go.
    pub async fn create_from_template(&self, id: &str, b: &FromTemplate) -> Result<Hired, ApiError> {
        let id = url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>();
        self.mutate(Method::POST, &format!("/api/templates/{id}/create"), Some(body(b)), true).await
    }

    // ---- threads & messages ---------------------------------------------

    pub async fn threads(&self, bot: Uuid) -> Result<Vec<Thread>, ApiError> {
        self.get(&format!("/api/bots/{bot}/threads")).await
    }
    pub async fn create_thread(&self, bot: Uuid, title: Option<&str>) -> Result<Thread, ApiError> {
        self.mutate(Method::POST, &format!("/api/bots/{bot}/threads"), Some(json!({"title": title})), true).await
    }
    pub async fn rename_thread(&self, id: Uuid, title: &str) -> Result<Thread, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/threads/{id}"), Some(json!({"title": title})), true).await
    }
    pub async fn delete_thread(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/threads/{id}"), None, true).await.map(|_| ())
    }
    /// The newest `limit` messages older than `before` (RFC 3339 `created_at`), oldest first.
    pub async fn messages(&self, thread: Uuid, before: Option<&str>, limit: Option<u32>) -> Result<Vec<Message>, ApiError> {
        let q = qs(&[("before", before.map(str::to_string)), ("limit", limit.map(|l| l.to_string()))]);
        self.get(&format!("/api/threads/{thread}/messages{q}")).await
    }
    /// Posting a user message queues a chat run on the server.
    pub async fn post_message(&self, thread: Uuid, content: &str) -> Result<Message, ApiError> {
        self.mutate(Method::POST, &format!("/api/threads/{thread}/messages"), Some(json!({"content": content})), true).await
    }

    // ---- runs -----------------------------------------------------------

    pub async fn thread_runs(&self, thread: Uuid, limit: Option<u32>, before: Option<&str>) -> Result<Vec<Run>, ApiError> {
        let q = qs(&[("limit", limit.map(|l| l.to_string())), ("before", before.map(str::to_string))]);
        self.get(&format!("/api/threads/{thread}/runs{q}")).await
    }
    pub async fn bot_runs(&self, bot: Uuid, limit: Option<u32>, before: Option<&str>) -> Result<Vec<Run>, ApiError> {
        let q = qs(&[("limit", limit.map(|l| l.to_string())), ("before", before.map(str::to_string))]);
        self.get(&format!("/api/bots/{bot}/runs{q}")).await
    }
    pub async fn run(&self, id: Uuid) -> Result<Run, ApiError> {
        self.get(&format!("/api/runs/{id}")).await
    }
    pub async fn run_events(&self, id: Uuid, after_seq: Option<i32>, limit: Option<u32>) -> Result<Vec<Event>, ApiError> {
        let q = qs(&[("after_seq", after_seq.map(|s| s.to_string())), ("limit", limit.map(|l| l.to_string()))]);
        self.get(&format!("/api/runs/{id}/events{q}")).await
    }
    pub async fn cancel_run(&self, id: Uuid) -> Result<Run, ApiError> {
        self.mutate(Method::POST, &format!("/api/runs/{id}/cancel"), Some(json!({})), true).await
    }

    // ---- approvals & rules ----------------------------------------------

    /// `status`: pending | approved | denied | expired | revise (none = all).
    pub async fn approvals(&self, status: Option<&str>, bot: Option<Uuid>) -> Result<Vec<Approval>, ApiError> {
        let q = qs(&[("status", status.map(str::to_string)), ("bot_id", bot.map(|b| b.to_string()))]);
        self.get(&format!("/api/approvals{q}")).await
    }
    /// `response` carries the answer to an `ask_user` question or a note on a denial.
    pub async fn decide_approval(&self, id: Uuid, approve: bool, response: Option<&str>) -> Result<Approval, ApiError> {
        let mut b = json!({"decision": if approve { "approve" } else { "deny" }});
        if let Some(r) = response {
            b["response"] = json!(r);
        }
        self.mutate(Method::POST, &format!("/api/approvals/{id}"), Some(b), true).await
    }
    /// Any decision: edits, a note, ask for changes, always allow (see [`ApprovalDecision`]).
    pub async fn decide(&self, id: Uuid, d: &ApprovalDecision) -> Result<Decided, ApiError> {
        self.mutate(Method::POST, &format!("/api/approvals/{id}"), Some(body(d)), true).await
    }
    /// No `bot`: global rules; `bot`: that bot's own rules; `all`: everything.
    pub async fn rules(&self, bot: Option<Uuid>, all: bool) -> Result<Vec<Rule>, ApiError> {
        let q = qs(&[("bot_id", bot.map(|b| b.to_string())), ("all", all.then(|| "1".to_string()))]);
        self.get(&format!("/api/rules{q}")).await
    }
    pub async fn create_rule(&self, r: &NewRule) -> Result<Rule, ApiError> {
        self.mutate(Method::POST, "/api/rules", Some(body(r)), true).await
    }
    pub async fn delete_rule(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/rules/{id}"), None, true).await.map(|_| ())
    }

    // ---- desktop control ------------------------------------------------

    /// "Stop desktop control": deny every waiting desktop request and take the desktop from the teammate using it.
    pub async fn stop_desktop(&self) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::POST, "/api/desktop/stop", Some(serde_json::json!({})), true).await.map(|_| ())
    }

    // ---- shared folders -------------------------------------------------

    pub async fn folders(&self, bot: Uuid) -> Result<Vec<Folder>, ApiError> {
        self.get(&format!("/api/bots/{bot}/folders")).await
    }
    /// Share a folder (`mode`: read | write); the API refuses dangerous ones with a plain-words reason.
    pub async fn add_folder(&self, bot: Uuid, path: &str, mode: &str) -> Result<Folder, ApiError> {
        let b = serde_json::json!({ "path": path, "mode": mode });
        self.mutate(Method::POST, &format!("/api/bots/{bot}/folders"), Some(b), true).await
    }
    pub async fn set_folder_mode(&self, id: Uuid, mode: &str) -> Result<Folder, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/folders/{id}"), Some(serde_json::json!({ "mode": mode })), true).await
    }
    pub async fn delete_folder(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/folders/{id}"), None, true).await.map(|_| ())
    }

    // ---- schedules ------------------------------------------------------

    pub async fn schedules(&self, bot: Uuid) -> Result<Vec<Schedule>, ApiError> {
        self.get(&format!("/api/bots/{bot}/schedules")).await
    }
    /// Every teammate's schedules, with label, teammate name and last run.
    pub async fn all_schedules(&self) -> Result<Vec<Schedule>, ApiError> {
        self.get("/api/schedules").await
    }
    /// Queue a schedule's run now (409 while one is still queued).
    pub async fn run_schedule(&self, id: Uuid) -> Result<Run, ApiError> {
        self.mutate(Method::POST, &format!("/api/schedules/{id}/run"), Some(json!({})), true).await
    }
    pub async fn create_schedule(&self, bot: Uuid, s: &NewSchedule) -> Result<Schedule, ApiError> {
        self.mutate(Method::POST, &format!("/api/bots/{bot}/schedules"), Some(body(s)), true).await
    }
    pub async fn update_schedule(&self, id: Uuid, p: &SchedulePatch) -> Result<Schedule, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/schedules/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_schedule(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/schedules/{id}"), None, true).await.map(|_| ())
    }

    // ---- memories & skills ----------------------------------------------

    /// `status`: active | proposed | rejected (none = all).
    pub async fn memories(&self, bot: Uuid, status: Option<&str>) -> Result<Vec<Memory>, ApiError> {
        let q = qs(&[("status", status.map(str::to_string))]);
        self.get(&format!("/api/bots/{bot}/memories{q}")).await
    }
    pub async fn create_memory(&self, bot: Uuid, content: &str) -> Result<Memory, ApiError> {
        self.mutate(Method::POST, &format!("/api/bots/{bot}/memories"), Some(json!({"content": content})), true).await
    }
    pub async fn update_memory(&self, id: Uuid, p: &MemoryPatch) -> Result<Memory, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/memories/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_memory(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/memories/{id}"), None, true).await.map(|_| ())
    }
    /// Queue a "dream" (memory consolidation) run; returns its run id.
    pub async fn dream(&self, bot: Uuid) -> Result<Uuid, ApiError> {
        let v: Value = self.mutate(Method::POST, &format!("/api/bots/{bot}/dream"), Some(json!({})), true).await?;
        v.get("run_id").and_then(Value::as_str).and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::Decode("no run_id".into()))
    }
    pub async fn skills(&self, bot: Uuid) -> Result<Vec<Skill>, ApiError> {
        self.get(&format!("/api/bots/{bot}/skills")).await
    }

    // ---- artifacts ------------------------------------------------------

    pub async fn bot_artifacts(&self, bot: Uuid) -> Result<Vec<Artifact>, ApiError> {
        self.get(&format!("/api/bots/{bot}/artifacts")).await
    }
    pub async fn run_artifacts(&self, run: Uuid) -> Result<Vec<Artifact>, ApiError> {
        self.get(&format!("/api/runs/{run}/artifacts")).await
    }
    /// A waiting desktop step's picture of the screen around its target (PNG).
    pub async fn approval_preview(&self, id: Uuid) -> Result<Bytes, ApiError> {
        self.send(Method::GET, &format!("/api/approvals/{id}/preview"), None, true).await.map(|(_, b)| b)
    }
    pub async fn download_artifact(&self, id: Uuid) -> Result<Bytes, ApiError> {
        self.send(Method::GET, &format!("/api/artifacts/{id}/download"), None, true).await.map(|(_, b)| b)
    }
    /// URL an `<img>`-style loader can fetch directly (token in the query).
    pub fn artifact_url(&self, id: Uuid) -> String {
        self.token_url(&format!("/api/artifacts/{id}/download"), &[])
    }

    // ---- connectors -----------------------------------------------------

    pub async fn connectors(&self) -> Result<Vec<Connector>, ApiError> {
        self.get("/api/connectors").await
    }
    pub async fn connector_presets(&self) -> Result<Vec<ConnectorPreset>, ApiError> {
        self.get("/api/connectors/presets").await
    }
    pub async fn create_connector(&self, c: &NewConnector) -> Result<Connector, ApiError> {
        self.mutate(Method::POST, "/api/connectors", Some(body(c)), true).await
    }
    pub async fn update_connector(&self, id: Uuid, p: &ConnectorPatch) -> Result<Connector, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/connectors/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_connector(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/connectors/{id}"), None, true).await.map(|_| ())
    }
    /// Connectors linked to a bot.
    pub async fn bot_connectors(&self, bot: Uuid) -> Result<Vec<Connector>, ApiError> {
        self.get(&format!("/api/bots/{bot}/connectors")).await
    }
    /// Replace the bot's connector set.
    pub async fn set_bot_connectors(&self, bot: Uuid, ids: &[Uuid]) -> Result<Vec<Connector>, ApiError> {
        self.mutate(Method::PUT, &format!("/api/bots/{bot}/connectors"), Some(json!({"connector_ids": ids})), true).await
    }

    // ---- channels & triggers --------------------------------------------

    pub async fn channels(&self) -> Result<Vec<Channel>, ApiError> {
        self.get("/api/channels").await
    }
    pub async fn create_channel(&self, c: &NewChannel) -> Result<Channel, ApiError> {
        self.mutate(Method::POST, "/api/channels", Some(body(c)), true).await
    }
    pub async fn update_channel(&self, id: Uuid, p: &ChannelPatch) -> Result<Channel, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/channels/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_channel(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/channels/{id}"), None, true).await.map(|_| ())
    }
    pub async fn triggers(&self, bot: Uuid) -> Result<Vec<Trigger>, ApiError> {
        self.get(&format!("/api/bots/{bot}/triggers")).await
    }
    pub async fn create_trigger(&self, bot: Uuid, t: &NewTrigger) -> Result<Trigger, ApiError> {
        self.mutate(Method::POST, &format!("/api/bots/{bot}/triggers"), Some(body(t)), true).await
    }
    pub async fn update_trigger(&self, id: Uuid, p: &TriggerPatch) -> Result<Trigger, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/triggers/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_trigger(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/triggers/{id}"), None, true).await.map(|_| ())
    }
    /// Issue a new webhook URL (the old one stops working).
    pub async fn rotate_trigger(&self, id: Uuid) -> Result<Trigger, ApiError> {
        self.mutate(Method::POST, &format!("/api/triggers/{id}/rotate"), Some(json!({})), true).await
    }

    // ---- CRM ------------------------------------------------------------

    pub async fn crm_companies(&self, p: &CrmListParams) -> Result<Vec<CrmCompany>, ApiError> {
        self.get(&format!("/api/crm/companies{}", crm_list_qs(p))).await
    }
    pub async fn crm_company(&self, id: Uuid) -> Result<CrmCompany, ApiError> {
        self.get(&format!("/api/crm/companies/{id}")).await
    }
    /// Creates the company, or updates the one it matches (same domain, else same name).
    pub async fn create_crm_company(&self, c: &NewCompany) -> Result<CrmCompany, ApiError> {
        self.mutate(Method::POST, "/api/crm/companies", Some(body(c)), true).await
    }
    pub async fn update_crm_company(&self, id: Uuid, p: &CompanyPatch) -> Result<CrmCompany, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/crm/companies/{id}"), Some(body(p)), true).await
    }
    /// A soft delete: [`Client::undo_crm_change`] brings it back.
    pub async fn delete_crm_company(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/crm/companies/{id}"), None, true).await.map(|_| ())
    }

    pub async fn crm_contacts(&self, p: &CrmListParams) -> Result<Vec<CrmContact>, ApiError> {
        self.get(&format!("/api/crm/contacts{}", crm_list_qs(p))).await
    }
    pub async fn crm_contact(&self, id: Uuid) -> Result<CrmContact, ApiError> {
        self.get(&format!("/api/crm/contacts/{id}")).await
    }
    /// Creates the contact, or updates the one it matches (same email, else LinkedIn link, else name at the company).
    pub async fn create_crm_contact(&self, c: &NewContact) -> Result<CrmContact, ApiError> {
        self.mutate(Method::POST, "/api/crm/contacts", Some(body(c)), true).await
    }
    pub async fn update_crm_contact(&self, id: Uuid, p: &ContactPatch) -> Result<CrmContact, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/crm/contacts/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_crm_contact(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/crm/contacts/{id}"), None, true).await.map(|_| ())
    }

    pub async fn crm_deals(&self, p: &CrmListParams) -> Result<Vec<CrmDeal>, ApiError> {
        self.get(&format!("/api/crm/deals{}", crm_list_qs(p))).await
    }
    pub async fn crm_deal(&self, id: Uuid) -> Result<CrmDeal, ApiError> {
        self.get(&format!("/api/crm/deals/{id}")).await
    }
    /// Creates the deal, or updates the one at the same company with the same title.
    pub async fn create_crm_deal(&self, d: &NewDeal) -> Result<CrmDeal, ApiError> {
        self.mutate(Method::POST, "/api/crm/deals", Some(body(d)), true).await
    }
    /// Also how a deal moves between stages (`stage`).
    pub async fn update_crm_deal(&self, id: Uuid, p: &DealPatch) -> Result<CrmDeal, ApiError> {
        self.mutate(Method::PATCH, &format!("/api/crm/deals/{id}"), Some(body(p)), true).await
    }
    pub async fn delete_crm_deal(&self, id: Uuid) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::DELETE, &format!("/api/crm/deals/{id}"), None, true).await.map(|_| ())
    }

    /// The timeline, newest first.
    pub async fn crm_activities(&self, p: &CrmActivityParams) -> Result<Vec<CrmActivity>, ApiError> {
        let q = qs(&[
            ("company_id", p.company_id.map(|v| v.to_string())),
            ("contact_id", p.contact_id.map(|v| v.to_string())),
            ("deal_id", p.deal_id.map(|v| v.to_string())),
            ("limit", p.limit.map(|v| v.to_string())),
            ("offset", p.offset.map(|v| v.to_string())),
        ]);
        self.get(&format!("/api/crm/activities{q}")).await
    }
    pub async fn log_crm_activity(&self, a: &NewActivity) -> Result<CrmActivity, ApiError> {
        self.mutate(Method::POST, "/api/crm/activities", Some(body(a)), true).await
    }
    /// The board: every stage in order, with its count, value and deals.
    pub async fn crm_pipeline(&self) -> Result<Vec<PipelineStage>, ApiError> {
        self.get("/api/crm/pipeline").await
    }
    /// The change log, newest first.
    pub async fn crm_changes(&self, p: &CrmChangeParams) -> Result<Vec<CrmChange>, ApiError> {
        let q = qs(&[
            ("entity", p.entity.clone()),
            ("entity_id", p.entity_id.map(|v| v.to_string())),
            ("bot_id", p.bot_id.map(|v| v.to_string())),
            ("limit", p.limit.map(|v| v.to_string())),
        ]);
        self.get(&format!("/api/crm/changes{q}")).await
    }
    /// Undo a change (409 unless it is the newest of its record); answers with the new `undo` change.
    pub async fn undo_crm_change(&self, id: Uuid) -> Result<CrmChange, ApiError> {
        self.mutate(Method::POST, &format!("/api/crm/changes/{id}/undo"), Some(json!({})), true).await
    }
    /// `kind`: companies | contacts | deals. UTF-8 CSV with a header row.
    pub async fn crm_export_csv(&self, kind: &str) -> Result<String, ApiError> {
        let q = qs(&[("kind", Some(kind.to_string()))]);
        let (_, b) = self.send(Method::GET, &format!("/api/crm/export.csv{q}"), None, true).await?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }
    /// Import CSV text (at most 5 MB and 5000 rows). `dry_run` reports what would happen and changes nothing.
    pub async fn crm_import(&self, kind: &str, csv: &str, dry_run: bool) -> Result<CrmImportResult, ApiError> {
        let q = qs(&[("kind", Some(kind.to_string())), ("dry_run", Some(dry_run.to_string()))]);
        let (_, b) = self.send_once(&Method::POST, &format!("/api/crm/import{q}"), None, Some(csv), true).await?;
        self.0.cache.clear_inflight();
        Self::decode(Self::parse_value(&b))
    }

    // ---- live browser view ----------------------------------------------

    pub async fn live_info(&self, bot: Uuid) -> Result<LiveInfo, ApiError> {
        self.get(&format!("/api/bots/{bot}/live")).await
    }
    /// Latest JPEG frame of the bot's browser.
    pub async fn live_frame(&self, bot: Uuid) -> Result<Bytes, ApiError> {
        self.send(Method::GET, &format!("/api/bots/{bot}/live.jpg"), None, true).await.map(|(_, b)| b)
    }
    pub fn live_frame_url(&self, bot: Uuid, ver: u64) -> String {
        self.token_url(&format!("/api/bots/{bot}/live.jpg"), &[("t", ver.to_string())])
    }
    pub async fn live_input(&self, bot: Uuid, input: &LiveInput) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::POST, &format!("/api/bots/{bot}/live/input"), Some(body(input)), true).await.map(|_| ())
    }

    pub(crate) fn token_url(&self, path: &str, extra: &[(&str, String)]) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair("token", &self.token().unwrap_or_default());
        for (k, v) in extra {
            s.append_pair(k, v);
        }
        format!("{}{}?{}", self.0.base_url, path, s.finish())
    }
}
