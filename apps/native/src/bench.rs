//! `familiar-native --bench`: a repeatable lab benchmark of the app shell that needs no engine, network or model.
//!
//! A local fake of the Familiar API (127.0.0.1, an ephemeral port, a fixed 25 ms per answer) serves synthetic
//! teammates, a 60-message thread with lists and code blocks, approvals and schedules; the real [`Shell`] and
//! [`AppData`] run on it. The driver then measures: idle frames on Today, switching to three teammates (click to the
//! frame showing their chat), opening the long thread, idle frames on the chat, and a ~20 000-character reply
//! streamed in 40-character deltas every 16 ms (frame times, UI-thread and process CPU, renders per view, delta to
//! paint). The report goes to `--bench-out <file>` (else stdout); the window quits when done.
//!
//! `--bench-shot today|needs|schedules|chat` only opens that page on the same data and quits after `--bench-hold`
//! seconds (default 8): for before/after screenshots. `crm…` shots open the CRM screens on a synthetic CRM
//! ([`crate::bench_crm`]; [`crm_shot`] lists them) with three of the GTM crew among the teammates. The screens of the
//! integrations, a teammate's connectors, triggers, skills and files, and the rules have their own fake data
//! ([`crate::bench_parity`]; [`integrations_shot`] lists the Integrations shots).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use familiar_client::{
    Approval, ApprovalStatus, Bot, BotEngine, BotStatus, Client, Device, Message, Notice, Overview, Role, Run, RunKind,
    RunStatus, Schedule, Thread,
};
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, IntoElement, Render, Window, WindowHandle};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::data::{AppData, Mode};
use crate::perf;
use crate::shell::{Route, Shell};

/// What the fake API takes to answer (a local API on the built-in database answers in roughly this).
const LATENCY: Duration = Duration::from_millis(25);
const THREAD_MESSAGES: usize = 60;
const REPLY_CHARS: usize = 20_000;
const CHUNK: usize = 40;
const CHUNK_EVERY: Duration = Duration::from_millis(16);
/// Between hovering a teammate and clicking it, as a person does.
const HOVER_TO_CLICK: Duration = Duration::from_millis(250);
const IDLE_WINDOW: Duration = Duration::from_secs(3);

fn id(n: u128) -> Uuid {
    Uuid::from_u128(0xbe4c_0000_0000_4000_8000_0000_0000_0000 | n)
}

const ADA: u128 = 0x1;
const MILO: u128 = 0x2;
const JUNIPER: u128 = 0x3;
const PIP: u128 = 0x4;
const ADA_SHORT: u128 = 0x101;
const ADA_LONG: u128 = 0x102;
const LIVE_RUN: u128 = 0x900;
// The GTM crew in the CRM shots.
const LEAD: u128 = 0x11;
const DRAFTER: u128 = 0x12;
const TRACKER: u128 = 0x13;

fn crew_ids() -> crate::bench_crm::CrewIds {
    crate::bench_crm::CrewIds { lead: id(LEAD), drafter: id(DRAFTER), tracker: id(TRACKER) }
}

pub struct Args {
    pub out: Option<String>,
    pub shot: Option<String>,
    pub hold: u64,
}

// ---- synthetic data ---------------------------------------------------------------------------------------------

/// The fake API's rows; the driver changes them as the scenario runs.
struct Fixture {
    now: DateTime<Utc>,
    bots: Vec<Bot>,
    threads: Vec<Thread>,
    messages: BTreeMap<Uuid, Vec<Message>>,
    /// Newest first.
    runs: Vec<Run>,
    approvals: Vec<Approval>,
    schedules: Vec<Schedule>,
    crm: crate::bench_crm::Crm,
    parity: crate::bench_parity::Parity,
}

fn ago(now: DateTime<Utc>, minutes: i64) -> DateTime<Utc> {
    now - chrono::Duration::minutes(minutes)
}

/// A user turn of the long thread.
fn question(i: usize) -> String {
    const ASKS: [&str; 6] = [
        "Can you draft the launch checklist for Thursday?",
        "What's left before we can ship the pricing page?",
        "Summarise yesterday's feedback from the beta group.",
        "Give me the SQL for weekly active teams, and explain it.",
        "Which of the onboarding emails underperformed, and why?",
        "Turn that into steps I can hand to Milo.",
    ];
    format!("{} (#{})", ASKS[i % ASKS.len()], i / 2 + 1)
}

/// An assistant turn: a paragraph with **bold** and `code`, lists, and every other one a fenced code block.
fn answer(i: usize) -> String {
    let mut s = String::new();
    if i % 3 == 0 {
        let _ = writeln!(s, "## Plan for step {}\n", i / 2 + 1);
    }
    let _ = writeln!(
        s,
        "Here is what I found. The **pricing page** still points at the old `plans.json`, and the checkout test in \
         `e2e/checkout.spec.ts` fails on the annual toggle. Everything else in the {} items we tracked is green, and \
         the copy review came back with two small notes.\n",
        12 + i % 7
    );
    for k in 0..(3 + i % 3) {
        let _ = writeln!(s, "- Item {k}: update `config/{k}.toml` so the **{}** flag is on for the beta group", ["search", "billing", "invites", "export"][k % 4]);
    }
    s.push('\n');
    for k in 1..=(2 + i % 3) {
        let _ = writeln!(s, "{k}. Re-run the suite with `cargo test -p familiar-{k}` and post the result in #launch");
    }
    if i % 2 == 1 {
        s.push_str("\n```rust\n");
        for k in 0..(8 + i % 8) {
            let _ = writeln!(s, "    let total_{k} = rows.iter().filter(|r| r.team_id == team && r.week == {k}).count();");
        }
        s.push_str("```\n");
    }
    let _ = write!(s, "\nShould I go ahead and open a pull request for the first {} of these?", 2 + i % 4);
    s
}

/// The streamed reply: sections like [`answer`] until it is about [`REPLY_CHARS`] long (ASCII, so chunks split anywhere).
fn long_reply() -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < REPLY_CHARS {
        let _ = writeln!(s, "### Part {}\n", i + 1);
        s.push_str(&answer(i));
        s.push_str("\n\n");
        i += 1;
    }
    s.truncate(REPLY_CHARS);
    s
}

impl Fixture {
    /// The data for `page` (`--bench-shot`): `crm-empty` has an empty CRM; the integrations shots pick Telegram's state.
    fn new(page: &str) -> Self {
        let empty_crm = page == "crm-empty";
        let now = Utc::now();
        let bot = |n: u128, name: &str, persona: &str, last: i64| Bot {
            id: id(n),
            slug: name.to_lowercase(),
            name: name.into(),
            persona: Some(persona.into()),
            model: "sonnet".into(),
            engine: BotEngine::Claude,
            status: Some(BotStatus::Idle),
            last_run_at: Some(ago(now, last)),
            created_at: ago(now, 60 * 24 * 20),
            ..Default::default()
        };
        let bots = vec![
            bot(ADA, "Ada", "Plans launches and keeps the checklist honest.", 30),
            bot(MILO, "Milo", "Writes posts and replies for the product's social accounts.", 95),
            bot(JUNIPER, "Juniper", "Looks after the website and its deploys.", 240),
            bot(PIP, "Pip", "Sweeps the inbox and flags what needs you.", 400),
        ];
        let thread = |n: u128, bot: u128, title: &str, updated: i64| Thread {
            id: id(n),
            bot_id: id(bot),
            title: Some(title.into()),
            source: Some("app".into()),
            created_at: ago(now, updated + 600),
            updated_at: ago(now, updated),
            ..Default::default()
        };
        // Newest first, as the API lists them: Ada opens on her short thread.
        let mut threads = vec![thread(ADA_SHORT, ADA, "Quick question", 30), thread(ADA_LONG, ADA, "Launch plan", 120)];
        for (k, b) in [MILO, JUNIPER, PIP].into_iter().enumerate() {
            threads.push(thread(0x200 + b, b, "Getting started", 90 + 60 * k as i64));
        }
        let mut messages = BTreeMap::new();
        let conversation = |thread: Uuid, count: usize, end: i64| -> Vec<Message> {
            (0..count)
                .map(|i| Message {
                    id: Uuid::from_u128(thread.as_u128() ^ (0x10_0000 + i as u128)),
                    thread_id: thread,
                    role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                    content: if i % 2 == 0 { question(i) } else { answer(i) },
                    run_id: None,
                    created_at: ago(now, end + 2 * (count - i) as i64),
                })
                .collect()
        };
        messages.insert(id(ADA_LONG), conversation(id(ADA_LONG), THREAD_MESSAGES, 120));
        messages.insert(id(ADA_SHORT), conversation(id(ADA_SHORT), 4, 30));
        for b in [MILO, JUNIPER, PIP] {
            messages.insert(id(0x200 + b), conversation(id(0x200 + b), 6, 90));
        }
        let run = |n: u128, bot: u128, thread: Uuid, prompt: &str, finished: i64| Run {
            id: id(n),
            bot_id: id(bot),
            thread_id: thread,
            kind: RunKind::Chat,
            prompt: Some(prompt.into()),
            status: RunStatus::Succeeded,
            cost_usd: Some(0.42),
            started_at: Some(ago(now, finished + 3)),
            finished_at: Some(ago(now, finished)),
            created_at: ago(now, finished + 3),
            ..Default::default()
        };
        let runs = vec![
            run(0x301, ADA, id(ADA_SHORT), "Is the launch still on for Thursday?", 30),
            run(0x302, MILO, id(0x200 + MILO), "Draft three posts for the launch", 95),
            run(0x303, ADA, id(ADA_LONG), "Give me the SQL for weekly active teams", 120),
            run(0x304, JUNIPER, id(0x200 + JUNIPER), "Check the landing page on mobile", 240),
            run(0x305, PIP, id(0x200 + PIP), "Sweep the inbox", 400),
        ];
        let approvals = vec![
            Approval {
                id: id(0x401),
                run_id: id(0x302),
                bot_id: id(MILO),
                tool_name: "propose_draft".into(),
                input: Some(json!({
                    "kind": "post",
                    "channel": "x",
                    "body": "Familiar 0.2 is out: teammates that work on your own computer, with every risky step waiting for your OK. Try it this week and tell us what breaks."
                })),
                reason: Some("Launch post for Thursday".into()),
                status: ApprovalStatus::Pending,
                created_at: ago(now, 20),
                bot_name: Some("Milo".into()),
                editable: vec!["body".into()],
                ..Default::default()
            },
            Approval {
                id: id(0x402),
                run_id: id(0x304),
                bot_id: id(JUNIPER),
                tool_name: "Bash".into(),
                input: Some(json!({ "command": "npm run deploy -- --prod", "description": "Deploy the landing page" })),
                reason: Some("Deploy the landing page".into()),
                status: ApprovalStatus::Pending,
                created_at: ago(now, 8),
                bot_name: Some("Juniper".into()),
                ..Default::default()
            },
        ];
        let schedule = |n: u128, bot: u128, name: &str, label: &str, cron: &str, kind: RunKind, next: i64| Schedule {
            id: id(n),
            bot_id: id(bot),
            cron: cron.into(),
            prompt: format!("{label}: do the usual and report back."),
            kind,
            enabled: true,
            last_run_at: Some(ago(now, 600)),
            next_run_at: Some(now + chrono::Duration::minutes(next)),
            label: Some(label.into()),
            bot_name: Some(name.into()),
            last_status: Some(RunStatus::Succeeded),
            last_finished_at: Some(ago(now, 598)),
            ..Default::default()
        };
        let schedules = vec![
            schedule(0x501, ADA, "Ada", "Morning briefing", "0 8 * * *", RunKind::Scheduled, 14 * 60),
            schedule(0x502, MILO, "Milo", "Weekly metrics", "0 9 * * 1", RunKind::Scheduled, 3 * 24 * 60),
            schedule(0x503, PIP, "Pip", "Inbox sweep", "*/30 * * * *", RunKind::Proactive, 20),
        ];
        Self {
            now,
            bots,
            threads,
            messages,
            runs,
            approvals,
            schedules,
            crm: crate::bench_crm::Crm::new(now, crew_ids(), empty_crm),
            parity: crate::bench_parity::Parity::new(now, page, id(ADA)),
        }
    }

    fn overview(&self) -> Overview {
        Overview {
            bots: self.bots.clone(),
            pending_approvals: json!(self.approvals.len()),
            devices: vec![Device {
                id: id(0x601),
                name: Some("This PC".into()),
                online: Some(true),
                last_seen_at: Some(self.now),
                ..Default::default()
            }],
        }
    }

    /// The CRM shots' extras: three of the GTM crew at work, and a first email of theirs waiting in Needs you.
    fn add_crew(&mut self) {
        self.bots.extend(crate::bench_crm::crew_bots(self.now, &crew_ids()));
        let deal = self.crm.deals.first().cloned().unwrap_or_default();
        self.approvals.push(Approval {
            id: self.crm.waiting_draft,
            run_id: id(0x305),
            bot_id: id(DRAFTER),
            tool_name: "propose_draft".into(),
            input: Some(json!({
                "kind": "email",
                "channel": "email",
                "to": deal.contact_email.clone().unwrap_or_default(),
                "subject": format!("Incident reviews at {}", deal.company_name.clone().unwrap_or_default()),
                "body": "Hi Sam, saw the postmortem your team published last week. We help platform teams cut the time from page to fix. Worth a 20-minute call next week?\n\nIf this isn't relevant, reply \"stop\" and I won't write again.",
                "note": format!("CRM deal {} / contact {}", deal.id, deal.contact_id.unwrap_or_default()),
            })),
            reason: Some("First email for a new deal".into()),
            status: ApprovalStatus::Pending,
            created_at: self.now - chrono::Duration::minutes(1),
            bot_name: Some("Outbound drafter".into()),
            editable: vec!["subject".into(), "body".into()],
            ..Default::default()
        });
    }

    /// The fake API: the JSON for a GET, or `None` (404).
    fn get(&self, target: &str) -> Option<Value> {
        let path = target.split('?').next().unwrap_or("/");
        let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
        if let ["api", rest @ ..] = segs.as_slice()
            && let Some(v) = self.parity.get(rest).or_else(|| self.crm.get(rest, &crate::bench_crm::query(target)))
        {
            return Some(v);
        }
        let uuid = |s: &str| s.parse::<Uuid>().ok();
        let v = match segs.as_slice() {
            ["api", "overview"] => json!(self.overview()),
            ["api", "approvals"] => json!(self.approvals),
            ["api", "schedules"] => json!(self.schedules),
            ["api", "bots", b, "runs"] => {
                let b = uuid(b)?;
                json!(self.runs.iter().filter(|r| r.bot_id == b).collect::<Vec<_>>())
            }
            ["api", "bots", b, "threads"] => {
                let b = uuid(b)?;
                json!(self.threads.iter().filter(|t| t.bot_id == b).collect::<Vec<_>>())
            }
            ["api", "bots", b, "schedules"] => {
                let b = uuid(b)?;
                json!(self.schedules.iter().filter(|s| s.bot_id == b).collect::<Vec<_>>())
            }
            ["api", "bots", _, "folders"] | ["api", "runs", _, "events"] => json!([]),
            ["api", "threads", t, "messages"] => json!(self.messages.get(&uuid(t)?)?),
            ["api", "threads", t, "runs"] => {
                let t = uuid(t)?;
                json!(self.runs.iter().filter(|r| r.thread_id == t).collect::<Vec<_>>())
            }
            _ => return None,
        };
        Some(v)
    }

    /// The fake API's writes (the CRM's; hiring the crew also adds its teammates and schedules).
    fn write(&mut self, method: &str, target: &str, body: &str) -> Option<Value> {
        let path = target.split('?').next().unwrap_or("/");
        let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
        let ["api", rest @ ..] = segs.as_slice() else { return None };
        if let Some(v) = self.parity.write(method, rest, body) {
            return Some(v);
        }
        let (v, hired) = self.crm.write(method, rest, body)?;
        if let Some((bots, schedules)) = hired {
            self.bots.extend(bots);
            self.schedules.extend(schedules);
        }
        Some(v)
    }

    fn set_status(&mut self, bot: Uuid, status: BotStatus) {
        let now = Utc::now();
        if let Some(b) = self.bots.iter_mut().find(|b| b.id == bot) {
            b.status = Some(status);
            b.last_run_at = Some(now);
        }
    }

    /// Ada starts working on a long reply in the long thread.
    fn start_run(&mut self) {
        let now = Utc::now();
        let prompt = "Write the full launch plan, with the checklist and the queries.";
        self.messages.entry(id(ADA_LONG)).or_default().push(Message {
            id: id(0x700),
            thread_id: id(ADA_LONG),
            role: Role::User,
            content: prompt.into(),
            created_at: now,
            ..Default::default()
        });
        self.runs.insert(
            0,
            Run {
                id: id(LIVE_RUN),
                bot_id: id(ADA),
                thread_id: id(ADA_LONG),
                kind: RunKind::Chat,
                prompt: Some(prompt.into()),
                status: RunStatus::Running,
                started_at: Some(now),
                created_at: now,
                ..Default::default()
            },
        );
        self.set_status(id(ADA), BotStatus::Running);
    }

    /// The reply is done: the run succeeded and its text is a message.
    fn finish_run(&mut self, text: String) {
        let now = Utc::now();
        if let Some(r) = self.runs.iter_mut().find(|r| r.id == id(LIVE_RUN)) {
            r.status = RunStatus::Succeeded;
            r.finished_at = Some(now);
        }
        self.messages.entry(id(ADA_LONG)).or_default().push(Message {
            id: id(0x701),
            thread_id: id(ADA_LONG),
            role: Role::Assistant,
            content: text,
            run_id: Some(id(LIVE_RUN)),
            created_at: now,
        });
        self.set_status(id(ADA), BotStatus::Idle);
    }
}

// ---- the fake API -------------------------------------------------------------------------------------------------

async fn serve(listener: tokio::net::TcpListener, fx: Arc<Mutex<Fixture>>) {
    while let Ok((sock, _)) = listener.accept().await {
        let fx = fx.clone();
        tokio::spawn(async move {
            let _ = answer_one(sock, fx).await;
        });
    }
}

async fn answer_one(mut sock: tokio::net::TcpStream, fx: Arc<Mutex<Fixture>>) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut buf).await?;
        if n == 0 || head.len() > 64 * 1024 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    // The body: what came with the head, then the rest of `Content-Length`.
    let split = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |p| p + 4);
    let mut body = head.split_off(split);
    let head = String::from_utf8_lossy(&head);
    let length = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(8 * 1024 * 1024);
    while body.len() < length {
        let n = sock.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let mut words = head.split_whitespace();
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
    let path = target.split('?').next().unwrap_or("/");
    if path == "/api/stream" {
        // A live stream that stays quiet: the driver feeds the app its deltas and notices directly.
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n: ok\n\n").await?;
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            sock.write_all(b": ping\n\n").await?;
        }
    }
    tokio::time::sleep(LATENCY).await;
    let found = if method == "GET" { fx.lock().unwrap().get(target) } else { fx.lock().unwrap().write(method, target, &body) };
    let (status, body) = match found {
        Some(v) => ("200 OK", v.to_string()),
        None => ("404 Not Found", json!({ "error": "not found" }).to_string()),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    sock.write_all(reply.as_bytes()).await?;
    sock.shutdown().await
}

// ---- the window ---------------------------------------------------------------------------------------------------

pub struct BenchRoot {
    shell: Entity<Shell>,
}

impl Render for BenchRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.shell.clone().into_any_element();
        crate::root::window_frame(body, true, window, cx)
    }
}

/// Open the bench window on the fake API and run the scenario (or show one page for `--bench-shot`).
pub fn open(args: Args, rt: tokio::runtime::Handle, cx: &mut App) {
    let page = args.shot.as_deref().unwrap_or("");
    let mut fixture = Fixture::new(page);
    // The CRM shots show three of the GTM crew at work (not the ones that hire it).
    if page.starts_with("crm") && !matches!(page, "crm-crew" | "crm-crew-done" | "crm-picker") {
        fixture.add_crew();
    }
    let fx = Arc::new(Mutex::new(fixture));
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind the bench's local API");
    listener.set_nonblocking(true).expect("non-blocking listener");
    let base = format!("http://{}", listener.local_addr().expect("local address"));
    let served = fx.clone();
    rt.spawn(async move {
        if let Ok(listener) = tokio::net::TcpListener::from_std(listener) {
            serve(listener, served).await;
        }
    });
    let client = Client::new(base, Some("bench".into()));
    let data = cx.new(|cx| AppData::new(client, Mode::Hosted, cx));
    let options = crate::window_options(cx, "Familiar — Bench", 1180.0, 780.0, true);
    let shell_data = data.clone();
    let window: WindowHandle<BenchRoot> = cx
        .open_window(options, move |window, cx| {
            let shell = cx.new(|cx| Shell::new(shell_data, None, window, cx));
            cx.new(|_| BenchRoot { shell })
        })
        .expect("open the bench window");
    cx.activate(true);
    let shell = window.update(cx, |root, _, _| root.shell.clone()).expect("bench window");
    cx.spawn(async move |cx| {
        let report = match args.shot.as_deref() {
            Some(page) => {
                shot(page, window, &shell, &data, cx).await;
                cx.background_executor().timer(Duration::from_secs(args.hold)).await;
                None
            }
            None => Some(scenario(&shell, &data, &fx, cx).await),
        };
        if let Some(report) = report {
            match &args.out {
                Some(path) => {
                    let _ = std::fs::write(path, &report);
                }
                None => println!("{report}"),
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn wait(cx: &AsyncApp, d: Duration) {
    cx.background_executor().timer(d).await;
}

/// Poll `ready` every 5 ms for up to `limit`.
async fn until(cx: &mut AsyncApp, limit: Duration, mut ready: impl FnMut(&mut App) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cx.update(|cx| ready(cx)) {
            return true;
        }
        wait(cx, Duration::from_millis(5)).await;
    }
    false
}

/// The time of the [`perf::begin`]/[`perf::painted`] interval `name`, waiting up to 5 s for it.
async fn interval(cx: &mut AsyncApp, name: &'static str) -> Option<f64> {
    let mut got = None;
    until(cx, Duration::from_secs(5), |_| {
        got = perf::take_results().into_iter().find(|(n, _)| *n == name).map(|(_, ms)| ms);
        got.is_some()
    })
    .await;
    if got.is_none() {
        perf::cancel(name);
    }
    got
}

async fn shot(page: &str, window: WindowHandle<BenchRoot>, shell: &Entity<Shell>, data: &Entity<AppData>, cx: &mut AsyncApp) {
    until(cx, Duration::from_secs(10), |cx| data.read(cx).overview.is_some()).await;
    if page.starts_with("crm") {
        return crm_shot(page, window, shell, cx).await;
    }
    if page.starts_with("integrations") {
        return integrations_shot(page, window, shell, cx).await;
    }
    if page.starts_with("teammate-") {
        return teammate_shot(page, window, shell, cx).await;
    }
    match page {
        "chat" => {
            cx.update(|cx| shell.update(cx, |s, cx| s.navigate(Route::Teammate(id(ADA).to_string().into()), cx)));
            wait(cx, Duration::from_millis(400)).await;
            cx.update(|cx| {
                if let Some(p) = shell.read(cx).page_of(id(ADA)) {
                    p.update(cx, |p, cx| p.select(id(ADA_LONG), cx));
                }
            });
        }
        other => {
            let route = match other {
                "needs" => Route::NeedsYou,
                "schedules" => Route::Schedules,
                _ => Route::Today,
            };
            cx.update(|cx| shell.update(cx, |s, cx| s.navigate(route, cx)));
        }
    }
}

/// The CRM shots: `crm` (companies), `crm-contacts`, `crm-pipeline`, `crm-deals`, `crm-activity`, `crm-company`,
/// `crm-contact` (do-not-contact), `crm-deal`, `crm-move` (the board's stage menu), `crm-import` (the dry-run
/// preview), `crm-empty`, `crm-picker` (New teammate with the crew), `crm-crew`, `crm-crew-done`, `crm-webhooks`,
/// `crm-webhook-add` (with an address it refuses) and `crm-webhook-secret`.
async fn crm_shot(page: &str, window: WindowHandle<BenchRoot>, shell: &Entity<Shell>, cx: &mut AsyncApp) {
    use crate::crm::Tab;
    use crate::crm_record::Rec;
    let open = |cx: &mut AsyncApp, target: &str| {
        let target = target.to_owned();
        cx.update(|cx| shell.update(cx, |s, cx| s.open(&target, cx)));
    };
    let crm_ready = |cx: &mut App| shell.read(cx).crm_page().is_some();
    // Run `f` on the CRM page with its window once the page exists and its lists are in.
    async fn on_crm(
        window: WindowHandle<BenchRoot>,
        shell: &Entity<Shell>,
        cx: &mut AsyncApp,
        f: impl FnOnce(&mut crate::crm::CrmPage, &mut Window, &mut Context<crate::crm::CrmPage>),
    ) {
        wait(cx, Duration::from_millis(700)).await;
        let _ = window.update(cx, |_, window, cx| {
            if let Some(p) = shell.read(cx).crm_page() {
                p.update(cx, |p, cx| f(p, window, cx));
            }
        });
    }
    match page {
        "crm-picker" => open(cx, "new"),
        "crm-crew" | "crm-crew-done" => {
            open(cx, "crew");
            if page == "crm-crew-done" {
                until(cx, Duration::from_secs(5), |cx| shell.read(cx).crew_form().is_some()).await;
                wait(cx, Duration::from_millis(600)).await;
                cx.update(|cx| {
                    if let Some(c) = shell.read(cx).crew_form() {
                        c.update(cx, |c, cx| c.hire(cx));
                    }
                });
            }
        }
        "crm-webhooks" | "crm-webhook-add" | "crm-webhook-secret" => {
            open(cx, "settings");
            wait(cx, Duration::from_millis(500)).await;
            let fill = match page {
                "crm-webhook-add" => Some(("https://192.168.1.20/hooks/crm", false)),
                "crm-webhook-secret" => Some(("https://hooks.zapier.com/hooks/catch/1234567/n3wh00k/", true)),
                _ => None,
            };
            if let Some((url, submit)) = fill {
                let _ = window.update(cx, |_, window, cx| {
                    if let Some(s) = shell.read(cx).settings_page() {
                        let hooks = s.read(cx).webhooks();
                        hooks.update(cx, |h, cx| h.fill_add(url, submit, window, cx));
                    }
                });
            }
            wait(cx, Duration::from_millis(500)).await;
            cx.update(|cx| shell.update(cx, |s, cx| {
                s.scroll_page_to(5000.0);
                cx.notify()
            }));
        }
        _ => {
            let (target, then): (&str, Option<&str>) = match page {
                "crm-contacts" => ("crm/contacts", None),
                "crm-pipeline" | "crm-move" => ("crm/pipeline", Some(page)),
                "crm-deals" => ("crm/deals", None),
                "crm-activity" => ("crm/activity", None),
                "crm-contact" => ("crm/contacts", Some(page)),
                "crm-company" | "crm-deal" | "crm-import" => ("crm", Some(page)),
                _ => ("crm", None),
            };
            open(cx, target);
            until(cx, Duration::from_secs(5), crm_ready).await;
            // The newest company, a do-not-contact contact, the first deal (the fixture's ids are fixed).
            use crate::bench_crm::cid;
            let (company, dnc_contact, deal) = (cid(1, 0), cid(2, 5), cid(3, 0));
            match then {
                Some("crm-company") => on_crm(window, shell, cx, |p, w, cx| p.open(Rec::Company(company), false, w, cx)).await,
                Some("crm-contact") => on_crm(window, shell, cx, |p, w, cx| p.open(Rec::Contact(dnc_contact), false, w, cx)).await,
                Some("crm-deal") => on_crm(window, shell, cx, |p, w, cx| p.open(Rec::Deal(deal), false, w, cx)).await,
                Some("crm-import") => {
                    on_crm(window, shell, cx, |p, _, cx| p.preview_import("leads-october.csv".into(), crate::bench_crm::sample_csv(), "companies", cx)).await
                }
                Some("crm-move") => {
                    on_crm(window, shell, cx, |p, w, cx| {
                        p.set_tab(Tab::Pipeline, cx);
                        // The first card of Replied.
                        let replied = crate::crm_model::stage_index(familiar_client::DealStage::Replied).unwrap_or(3);
                        if let Some(d) = p.board_card(replied, 0) {
                            p.open_move_menu(d, w, cx);
                        }
                    })
                    .await
                }
                _ => {}
            }
        }
    }
}

/// The Integrations shots: `integrations` (the installed connectors), `integrations-catalog` (scrolled to the
/// catalog), `integrations-install` (Slack's install dialog: two secret fields), `integrations-custom` (your own
/// server), `integrations-edit` (an installed server of the owner's, with its stored secrets' names), and Telegram:
/// `integrations-phone` (paired), `integrations-telegram-pair` (waiting for `/start <code>`), `integrations-telegram`
/// (the three steps).
async fn integrations_shot(page: &str, window: WindowHandle<BenchRoot>, shell: &Entity<Shell>, cx: &mut AsyncApp) {
    let target = if page == "integrations-install" { "integrations/slack" } else { "integrations" };
    cx.update(|cx| shell.update(cx, |s, cx| s.open(target, cx)));
    until(cx, Duration::from_secs(5), |cx| shell.read(cx).integrations_page().is_some()).await;
    wait(cx, Duration::from_millis(600)).await;
    let _ = window.update(cx, |_, window, cx| {
        let Some(p) = shell.read(cx).integrations_page() else { return };
        p.update(cx, |p, cx| match page {
            "integrations-custom" => p.open_dialog(None, window, cx),
            "integrations-edit" => p.open_dialog(Some("notes"), window, cx),
            "integrations-catalog" => p.scroll_to(1200.0, cx),
            "integrations-telegram" | "integrations-telegram-pair" | "integrations-phone" => p.scroll_to(640.0, cx),
            _ => {}
        });
    });
}

/// Ada's page on one tab, scrolled to what the shot is about: `teammate-connectors` (Settings → Connectors),
/// `teammate-triggers` (Settings → Webhooks that wake it), `teammate-trigger-url` (a new webhook's address, shown once).
async fn teammate_shot(page: &str, window: WindowHandle<BenchRoot>, shell: &Entity<Shell>, cx: &mut AsyncApp) {
    let (target, y) = match page {
        "teammate-connectors" => ("ada/settings", 840.0),
        "teammate-triggers" | "teammate-trigger-url" => ("ada/settings", 1180.0),
        _ => ("ada", 0.0),
    };
    cx.update(|cx| shell.update(cx, |s, cx| s.open(target, cx)));
    until(cx, Duration::from_secs(5), |cx| shell.read(cx).page_of(id(ADA)).is_some()).await;
    wait(cx, Duration::from_millis(900)).await;
    if page == "teammate-trigger-url" {
        let _ = window.update(cx, |_, window, cx| {
            let triggers = shell.read(cx).page_of(id(ADA)).and_then(|p| p.read(cx).settings_tab()).and_then(|s| s.read(cx).triggers_view());
            if let Some(t) = triggers {
                t.update(cx, |t, cx| t.fill("Deploy finished", "A deploy just finished. Check the site and tell me if anything looks off.", true, window, cx));
            }
        });
        wait(cx, Duration::from_millis(600)).await;
    }
    cx.update(|cx| {
        if let Some(p) = shell.read(cx).page_of(id(ADA)) {
            p.update(cx, |p, cx| p.scroll_tab_to(y, cx));
        }
    });
}

/// Frames and renders over `d` of doing nothing.
async fn idle(cx: &mut AsyncApp, d: Duration) -> String {
    perf::take_frames();
    perf::take_renders();
    wait(cx, d).await;
    let frames = perf::take_frames();
    let renders = perf::take_renders();
    let secs = d.as_secs_f32();
    format!("{:.1} frames/s; renders/s {}", frames.len() as f32 / secs, per_second(&renders, secs))
}

fn per_second(renders: &BTreeMap<&'static str, u64>, secs: f32) -> String {
    renders.iter().map(|(k, n)| format!("{k} {:.1}", *n as f32 / secs)).collect::<Vec<_>>().join(", ")
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

async fn scenario(shell: &Entity<Shell>, data: &Entity<AppData>, fx: &Arc<Mutex<Fixture>>, cx: &mut AsyncApp) -> String {
    let mut out = String::new();
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let _ = writeln!(out, "familiar-native bench ({profile} build, {})", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    let ready = until(cx, Duration::from_secs(30), |cx| {
        let d = data.read(cx);
        d.overview.is_some() && d.runs_loaded && d.schedules_loaded
    })
    .await;
    let _ = writeln!(out, "data loaded: {ready} at {:.0} ms since process start", perf::since_start_ms());
    wait(cx, Duration::from_millis(800)).await;

    let _ = writeln!(out, "idle, Today: {}", idle(cx, IDLE_WINDOW).await);

    // Teammate switch: hover, click, until their chat shows (first open of each).
    let mut switches = Vec::new();
    for bot in [MILO, JUNIPER, PIP] {
        let route = Route::Teammate(id(bot).to_string().into());
        cx.update(|cx| shell.update(cx, |s, cx| s.hover_teammate(id(bot), cx)));
        wait(cx, HOVER_TO_CLICK).await;
        cx.update(|cx| shell.update(cx, |s, cx| s.navigate(route, cx)));
        switches.push(interval(cx, "switch").await.unwrap_or(f64::NAN));
        wait(cx, Duration::from_millis(500)).await;
        cx.update(|cx| shell.update(cx, |s, cx| s.navigate(Route::Today, cx)));
        wait(cx, Duration::from_millis(500)).await;
    }
    let shown = switches.iter().map(|ms| format!("{ms:.1}")).collect::<Vec<_>>().join(", ");
    let _ = writeln!(out, "teammate switch (click to chat painted), ms: {shown}; median {:.1}", median(&mut switches));

    // The long thread: first open, then again (cached).
    cx.update(|cx| shell.update(cx, |s, cx| s.navigate(Route::Teammate(id(ADA).to_string().into()), cx)));
    let ada = interval(cx, "switch").await;
    wait(cx, Duration::from_millis(500)).await;
    let page = cx.update(|cx| shell.read(cx).page_of(id(ADA))).expect("Ada's page");
    let mut opens = Vec::new();
    for thread in [ADA_LONG, ADA_SHORT, ADA_LONG] {
        cx.update(|cx| page.update(cx, |p, cx| p.select(id(thread), cx)));
        opens.push(interval(cx, "thread_open").await.unwrap_or(f64::NAN));
        wait(cx, Duration::from_millis(600)).await;
    }
    let _ = writeln!(
        out,
        "open Ada (4-message thread): {:.1} ms; open the {THREAD_MESSAGES}-message thread: first {:.1} ms, again {:.1} ms",
        ada.unwrap_or(f64::NAN),
        opens[0],
        opens[2]
    );

    let _ = writeln!(out, "idle, long chat: {}", idle(cx, IDLE_WINDOW).await);

    // Ada starts a run; then the reply streams.
    let notice = |t: &str| Notice { t: t.into(), bot: Some(id(ADA).to_string()), run: Some(id(LIVE_RUN).to_string()), ..Default::default() };
    fx.lock().unwrap().start_run();
    cx.update(|cx| {
        data.update(cx, |d, cx| {
            d.push_notice(Some(notice("messages")), cx);
            d.push_notice(Some(notice("runs")), cx);
        })
    });
    wait(cx, Duration::from_millis(800)).await;

    let text = long_reply();
    let run = id(LIVE_RUN);
    let chunks: Vec<&str> = text.as_bytes().chunks(CHUNK).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    perf::take_frames();
    perf::take_renders();
    perf::take_results();
    let (ui0, proc0) = perf::cpu_times();
    let start = Instant::now();
    let mut delta_ms = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let due = start + CHUNK_EVERY * i as u32;
        let now = Instant::now();
        if due > now {
            wait(cx, due - now).await;
        }
        perf::begin("delta");
        cx.update(|cx| data.update(cx, |d, cx| d.push_delta(run, false, chunk, cx)));
        delta_ms.extend(perf::take_results().into_iter().filter(|(n, _)| *n == "delta").map(|(_, ms)| ms as f32));
    }
    wait(cx, Duration::from_millis(100)).await;
    delta_ms.extend(perf::take_results().into_iter().filter(|(n, _)| *n == "delta").map(|(_, ms)| ms as f32));
    let wall = start.elapsed();
    let (ui1, proc1) = perf::cpu_times();
    let frames = perf::take_frames();
    let renders = perf::take_renders();
    let secs = wall.as_secs_f32();
    let (p50, p95, max) = perf::percentiles(&frames);
    let (d50, d95, dmax) = perf::percentiles(&delta_ms);
    let _ = writeln!(
        out,
        "streaming {} chars in {} deltas over {secs:.2} s: {} frames ({:.1}/s); frame p50 {p50:.2} ms, p95 {p95:.2} ms, max {max:.2} ms",
        text.len(),
        chunks.len(),
        frames.len(),
        frames.len() as f32 / secs
    );
    // Does a frame cost more as the reply grows? The first and the last eighth of the stream's frames.
    let eighth = (frames.len() / 8).max(1).min(frames.len());
    let (early, _, _) = perf::percentiles(&frames[..eighth]);
    let (late, _, _) = perf::percentiles(&frames[frames.len() - eighth..]);
    let _ = writeln!(out, "streaming frame p50, first eighth {early:.2} ms, last eighth {late:.2} ms");
    let _ = writeln!(
        out,
        "streaming CPU: UI thread {:.0} ms ({:.1}% of a core), process {:.0} ms ({:.1}% of a core)",
        (ui1 - ui0).as_secs_f64() * 1000.0,
        (ui1 - ui0).as_secs_f32() / secs * 100.0,
        (proc1 - proc0).as_secs_f64() * 1000.0,
        (proc1 - proc0).as_secs_f32() / secs * 100.0
    );
    let _ = writeln!(out, "streaming renders/s: {}", per_second(&renders, secs));
    let _ = writeln!(
        out,
        "delta to painted: {} of {} deltas measured; p50 {d50:.2} ms, p95 {d95:.2} ms, max {dmax:.2} ms",
        delta_ms.len(),
        chunks.len()
    );

    fx.lock().unwrap().finish_run(text);
    cx.update(|cx| {
        data.update(cx, |d, cx| {
            d.push_notice(Some(notice("messages")), cx);
            d.push_notice(Some(notice("runs")), cx);
        })
    });
    wait(cx, Duration::from_millis(1500)).await;
    let _ = writeln!(out, "idle, long chat after the reply: {}", idle(cx, IDLE_WINDOW).await);
    out
}
