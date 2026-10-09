//! Performance probes. `FAMILIAR_PERF=1` appends to `~/.familiar/logs/perf.log` (`FAMILIAR_PERF=<file>`: that file):
//! start-up milestones since the process was created, navigation timings (a click to the frame that shows the result)
//! and, every 5 s, frame times (p50/p95/max) and renders per view. `--bench` reads the same probes. Off, every probe
//! is one relaxed atomic load.
//!
//! A frame is timed from the top of the window root's `render` to the paint of [`frame_probe`], the root's last
//! child: render, layout, prepaint and paint on the UI thread (not the GPU's present).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, IntoElement, Styled as _, canvas};

static ON: AtomicBool = AtomicBool::new(false);
/// `main`'s first instant, and how long the process had existed by then.
static START: OnceLock<(Instant, Duration)> = OnceLock::new();
static LOG: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Summaries are written this often while the log is on.
const SUMMARY_EVERY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Stats {
    frame_start: Option<Instant>,
    /// Frame times (ms) since the last take.
    frames: Vec<f32>,
    renders: BTreeMap<&'static str, u64>,
    began: HashMap<&'static str, Instant>,
    /// Intervals that end with the frame being drawn.
    ending: Vec<&'static str>,
    results: Vec<(&'static str, f64)>,
    milestones: HashSet<&'static str>,
    /// Milestones that land with the frame being drawn.
    painted_milestones: Vec<&'static str>,
}

thread_local! {
    static STATS: RefCell<Stats> = RefCell::new(Stats::default());
}

/// At most this many frame times and interval results are kept between takes.
const MAX_KEPT: usize = 100_000;

/// Call first thing in `main`. `bench` turns the probes on without a log file (the bench reports itself).
pub fn init(bench: bool) {
    START.get_or_init(|| (Instant::now(), process_age()));
    let env = std::env::var("FAMILIAR_PERF").ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty() && v != "0");
    let log = match env.as_deref() {
        _ if bench => None,
        None => None,
        Some("1") => Some(crate::engine::familiar_home().join("logs").join("perf.log")),
        Some(path) => Some(PathBuf::from(path)),
    };
    let on = bench || log.is_some();
    if let Some(path) = &log
        && let Some(dir) = path.parent()
    {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = LOG.set(log);
    ON.store(on, Ordering::Relaxed);
    if on && !bench {
        write(&format!("---- familiar-native {} (pid {})", env!("CARGO_PKG_VERSION"), std::process::id()));
    }
}

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Milliseconds since the process was created.
pub fn since_start_ms() -> f64 {
    let (at, age) = START.get_or_init(|| (Instant::now(), Duration::ZERO));
    (*age + at.elapsed()).as_secs_f64() * 1000.0
}

fn write(line: &str) {
    let Some(Some(path)) = LOG.get() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%H:%M:%S%.3f"));
    }
}

/// A start-up milestone, logged once with its time since the process was created.
pub fn milestone(name: &'static str) {
    if !on() {
        return;
    }
    let first = STATS.with(|s| s.borrow_mut().milestones.insert(name));
    if first {
        write(&format!("milestone {name} {:.0} ms", since_start_ms()));
    }
}

/// A free-form line with its time since the process was created (boot steps).
pub fn note(what: &str) {
    if on() {
        write(&format!("note {what} {:.0} ms", since_start_ms()));
    }
}

/// [`milestone`], at the end of the frame being drawn (call from `render`).
pub fn milestone_painted(name: &'static str) {
    if on() {
        STATS.with(|s| {
            let mut s = s.borrow_mut();
            if !s.milestones.contains(name) && !s.painted_milestones.contains(&name) {
                s.painted_milestones.push(name);
            }
        });
    }
}

/// Start timing `name` (a click, a navigation). It ends at the first frame that calls [`painted`] for it.
pub fn begin(name: &'static str) {
    if on() {
        STATS.with(|s| {
            s.borrow_mut().began.insert(name, Instant::now());
        });
    }
}

/// From `render`: what `name` waited for is in this frame; its time ends when the frame is painted.
pub fn painted(name: &'static str) {
    if on() {
        STATS.with(|s| {
            let mut s = s.borrow_mut();
            if s.began.contains_key(name) && !s.ending.contains(&name) {
                s.ending.push(name);
            }
        });
    }
}

/// Count a render of `view`.
#[inline]
pub fn count(view: &'static str) {
    if on() {
        STATS.with(|s| *s.borrow_mut().renders.entry(view).or_default() += 1);
    }
}

/// Top of the window root's `render`.
pub fn frame_begin() {
    if on() {
        STATS.with(|s| s.borrow_mut().frame_start = Some(Instant::now()));
    }
}

/// The window root's last child: its paint ends the frame.
pub fn frame_probe() -> impl IntoElement {
    canvas(|_, _, _| {}, |_, _, _, _| frame_end()).absolute().top_0().left_0().size_0()
}

fn frame_end() {
    if !on() {
        return;
    }
    let now = Instant::now();
    let mut lines = Vec::new();
    STATS.with(|s| {
        let mut s = s.borrow_mut();
        // Capped: only the bench and the 5 s log summary drain these, and a gallery window or a long session with the
        // log on would otherwise grow them forever.
        if let Some(start) = s.frame_start.take()
            && s.frames.len() < MAX_KEPT
        {
            s.frames.push(now.duration_since(start).as_secs_f32() * 1000.0);
        }
        for name in std::mem::take(&mut s.ending) {
            if let Some(at) = s.began.remove(name) {
                let ms = now.duration_since(at).as_secs_f64() * 1000.0;
                if s.results.len() < MAX_KEPT {
                    s.results.push((name, ms));
                }
                lines.push(format!("{name} {ms:.1} ms"));
            }
        }
        for name in std::mem::take(&mut s.painted_milestones) {
            if s.milestones.insert(name) {
                lines.push(format!("milestone {name} {:.0} ms", since_start_ms()));
            }
        }
    });
    for line in lines {
        write(&line);
    }
}

/// Frame times (ms) since the last take.
pub fn take_frames() -> Vec<f32> {
    STATS.with(|s| std::mem::take(&mut s.borrow_mut().frames))
}

/// Renders per view since the last take.
pub fn take_renders() -> BTreeMap<&'static str, u64> {
    STATS.with(|s| std::mem::take(&mut s.borrow_mut().renders))
}

/// Finished [`begin`]/[`painted`] intervals since the last take.
pub fn take_results() -> Vec<(&'static str, f64)> {
    STATS.with(|s| std::mem::take(&mut s.borrow_mut().results))
}

/// Forget an interval that never painted (a bench timeout).
pub fn cancel(name: &'static str) {
    STATS.with(|s| {
        let mut s = s.borrow_mut();
        s.began.remove(name);
        s.ending.retain(|n| *n != name);
    });
}

/// `(p50, p95, max)` of `v` (0s when empty).
pub fn percentiles(v: &[f32]) -> (f32, f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f32| v[((v.len() - 1) as f32 * p).round() as usize];
    (at(0.5), at(0.95), v[v.len() - 1])
}

/// With the log on: every 5 s, the frames and renders per view of the last 5 s (also when idle).
pub fn start_summaries(cx: &mut App) {
    if !on() || !matches!(LOG.get(), Some(Some(_))) {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(SUMMARY_EVERY).await;
            let frames = take_frames();
            let renders = take_renders();
            let (p50, p95, max) = percentiles(&frames);
            let secs = SUMMARY_EVERY.as_secs_f32();
            let per_view = renders.iter().map(|(k, n)| format!("{k}={:.1}", *n as f32 / secs)).collect::<Vec<_>>().join(" ");
            write(&format!(
                "frames {:.1}/s p50 {p50:.2} ms p95 {p95:.2} ms max {max:.2} ms | renders/s {per_view}",
                frames.len() as f32 / secs
            ));
        }
    })
    .detach();
}

/// CPU time used so far: (this thread, the whole process).
pub fn cpu_times() -> (Duration, Duration) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes};
        let zero = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let total = |k: &FILETIME, u: &FILETIME| Duration::from_nanos((ticks(k) + ticks(u)) * 100);
        let (mut c, mut e, mut k, mut u) = (zero(), zero(), zero(), zero());
        // SAFETY: the pseudo-handles need no closing; the out-pointers are valid locals.
        let thread = unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) } != 0;
        let t = if thread { total(&k, &u) } else { Duration::ZERO };
        let (mut c, mut e, mut k, mut u) = (zero(), zero(), zero(), zero());
        // SAFETY: as above.
        let process = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } != 0;
        let p = if process { total(&k, &u) } else { Duration::ZERO };
        (t, p)
    }
    #[cfg(not(windows))]
    {
        (Duration::ZERO, Duration::ZERO)
    }
}

#[cfg(windows)]
fn ticks(t: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// How long ago the OS created this process (0 where unknown).
fn process_age() -> Duration {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let zero = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut e, mut k, mut u) = (zero(), zero(), zero(), zero());
        // SAFETY: the pseudo-handle needs no closing; the out-pointers are valid locals.
        if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut e, &mut k, &mut u) } == 0 {
            return Duration::ZERO;
        }
        // FILETIME counts 100 ns ticks since 1601; the Unix epoch is 11 644 473 600 s later.
        let created_unix_ns = ticks(&created).saturating_sub(116_444_736_000_000_000) * 100;
        let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        Duration::from_nanos(now_ns.saturating_sub(created_unix_ns))
    }
    #[cfg(not(windows))]
    {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_pick_by_rank() {
        let v: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        assert_eq!(percentiles(&v), (51.0, 95.0, 100.0));
        assert_eq!(percentiles(&[]), (0.0, 0.0, 0.0));
        assert_eq!(percentiles(&[3.0]), (3.0, 3.0, 3.0));
    }
}
