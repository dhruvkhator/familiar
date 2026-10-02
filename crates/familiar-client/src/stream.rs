//! Live updates: one SSE connection (`GET /api/stream?token=`) as an async `Stream` of
//! [`LiveEvent`]s with auto-reconnect, plus a [`Coalescer`] for bursty notices.

use crate::client::Client;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A change notice: table `t` (runs, approvals, bots, messages, ...), the row `id`, and scoping ids.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Notice {
    pub t: String,
    pub id: Option<String>,
    pub op: Option<String>,
    pub run: Option<String>,
    pub bot: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeltaKind {
    Text,
    Thinking,
    #[default]
    #[serde(other)]
    Other,
}

/// Best-effort live token delta (never persisted).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Delta {
    pub run: String,
    pub kind: DeltaKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    Notice(Notice),
    Delta(Delta),
    /// Refetch everything you show: emitted after every reconnect and when the server says so.
    Resync,
}

// ---- SSE parsing ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    pub event: String,
    pub data: String,
}

/// Incremental parser for `text/event-stream`: CRLF/LF/CR line ends, `:` comments, multi-line `data`.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    started: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns every message completed by it.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseMessage> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let rest = &self.buf[pos..];
            let Some(i) = rest.iter().position(|&b| b == b'\n' || b == b'\r') else { break };
            let mut end = i + 1;
            if rest[i] == b'\r' {
                match rest.get(i + 1) {
                    Some(b'\n') => end += 1,
                    None => break, // might be the first half of CRLF
                    _ => {}
                }
            }
            let line = String::from_utf8_lossy(&rest[..i]).into_owned();
            pos += end;
            self.line(&line, &mut out);
        }
        self.buf.drain(..pos);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseMessage>) {
        if !self.started {
            self.started = true;
            // tolerate a UTF-8 BOM at the very start
            if let Some(rest) = line.strip_prefix('\u{feff}') {
                return self.line(rest, out);
            }
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                out.push(SseMessage { event: self.event.take().unwrap_or_else(|| "message".into()), data: self.data.join("\n") });
            }
            self.event = None;
            self.data.clear();
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {} // id / retry / unknown: ignored
        }
    }
}

/// Map a raw SSE message to a [`LiveEvent`]; unknown event names and malformed payloads are dropped.
pub fn to_live_event(m: &SseMessage) -> Option<LiveEvent> {
    match m.event.as_str() {
        "notice" => serde_json::from_str(&m.data).ok().map(LiveEvent::Notice),
        "delta" => serde_json::from_str(&m.data).ok().map(LiveEvent::Delta),
        "resync" => Some(LiveEvent::Resync),
        _ => None,
    }
}

// ---- reconnecting stream ---------------------------------------------------

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// A connection that stayed up this long resets the backoff.
const HEALTHY: Duration = Duration::from_secs(5);

struct St {
    client: Client,
    body: Option<BoxStream<'static, Result<Bytes, reqwest::Error>>>,
    parser: SseParser,
    queue: VecDeque<LiveEvent>,
    backoff: Duration,
    wait: Option<Duration>,
    connected_at: Option<Instant>,
    ever_connected: bool,
    done: bool,
}

impl Client {
    /// The live event stream. Connects lazily on first poll, reconnects with backoff 0.5 s to 10 s,
    /// and yields [`LiveEvent::Resync`] after every reconnect (not the first connect).
    /// Ends only if the server rejects the token (401); drop it to disconnect.
    /// Each live notice also calls [`Client::invalidate_requests`], like the web client.
    pub fn stream(&self) -> impl Stream<Item = LiveEvent> + Send + 'static {
        let st = St {
            client: self.clone(),
            body: None,
            parser: SseParser::new(),
            queue: VecDeque::new(),
            backoff: BACKOFF_MIN,
            wait: None,
            connected_at: None,
            ever_connected: false,
            done: false,
        };
        futures_util::stream::unfold(st, |mut st| async move {
            loop {
                if let Some(ev) = st.queue.pop_front() {
                    return Some((ev, st));
                }
                if st.done {
                    return None;
                }
                if st.body.is_none() {
                    if let Some(w) = st.wait.take() {
                        tokio::time::sleep(w).await;
                    }
                    let url = st.client.token_url("/api/stream", &[]);
                    let resp = st.client.0.stream_http.get(url).header("Accept", "text/event-stream").send().await;
                    match resp {
                        Ok(r) if r.status().is_success() => {
                            st.body = Some(r.bytes_stream().boxed());
                            st.parser = SseParser::new();
                            st.connected_at = Some(Instant::now());
                            if std::mem::replace(&mut st.ever_connected, true) {
                                st.client.invalidate_requests();
                                st.queue.push_back(LiveEvent::Resync);
                            }
                        }
                        Ok(r) if r.status().as_u16() == 401 => {
                            st.done = true;
                        }
                        other => {
                            tracing::debug!("stream connect failed: {:?}", other.map(|r| r.status()));
                            st.wait = Some(st.backoff);
                            st.backoff = (st.backoff * 2).min(BACKOFF_MAX);
                        }
                    }
                    continue;
                }
                match st.body.as_mut().unwrap().next().await {
                    Some(Ok(chunk)) => {
                        for m in st.parser.feed(&chunk) {
                            if let Some(ev) = to_live_event(&m) {
                                if !matches!(ev, LiveEvent::Delta(_)) {
                                    st.client.invalidate_requests();
                                }
                                st.queue.push_back(ev);
                            }
                        }
                    }
                    Some(Err(_)) | None => {
                        st.body = None;
                        if st.connected_at.take().is_some_and(|t| t.elapsed() >= HEALTHY) {
                            st.backoff = BACKOFF_MIN;
                        }
                        st.wait = Some(st.backoff);
                        st.backoff = (st.backoff * 2).min(BACKOFF_MAX);
                    }
                }
            }
        })
    }
}

// ---- coalescer ---------------------------------------------------------------

/// Trailing-edge debouncer per key: the first `push` for a key opens a window; further pushes in
/// the window are merged into the pending value; when it closes the handler fires once.
/// The web UI uses 150 ms and "a resync wins over plain notices" as the merge.
/// Timers run on the tokio runtime current at construction (or the given handle).
pub struct Coalescer<K, V> {
    window: Duration,
    pending: Arc<Mutex<HashMap<K, V>>>,
    merge: Arc<dyn Fn(V, V) -> V + Send + Sync>,
    handler: Arc<dyn Fn(K, V) + Send + Sync>,
    rt: tokio::runtime::Handle,
}

impl<K, V> Coalescer<K, V>
where
    K: Hash + Eq + Clone + Send + 'static,
    V: Send + 'static,
{
    pub const DEFAULT_WINDOW: Duration = Duration::from_millis(150);

    /// `merge(old, new)` combines values pushed within one window. Must be called inside a tokio runtime.
    pub fn new(
        window: Duration,
        merge: impl Fn(V, V) -> V + Send + Sync + 'static,
        handler: impl Fn(K, V) + Send + Sync + 'static,
    ) -> Self {
        Self::with_handle(tokio::runtime::Handle::current(), window, merge, handler)
    }

    pub fn with_handle(
        rt: tokio::runtime::Handle,
        window: Duration,
        merge: impl Fn(V, V) -> V + Send + Sync + 'static,
        handler: impl Fn(K, V) + Send + Sync + 'static,
    ) -> Self {
        Self { window, pending: Arc::default(), merge: Arc::new(merge), handler: Arc::new(handler), rt }
    }

    pub fn push(&self, key: K, value: V) {
        let mut p = self.pending.lock().unwrap();
        if let Some(old) = p.remove(&key) {
            p.insert(key, (self.merge)(old, value));
            return;
        }
        p.insert(key.clone(), value);
        drop(p);
        let (pending, handler, window) = (self.pending.clone(), self.handler.clone(), self.window);
        self.rt.spawn(async move {
            tokio::time::sleep(window).await;
            let v = pending.lock().unwrap().remove(&key);
            if let Some(v) = v {
                handler(key, v);
            }
        });
    }
}

impl<K, V> Coalescer<K, V>
where
    K: Hash + Eq + Clone + Send + 'static,
    V: Send + 'static,
{
    /// Latest value wins.
    pub fn latest(window: Duration, handler: impl Fn(K, V) + Send + Sync + 'static) -> Self {
        Self::new(window, |_, new| new, handler)
    }
}

/// Merge for `Option<Notice>` coalescing as the web UI does it: `None` (a resync) wins over
/// plain notices, otherwise the latest notice wins.
pub fn resync_wins(old: Option<Notice>, new: Option<Notice>) -> Option<Notice> {
    if old.is_none() || new.is_none() { None } else { new }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn feed_all(chunks: &[&[u8]]) -> Vec<SseMessage> {
        let mut p = SseParser::new();
        chunks.iter().flat_map(|c| p.feed(c)).collect()
    }

    #[test]
    fn parses_event_and_data() {
        let m = feed_all(&[b"event: notice\ndata: {\"t\":\"runs\"}\n\n"]);
        assert_eq!(m, vec![SseMessage { event: "notice".into(), data: "{\"t\":\"runs\"}".into() }]);
    }

    #[test]
    fn multi_line_data_joins_with_newline() {
        let m = feed_all(&[b"data: a\ndata: b\ndata:c\n\n"]);
        assert_eq!(m[0].data, "a\nb\nc");
        assert_eq!(m[0].event, "message");
    }

    #[test]
    fn comments_and_pings_are_ignored() {
        let m = feed_all(&[b": keep-alive\n\n:ping\n\nevent: resync\ndata: {}\n\n"]);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].event, "resync");
    }

    #[test]
    fn split_chunks_crlf_and_utf8() {
        let full = "event: delta\r\ndata: {\"text\":\"h\u{e9}llo\"}\r\n\r\n".as_bytes().to_vec();
        for cut in 1..full.len() {
            let m = feed_all(&[&full[..cut], &full[cut..]]);
            assert_eq!(m.len(), 1, "cut at {cut}");
            assert_eq!(m[0].event, "delta");
            assert!(m[0].data.contains("h\u{e9}llo"));
        }
    }

    #[test]
    fn event_name_resets_between_messages() {
        let m = feed_all(&[b"event: a\ndata: 1\n\ndata: 2\n\n"]);
        assert_eq!(m[0].event, "a");
        assert_eq!(m[1].event, "message");
    }

    #[test]
    fn live_event_mapping() {
        let n = to_live_event(&SseMessage { event: "notice".into(), data: r#"{"t":"runs","id":"1","op":"UPDATE","run":"r","bot":"b","extra":1}"#.into() });
        assert_eq!(n, Some(LiveEvent::Notice(Notice { t: "runs".into(), id: Some("1".into()), op: Some("UPDATE".into()), run: Some("r".into()), bot: Some("b".into()) })));
        let d = to_live_event(&SseMessage { event: "delta".into(), data: r#"{"run":"r","kind":"thinking","text":"hm"}"#.into() });
        assert_eq!(d, Some(LiveEvent::Delta(Delta { run: "r".into(), kind: DeltaKind::Thinking, text: "hm".into() })));
        assert_eq!(to_live_event(&SseMessage { event: "resync".into(), data: "{}".into() }), Some(LiveEvent::Resync));
        assert_eq!(to_live_event(&SseMessage { event: "wat".into(), data: "{}".into() }), None);
        assert_eq!(to_live_event(&SseMessage { event: "notice".into(), data: "nope".into() }), None);
    }

    #[tokio::test(start_paused = true)]
    async fn coalescer_trails_and_merges_per_key() {
        let seen: Arc<Mutex<Vec<(&'static str, u32)>>> = Arc::default();
        let s2 = seen.clone();
        let c = Coalescer::new(Duration::from_millis(150), |a: u32, b: u32| a + b, move |k, v| s2.lock().unwrap().push((k, v)));
        c.push("a", 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.push("a", 2);
        c.push("b", 10);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(seen.lock().unwrap().is_empty(), "nothing before the window closes");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(*seen.lock().unwrap(), vec![("a", 3)]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(seen.lock().unwrap().len(), 2);
        // a new window opens after firing
        c.push("a", 5);
        tokio::time::sleep(Duration::from_millis(151)).await;
        assert_eq!(seen.lock().unwrap().last(), Some(&("a", 5)));
    }

    #[tokio::test(start_paused = true)]
    async fn coalescer_latest_wins() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let c = Coalescer::latest(Coalescer::<u8, usize>::DEFAULT_WINDOW, move |_k: u8, v: usize| n2.store(v, Ordering::SeqCst));
        for i in 1..=5 {
            c.push(0, i);
        }
        tokio::time::sleep(Duration::from_millis(160)).await;
        assert_eq!(n.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn resync_beats_notice() {
        let n = Some(Notice { t: "runs".into(), ..Default::default() });
        assert_eq!(resync_wins(n.clone(), None), None);
        assert_eq!(resync_wins(None, n.clone()), None);
        assert_eq!(resync_wins(n.clone(), n.clone()), n);
    }
}
