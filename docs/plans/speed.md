# Plan: a faster, smoother desktop app

Inspired by Anthropic's "How we made claude.ai faster": measure first with repeatable lab numbers, stop redoing
finished work while a reply streams, prefetch on hover, cut needless redraws, show something usable at once.
Owner: Opus 5.5 (design-heavy GPUI work). Review: Fable 5.1. Scope: `apps/native` only.

## What the code does today (found while planning)
- `chat.rs`: `BotPage::new` does `cx.observe(&data, |_, _, cx| cx.notify())`, and `Shell::new` observes `AppData` too:
  any change anywhere in `AppData` (another teammate's run, an approval, a schedule, the computer panel's live frame)
  redraws the whole Shell and every open teammate page.
- `markdown.rs::render` parses the full source on every call, and `chat.rs` (~line 740) calls it for every message
  of the thread on every redraw, plus the streaming buffer (~767). Cost per redraw = all text in the thread.
- The thread view draws every message (no virtualisation).
- `Shell::render` and `Root::render` call `anim::frame(window)`; check whether that keeps requesting frames while
  nothing animates.

## Steps (commit each)
1. **Measure.** `FAMILIAR_PERF=1` perf log (zero cost when off): process start → first frame → embedded DB ready →
   API ready → first data → shell usable; teammate switch (click → painted); thread open; redraws per entity per
   second; per-frame time while streaming. A deterministic bench that needs no network or model (`--bench`, or a
   gallery section): a synthetic 60-message thread with code blocks and lists, then a ~20 000-character reply
   streamed in small chunks at a fixed rate; report frame p50/p95/max, render CPU, redraw counts. Record BEFORE.
2. **Streaming and long chats.** Cache parsed markdown per finished message (key: message id + content length/hash);
   for the streaming buffer re-parse only the growing tail (finished blocks memoised; parsing whole text must equal
   memoised result — unit test). Virtualise long threads (GPUI `list`), keeping stick-to-bottom and smooth scroll.
3. **Redraw scope.** Replace blanket `observe(&data)` with targeted events/subscriptions so each area (sidebar, a
   teammate page, chat, inbox badge, computer panel) redraws only for changes it shows. Count redraws in the perf log.
4. **Instant navigation.** Prefetch a teammate's data on hover (sidebar row, Today card; ~120 ms debounce, cancel on
   leave, through the existing SWR cache); keep teammate pages and their composer alive between switches; show cached
   content first, then refresh.
5. **Cold start.** Find where start-up time goes (embedded Postgres start is the likely bulk); paint the shell with the
   last session's cached data while the engine boots, if that can be done without showing wrong data; never block the
   first frame on avoidable work.
6. **AFTER.** Same benchmarks; before/after table: frame p95, max frame, streaming CPU, teammate switch ms, thread
   open ms, cold start to usable ms, idle redraws per second.

## Guard rails
No visual regressions (screenshots of Today, a long chat, Needs you, Schedules before and after); streaming text still
appears at once; scroll behaviour unchanged; `approval.rs` behaviour unchanged.
