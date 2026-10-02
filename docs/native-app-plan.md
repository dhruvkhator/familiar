# Familiar native (GPUI) — port plan from zeron

Sources read: `C:\personal\zed` (docs/ARCHITECTURE.md, apps/web/src/**, apps/desktop/src-tauri/src/{lib,embedded_db}.rs,
apps/native/**, crates/familiar-{core,server}/src), shallow clones in this scratchpad: `zeron/` (zeronsh/zeron, MIT,
v0.2.101), `zui/` (zeronsh/zui @ 667d0aaf, 2026-09-30), `gpuic/` (zeronsh/gpui-component; gpui-base 0.5.2, Apache-2.0).
"Unverified" marks things I did not confirm by reading code or running a build. The spike `apps/native` has **not been
compiled yet** (no `libgpui*` in `target/debug/deps`), so build-time numbers are estimates.

---

## 1. What to vendor from zeron vs. what to adapt / rewrite

zeron-ui is one 105k-line crate (`zeron/crates/ui/src`). Coupling, from the `use` graph:

| Layer | Modules | Coupled to | Verdict |
|---|---|---|---|
| gpui-only utilities | `motion.rs` (+`motion/windows_pulse.rs`), `edge_fade.rs`, `popover.rs` (+`popover/{contained,hover_intent}.rs`), `notice.rs`, `image_media.rs`, `image_viewer.rs`, `icons.rs` | `crate::theme`, `crate::motion`, `zeron_proto::motion` (constants only) | **Vendor nearly as-is** |
| Typography / appearance | `typography.rs`, `appearance.rs` | `crate::settings::{self,SavePolicy}` (ui-settings.json writer), `zeron_theme::{ThemeSelection,AccentSelection,SurfacePreference}` | **Vendor, strip the zeron-theme selections** |
| Theme | `theme.rs` (3028 lines), `crates/theme` (zeron-theme, 4.7k lines: builtins + VS Code importer) | `zeron_syntax::HighlightKind`, `zeron_theme::{ThemeVariant,ThemeRegistry,...}` | **Vendor helpers + pattern, author our own tokens**; skip `theme_library.rs`, `crates/theme/src/{vscode,library}.rs` |
| Markdown | `crates/markdown` (zeron-markdown: `parser.rs` 1735, `mend.rs` 413; only dep pulldown-cmark) and `ui/src/markdown/{render,veil,selection}.rs` | render.rs: `crate::theme::Theme`, `zeron_syntax::{HighlightKind,HighlightSpan,HighlightedDocument}`, `super::links` | **Vendor** (markdown crate as-is; render/veil/selection with theme import rename); `link_*`/`links.rs` adapt; `mermaid.rs` postpone |
| Syntax | `crates/syntax` (1354 lines + 27 tree-sitter grammar crates) | none (pure) | **Vendor with a trimmed grammar list** (feature-gated) |
| Transcript | `transcript.rs` (14.2k) | `zeron_doc::{MessagePart,SessionMessageEntry,...}`, `zeron_proto::ToolCall`, `crate::state::AppState`, `attachments`, `changes` | **Rewrite**; extract `StickSpring` (lines ~221-330), `diff_rows`, constants, tool-chip geometry, `format_timestamp/format_elapsed`, `flavour_word` |
| Composer | `composer.rs` (15.2k; `ComposerInput` ≈ lines 1679-4990) | `zeron_proto::invocation`, `zeron_rpc`, pickers, attachments, dictation, appshots | **Do not vendor in v0**: use `gpui_base::Textarea`; revisit extracting `ComposerInput` (gpui `examples/input.rs` lineage) if IME/caret quality demands |
| Shell/state/settings | `shell.rs` (16.7k), `state.rs`, `settings.rs`, `pickers.rs`, `history.rs`, `changes.rs`, `files/`, `terminal/`, `browser/`, `dictation/`, `appshots/`, `queue.rs`, `rail.rs`, `attachments.rs` | zeron engine/rpc/proto everywhere | **Mine for patterns only** (sidebar resize, titlebar drag `app_owns_titlebar_drag`, `WindowGeometry` restore in settings.rs ~691-760, `SavePolicy` writer ~284-340) |
| Update | `crates/update` (lib.rs 2023, windows.rs 510), `ui/src/app_update.rs` (540) | `gpui_tokio::Tokio`, zeron edge URLs, `zeron-update.json`, `zeron.exe`, Inno AppId | **Vendor as `familiar-update`, rename constants**, keep only `WindowsPortable`+`Unmanaged` install kinds for now |
| Loaders | `loaders.rs` (406) | `zeron_proto::motion::{MARK_CELLS,...}` = zeron logo pixel grid | Vendor `gradient_spinner`, `mini_mono_spinner`, `upload_progress_ring`, `splash_overlay` shape; **drop the zeron mark** (brand) |
| Notifications / sound | `notify.rs` (Windows path is a no-op in zeron), `sound.rs` (+ 4 wavs, no license noted) | — | Skip both; use gpui's own `SystemNotification` (`zui/crates/gpui_windows/src/system_notifications.rs`, WinRT toasts) |

### 1.1 Concrete vendoring list (new crate `crates/familiar-ui`, MIT; keep zeron's MIT notice + per-asset attributions)

- `theme.rs` → `familiar-ui/src/theme.rs`. Keep: `Appearance` (+`from_window`), `current_appearance/set_current_appearance`,
  `style_generation`, `Theme` struct shape and `Theme::of(cx)` Global, `ink/hairline/wash/scrim/neutral/oklch/grey/mix/flatten/
  relative_luminance/contrast_ratio`, the "light is designed, not inverted" rules in the module doc, the contrast-pairing
  test. Replace the palette with Familiar's tokens from `apps/web/src/index.css` (bg, surface, sunken, line, ink, muted,
  accent, accent-soft, warn, warn-soft, bad, ok, shadow). Remove `SurfaceTreatment/AccentSelection/ThemeRegistry/
  ThemeVariant`, glass/frost getters, `TerminalColors`. `SyntaxPalette` stays only if syntax is vendored (it is).
- `typography.rs` → keep `UiFontFamily`, `UiFontSize`, `register_fonts` (uses `cx.text_system().add_fonts` on the bundled
  TTFs), `init/effective/font_size/set_*`. Fonts: copy `zeron/crates/ui/assets/fonts/{Geist,GeistMono}*.ttf` (16 files,
  2.2 MB, SIL OFL 1.1 — copy `assets/fonts/licenses/Geist-OFL.txt`). Note `UiFontFamily::System` maps to `.SystemUIFont`
  (a macOS name); on Windows use `"Segoe UI"` (how zui resolves `.SystemUIFont` on Windows: unverified).
- `appearance.rs` → keep `AppearanceMode`, `AppearanceState`, `observe_window`, `resolve`, `apply` (uses
  `cx.refresh_windows()` because colours are read imperatively at paint time — keep that comment).
- `motion.rs` → as-is minus `pub use zeron_proto::motion::*`. Gives `CubicBezier`, `MotionSpec`, FADE_IN/MENU_IN/
  DIALOG_IN/RESIZE/COLLAPSE/HOVER_FADE constants, `fade_in/menu_in/dialog_in/splash_out` helpers, `HoverFades`, the
  self-parking 30 fps pulse clock (`pulse_delta`, `PULSE_TICK`), `ReduceMotion` (OS reduce-motion + focus pause).
  `motion/windows_pulse.rs` needs `windows-sys` features `Win32_System_Threading`.
- `edge_fade.rs` → as-is (requires the fork-only `gpui::EdgeFade` / `Window::with_edge_fade`; we pin the same rev).
- `popover.rs` + submodules → as-is except `use crate::theme::{Theme,hairline,ink}` and `super::search_match_ranges`
  (copy that fn from `lib.rs`). Gives `Loadable<T>`, anchored `deferred(anchored())` popovers with menu-in, keyboard
  navigation reducers, `MenuScrollbarState`.
- `icons.rs` → the `icon_assets!` macro + `Assets: AssetSource` + `icon(path)`; copy only the SVGs we use from
  `zeron/crates/ui/assets/icons/` (247 KB total). Attribution: Solar Icons (Linear) by 480 Design, **CC BY 4.0** — must
  credit in THIRD_PARTY_NOTICES. Exclude brand marks (`openai-mark.svg`, `cursor-mark.svg`, `devin-mark.svg`,
  `antigravity-mark.svg`; ARCHITECTURE: "no OpenAI names, logos or assets"). `claude-mark.svg`: trademark, use only as
  an engine badge if at all.
- `image_media.rs`, `image_viewer.rs` → as-is (decode via `image` crate, show with `img()`); basis for live JPEG frames and
  artifact previews.
- `notice.rs` → as-is (chip with icon).
- `crates/markdown` → `crates/familiar-markdown` as-is (`IncrementalParser::{append,set_text,reset,display_tree}`,
  `BlockTree`, `mend`). `ui/src/markdown/{render.rs,veil.rs,selection.rs}` → `familiar-ui/src/markdown/`. `render.rs`
  exposes `render_tree/render_block(RenderOptions)`, `RenderCache`, `FlatText/flatten_runs`, table layout
  (`table_columns`), `runs_for_syntax_line_with_plain`. Replace `LinkUi` routing (`links.rs`, `link_destination.rs`,
  `link_interaction.rs` 1401, `link_presentation.rs`, `inline_code_links.rs`, `workspace_links.rs`) with a ~150-line
  version: http(s) → `cx.open_url`, everything else inert. Needs `usvg`? no — only mermaid does; drop `mermaid.rs`.
- `crates/syntax` → `crates/familiar-syntax`. Keep `lib.rs` (`LanguageId`, `HighlightKind`, `HighlightedDocument`,
  `highlight(...)`) and `queries/kotlin` only if Kotlin stays. Grammar set v0 (all MIT): bash, json, python, javascript,
  typescript, rust, markdown (`tree-sitter-md`), yaml, toml, html, css (11 of 27). Each grammar is a C build — the main
  compile-time lever. `ui/src/syntax_cache.rs` (199) comes along (`SyntaxHighlightCache`, `DocumentHighlightKey`).
- `crates/update` → `crates/familiar-update`: `Manifest/FileMeta`, `version_newer`, `download_release_file` (SHA-256 +
  1 MiB metadata cap + 60 s timeout), `Updater::spawn_desktop(edge_url)` (hourly check, retry ladder 1/5/15/30 min,
  wall-clock schedule tick), `UpdateStatus` watch channel, `detect_install`, `windows.rs::{is_managed,stage,apply,
  wait_for_exit,cleanup_previous_image,artifact}`. Rename `zeron-update.json`→`familiar-update.json`,
  `zeron.exe`→`familiar.exe`, `zeron.exe.old`, `.zeron-update-incoming.exe`, `UNINSTALL_KEY` AppId GUID (generate a new
  one and use it in our .iss), expected `--version` output `familiar <ver>`. Drop `Managed`/`MacApp` branches and
  `restart_service`. `ui/src/app_update.rs` (update strip + "Restart to update" flow) vendor with `Theme` rename.
- `settings.rs` → take only the persistence skeleton: `UiSettings` serde struct, `SavePolicy::{Immediate,Debounced}`,
  single in-process writer to `~/.familiar/ui-settings.json`, `WindowGeometry::{from_bounds,restore,fit,bounds}` and
  `save_main_window_geometry` (lib.rs ~340-362).

gpui-base (`gpuic/crates/base`, Apache-2.0, already a dep of the spike) covers forms and chrome zeron hand-rolled or did
not need: `Button`, `Checkbox`, `Switch`, `Select`, `Combobox`, `Input`/`Textarea`/`Editor` (ropey-backed),
`Dialog`/`AlertDialog`/`Sheet`, `Popover`, `Tooltip`, `Scrollbar`, `Tabs`, `Table`, `VirtualList`, `Toast`,
`resizable` panels, `Avatar`, `Progress`, `focus_trap`, `theme_tokens`. Use it for Settings/Integrations/Rules/
Schedules/Memory forms and dialogs; use vendored zeron pieces for chat polish. gpui-base's own workspace pins a different
zui rev (de9f26e0) but zeron's lock shows a single `gpui 0.2.2 @667d0aaf` — the git dep in our Cargo.toml overrides it
the same way (verified in `zeron/Cargo.lock`; no `[patch]` section needed).

Not needed at all: `crates/text` (zeron-text, pretext-style analytic layout — zeron-ui does not depend on it; it serves the
mobile core), `crates/{client,proto,rpc,engine,harness,doc,sync,preview,mcp,voice,mobile}`.

---

## 2. App architecture for `familiar-native`

### 2.1 `crates/familiar-host` — shared boot for Tauri and native

Extract from `apps/desktop/src-tauri/src/lib.rs` + `embedded_db.rs` (nothing in them is Tauri-specific except
`tauri::async_runtime::spawn`, `AppHandle`, tray, notification plugin):

```
familiar-host
  src/lib.rs      pub struct Host { boot: watch::Receiver<Boot>, daemon: Arc<Mutex<Daemon>>, signals: broadcast::Sender<Signal>,
                                    pg: Arc<tokio::Mutex<Option<postgresql_embedded::PostgreSQL>>>, pool: OnceCell<PgPool> }
                  pub fn start(rt: tokio::runtime::Handle) -> Host          // = lib.rs boot(): Config::create_default → Config::load
                                                                             //   → embedded_db::start or database_url → start_api → start_daemon
                  pub async fn shutdown(&self)                               // = RunEvent::Exit path: cancel token, 35 s drain, pg.stop() 10 s
                  pub fn pause_daemon / resume_daemon / daemon_status / app_status
                  pub fn bots_folder / open_folder / open_cli_terminal(action)
                  pub async fn claude_status / codex_status                 // + codex_too_old
                  pub fn init_logging()                                      // ~/.familiar/logs/desktop.log, 10 MB truncate
  src/embedded_db.rs   moved verbatim (PORT 47432, role/db "zed")
  src/instance.rs      single-instance lock on ~/.familiar/host.lock (zeron InstanceLock pattern; see 2.6) — important while
                       Tauri and native coexist: two hosts = two daemons on one database
```

`Boot` keeps the `phase: starting|database|ready|error` contract so `apps/web/src/lib/desktop.ts::AppStatus` and the native
first-run screen share semantics. Tauri's `lib.rs` shrinks to: build `Host`, map `#[tauri::command]`s onto it, tray menu,
`forward_signals` → `tauri_plugin_notification`. `familiar_server::serve` and `familiar_core::run_with_signals` already
take plain tokio futures, so the only change is replacing `tauri::async_runtime::spawn` with `rt.spawn`.

### 2.2 Token without a password (native only)

Add to familiar-server (`crates/familiar-server/src/auth.rs`): make `new_session` reachable as
`pub async fn mint_owner_session(pool: &PgPool) -> R<Option<(String, User)>>` — returns `None` while no user exists,
otherwise inserts a 30-day session for the single owner exactly like `login` does (`sessions(token_hash, user_id,
expires_at)`), no password check. Expose it from `familiar_server::auth`. The native app runs **in the same process** as
the server and shares the `PgPool` through `Host`, so it calls this directly; no HTTP, no secret on disk.

Fallback for a future out-of-process UI: `POST /api/auth/local {secret}` guarded by (a) `ConnectInfo` peer address is
loopback and (b) `secret == Config.local_bootstrap_secret`, a 32-byte random value the host generates per process and
passes into `familiar_server::Config` and to the UI in memory. Not needed for v0; document it, do not build it.

Setup when no user exists yet: the native first-run still calls `POST /api/auth/setup {email,password}` (web/phone need a
password to log in to the same database), then mints its own session. The spike's `owner_password.txt` hack
(`apps/native/src/main.rs:55`) goes away. 401 anywhere → re-mint (owner may have revoked sessions via
`PATCH /api/auth/account`).

### 2.3 Client layer `crates/familiar-client` (no gpui; mirrors `apps/web/src/lib`)

- `types.rs`: serde structs for every row in `apps/web/src/lib/types.ts` (Bot, BotOverview, Overview, Thread, Message,
  Run, RunEvent, Approval, Rule, Schedule, Memory, Device(+Info), Artifact, Skill, ConnectorPreset, Connector, Channel,
  Trigger, LiveInfo). Hand-written like the web (server routes return sqlx rows/serde_json::Value; sharing types with the
  server would mean refactoring `crates/familiar-server/src/routes/*` — postpone).
- `api.rs`: `Api { base: Url, token: ArcSwap<Option<String>>, http: reqwest::Client }` with typed methods
  (`overview()`, `bots()`, `threads(bot)`, `messages(thread, before, limit)`, `runs_for_thread/bot`, `events(run,
  after_seq)`, `cancel_run`, `approvals(status, bot)`, `decide(approval, decision, response)`, `schedules`, `memories`,
  `rules`, `connectors*`, `channels*`, `triggers*`, `live_info(bot)`, `live_frame_bytes(bot)`, `live_input(bot, Input)`,
  `artifact_bytes(id)`). `ApiError { status: u16, message }`; 401 clears the token and emits `ClientEvent::SignedOut`.
  GET retry ×2, 3 s "slow" timer → `waking` watch channel (port of `useWaking`). Request de-dup: `inflight:
  Mutex<HashMap<String, Shared<BoxFuture<...>>>>`; stale-while-revalidate cache `HashMap<String, Cached { json: String,
  value: Arc<T> }>` compared by JSON string so unchanged results keep identity (port of `remember()`); cleared on any
  mutation or notice (port of `invalidateRequests`).
- `stream.rs`: SSE over `reqwest` bytes stream with a hand-rolled `text/event-stream` parser (`event:`/`data:` lines,
  blank-line dispatch); emits `Push::Notice{t,id,op,run,bot}`, `Push::Delta{run,kind,text}`, `Push::Resync`; reconnect
  with backoff; a reconnect after a drop emits `Resync` (web: `es.onopen` after `dropped`). Server source of truth:
  `crates/familiar-server/src/routes/stream.rs` (events `notice|delta|resync`, keep-alive 25 s, `?token=` query auth).
- Optional later: in-process shortcut that subscribes to `AppState.events: broadcast::Sender<Push>` directly
  (`familiar-server/src/lib.rs`) — same `Push` enum, skipping HTTP. Keep HTTP+SSE first so the native app can also point at
  a remote familiar-server (`FAMILIAR_API_URL`) like the web app.

### 2.4 GPUI bridging (`apps/native/src/live.rs`, `loader.rs`)

- `Live` (one `Entity`, stored as a `Global`): owns the SSE task via `gpui_tokio::Tokio::spawn`, hops to the foreground with
  `cx.spawn`, and fans out. `Live::subscribe(tables: &[&str], scope: Scope{bot,run}, cx, f) -> Subscription` with the
  150 ms trailing coalescing and "resync wins" rule from `apps/web/src/lib/hooks.ts::useLive`. Deltas go to a separate
  `subscribe_delta`.
- `Loader<T>`: port of `useLoad` — `Entity<Loader<T>>` holding `Loadable<T>` (vendored from popover.rs) + key; `reload()`
  keeps old data visible; `set(|t| ...)` for optimistic updates (Chat's outgoing bubble, approvals removal); results
  pass through the client cache so identical JSON yields the same `Arc<T>` and the view skips work.
- Prefetch (`apps/web/src/lib/prefetch.ts`): `Api::prefetch(path)` warms the de-dup map for 4 s; call on sidebar hover
  (`on_hover`) like the web's `onMouseEnter`.

### 2.5 State model

One `AppState` entity = port of `apps/web/src/lib/appdata.tsx` (overview + pending approvals loaders, bot identity memo,
`state_of(bot) -> MascotState`, `pc_online`, `throttle`, 60 s overview poll) plus per-screen entities owning their own
loaders (Shell, Today, Chat{ThreadList, ThreadView, Transcript, Composer}, Approvals, BotPage{tab}, ComputerPanel,
Memory, Settings, Integrations). Same split zeron uses (AppState + Shell + Transcript + Composer). Navigation: `Route`
enum in Shell (Today | Approvals | Bot{slug, tab, sub} | Integrations | Rules | Settings) + a history Vec for
back/forward (zeron shell.rs ~827 "navigation history").

### 2.6 Desktop integration (GPUI gaps, verified against zui @667d0aaf)

- **Tray**: GPUI has none. Recommend the `tray-icon` + `muda` crates (tauri-apps, MIT/Apache) created on the GPUI main
  thread after `app.run`; they need the thread's Win32 message pump, which gpui_windows runs (`dispatcher.rs`). Receive
  events via `TrayIconEvent::set_event_handler`/`MenuEvent::set_event_handler` → `futures::channel::mpsc` → `cx.spawn`
  drain. Unverified: whether tray-icon's hidden message window coexists with gpui's loop without an explicit
  `TranslateMessage/DispatchMessage` for its HWND (it should: gpui dispatches all thread messages). Keeping Tauri as the
  tray host (second process) is not recommended — two hosts, IPC, two daemons.
- **Close-to-tray**: `Window::on_window_should_close` exists (`zui/crates/gpui/src/window.rs:5950`) → return `false` and
  hide. But `Platform::hide` is a no-op on Windows (`gpui_windows/src/platform.rs:483`) and there is no per-window hide
  (only `minimize_window`). Use `raw_window_handle` to get the HWND and call `ShowWindow(hwnd, SW_HIDE)`/`SW_SHOW` via
  `windows-sys`. Risk: gpui may keep rendering a hidden window (check `WM_SHOWWINDOW` handling; unverified).
- **Notifications**: use `cx.show_system_notification(SystemNotification{...})` — gpui_windows implements WinRT toasts
  and registers the AUMID itself for unpackaged apps (`system_notifications.rs::register_app_user_model_id` writes
  `HKCU\Software\Classes\AppUserModelId\<id>`; `platform.rs::set_app_identity` calls
  `SetCurrentProcessExplicitAppUserModelID`). Also set `AppUserModelID:` on the Inno Start-menu shortcut (same id) so
  toasts attribute to the installed app. Focus rule from Tauri's `forward_signals` (quiet while window focused) carries
  over via `window.is_window_active()`.
- **Autostart**: HKCU `Software\Microsoft\Windows\CurrentVersion\Run\Familiar = "<exe>" --hidden` with `windows-registry`
  (already in the gpui_windows graph), or the `auto-launch` crate. Tauri currently uses `tauri-plugin-autostart` with
  `--hidden`; keep the flag.
- **Single instance**: named mutex `Local\dev.familiar.desktop` + a `WM_COPYDATA`/named-pipe nudge to show the running
  window; plus the host-level lock file so Tauri and native cannot both run a daemon (Tauri uses
  `tauri-plugin-single-instance`, which does not know about the native app).
- **Window chrome**: zeron on Windows keeps `TitlebarOptions{ title: Some("Zeron"), appears_transparent: true }` and draws
  its own strip with `app_owns_titlebar_drag: true` (`zeron/crates/ui/src/lib.rs:376-413`); Acrylic frost needs the OS
  transparency setting and the `BackdropBlur` fork primitive — stay opaque in v0. Window geometry restore from
  `settings.rs WindowGeometry`.

---

## 3. Screen port order (web line counts as effort proxy; `apps/web/src`)

| # | Screen | Web source | Notes / effort |
|---|---|---|---|
| A1 | Host extraction + client + Live/Loader | `lib/{api,hooks,appdata,prefetch}.ts(x)` 500 | 3-5 days; unit-test the SSE parser and coalescer |
| A2 | Theme/typography/motion/icons/popover vendoring | — | 2-3 days (mostly renames + token table) |
| A3 | Shell + sidebar + first-run | `components/Shell.tsx` 167, `pages/Onboarding.tsx` 193, `CreateBotDialog.tsx` 71, `EngineFields.tsx` 38 | 3-4 days. Boot phases from `Host`, Claude/Codex check, setup, first teammate |
| A4 | Today | `pages/Home.tsx` 164 | 2 days (greeting, bot cards with mascots, runs feed, schedules, pending approvals) |
| B1 | Chat: thread list + transcript + composer | `pages/Chat.tsx` 242, `EventList.tsx` 205, `ApprovalCard.tsx` 105 | 2-3 weeks. `list(ListState::new(n, ListAlignment::Bottom, px(320)))` + vendored `StickSpring` (70 px band, wheel-up releases); rows = messages + run events (`text/thinking/tool_call/tool_result/approval/artifact/error/result`) + live delta bubble (append to an `IncrementalParser`, drop on persisted `text`/run end) + inline approval card; `rows_for_entry`-style fingerprint cache + `diff_rows` → one `splice`; composer = `gpui_base::Textarea`, Enter sends, Shift+Enter newline, optimistic outgoing bubble (6 s safety timer), auto-title via `PATCH /api/threads/:id` |
| B2 | Mascot | `components/Mascot.tsx` 163, `AvatarBuilder.tsx` 59 | 3 days. See 3.1 |
| C1 | Approvals inbox, Activity, Rules, Schedules, Memory | 28+85+149+137+126 | 1 week with gpui-base forms; Memory = proposed-on-top Accept/Edit/Reject |
| D1 | Computer panel | `components/ComputerPanel.tsx` 136, `Files.tsx` 47, `Artifacts.tsx` 30 | 1 week. Poll `GET /api/bots/:id/live` on `live_frames` notices, fetch `/live.jpg` bytes → decode with `image` off-thread → `img(ImageSource::Render(Arc<RenderImage>))` (vendored `image_media.rs`); take-over: mouse down/up/move/scroll and key events on the image mapped to frame px → `POST /live/input`; URL bar → `navigate` |
| E1 | Settings, AppSettings, Integrations, BotConnectors, Triggers, Skills | 67+208+324+55+144+37 | 1-2 weeks, form-heavy → gpui-base |
| F1 | Tray, autostart, single instance, close-to-tray, toasts, update strip, installer | — | 1 week + packaging |
| F2 | Remove Tauri once parity is reached | `apps/desktop` | — |

Postpone: VS Code theme import / custom theme library, Acrylic/frost, mermaid, dictation, terminal, sounds, the full
27-grammar syntax set, macOS/Linux packaging (Windows first), in-process SSE shortcut, shared API type crate.

### 3.1 Avatars

`Mascot.tsx` is multi-colour SVG (body fill from a 6-entry palette, gradient overlay, white eyes, ink features,
accessories). gpui `svg()` tints a whole SVG with `text_color` (monochrome), so it cannot render the mascot directly.
Options: (a) **pre-render with `resvg`** (gpui already links resvg/usvg) into a `RenderImage`, cached per
(avatar, state, size, scale factor) — exact parity with the web artwork, ~20 variants per bot; (b) draw with
`PathBuilder` in a `canvas` — needs an SVG path-data parser (lyon's `lyon_svg`/`svgtypes` path parser; unverified in the
graph) and re-implementing the gradient. Recommend (a). Motion: `working` bob = animate a relative `top` inset (zeron
motion.rs translateY technique), `needs-you` ring = gpui circle/border drawn around the image, tilts (+6°/−3°) = rasterize
tilted variants (SVG root `transform`), `paused` desaturation = bake into the rasterization. Honour `ReduceMotion`.
The avatar JSON (`bots.avatar`: shape, color, eyes, mouth, accessory) and `defaultAvatar(id)` FNV hash port 1:1.

---

## 4. Windows specifics

- **Renderer/text**: zui `gpui_windows` = Direct3D 11 renderer (`directx_renderer.rs`, `directx_devices.rs`,
  `directx_atlas.rs`), DirectWrite text (`direct_write.rs`), `windows = 0.61`, `accesskit_windows` (UIA), WinRT toasts,
  DirectManipulation. HLSL shaders are compiled at **build time by `fxc.exe`** from the Windows SDK
  (`gpui_windows/build.rs`; override with `GPUI_FXC_PATH`; zeron's CI locates it under
  `${env:ProgramFiles(x86)}/Windows Kits/10/bin/*/x64/fxc.exe`, see `zeron/.github/actions/windows-ci-setup/action.yml`).
  Toolchain per `zeron/docs/reference/windows-development.md`: stable MSVC Rust (zeron pins its own
  `rust-toolchain.toml`; our workspace is edition 2024 — check the pinned zui compiles on our toolchain), VS C++ build
  tools, Windows SDK, Git; LLVM/clang only for aarch64 (`ring`). `gpui_platform` enables gpui's `windows-manifest`
  feature on Windows (embeds `resources/windows/gpui.manifest.xml` via `embed-resource` for DPI awareness).
- **App icon**: GPUI loads the window icon from **resource ID 1**: `dist/windows/familiar.rc` = `1 ICON "familiar.ico"` and
  a `build.rs` like `zeron/apps/zeron/build.rs` (`embed_resource::compile_for(rc, &["familiar-native"], NONE)
  .manifest_required()`). Reuse `apps/desktop/src-tauri/icons/icon.ico`.
- **Console**: `#![cfg_attr(windows, windows_subsystem = "windows")]` + zeron's `attach_parent_console()`
  (`apps/zeron/src/main.rs`) so `--version` works from a terminal (the updater and packaging script probe it).
- **Installer**: Inno Setup 6 (`winget install JRSoftware.InnoSetup`), script `zeron/dist/windows/zeron.iss`: per-user
  (`PrivilegesRequired=lowest`, `DefaultDirName={autopf}\…` → `%LOCALAPPDATA%\Programs\…`), `CloseApplications=yes`
  (Restart Manager), `ArchitecturesAllowed=x64compatible`, `MinVersion=10.0`, URL-scheme registry keys,
  `[UninstallDelete]` for updater leftovers, `[Run] postinstall`. Driver `zeron/scripts/package-windows.ps1`:
  `cargo build --release --locked`, probe `--version`, stage `<name>-<ver>-windows-<arch>/` with exe +
  `<name>-update.json {releases_url}` + LICENSE/notices/font licenses, `Compress-Archive` zip, copy the bare exe as the
  updater payload, `ISCC /DAppVersion /DArch /DPackageDir /DOutputDir`, write `manifest.json {version, files:{<exe>:
  {sha256}}}`. Release workflow uploads zip/exe/setup.exe + manifest to GitHub Releases;
  `releases_url = https://github.com/<owner>/<repo>/releases/latest/download`. Add `AppUserModelID: "dev.familiar.desktop"`
  to the `[Icons]` entries. Familiar-specific: the installer must not touch `~/.familiar` (pgdata!) and the updater's
  relaunch must run after `Host::shutdown` (embedded Postgres stopped) — apply on the "Restart to update"/quit path only.
- **Auto-update flow** (from `crates/update`): hourly `GET <releases_url>/manifest.json` → `version_newer` → `stage()`
  downloads `<name>-<ver>-windows-x86_64.exe` into a temp dir beside the install, verifies SHA-256 (mandatory on
  Windows), runs the staged binary with `--version`; `apply()` renames the running exe to `.old`, moves the staged one
  in, refreshes `DisplayVersion` under `HKCU\…\Uninstall\{AppId}_is1`, relaunches with `--wait-for-exit <pid>`;
  `cleanup_previous_image()` at startup. Unmanaged builds (no update json) only show an advisory link.
- **Code signing**: zeron does **not** sign Windows binaries (only macOS Developer ID in `release.yml`); unsigned
  setup.exe trips SmartScreen. Options: Azure Trusted Signing (requires a verified org; price unverified) or an OV/EV
  certificate; sign the exe **before** hashing/staging (updater verifies the hash of what it downloads) and the installer
  via Inno `SignTool=`. Budget this; it is not a code problem.

---

## 5. Risks

- **GPUI API churn / fork maintenance**: we pin zui 667d0aaf and gpui-base 2f73e5c2 exactly like zeron; vendored code
  uses fork-only primitives (`EdgeFade`, `BackdropBlur`, `ImageSource::evict`, per-pixel fades). Bumping means rebasing
  zeron's patch stack (listed in `zeron/Cargo.toml` comments). Mitigation: follow zeron's bumps rather than upstream Zed.
- **Compile times**: clean build = gpui (+ DirectX shaders via fxc) + tokio + reqwest + ≤11 tree-sitter C grammars +
  embedded-postgres + sqlx. Our `[profile.dev.package."*"] opt-level = 2` matches zeron. Expect several minutes clean on
  a laptop (unverified); zeron's Windows CI uses 45-minute job timeouts and an opt-level-0 release override for its own
  crates. Keep `familiar-ui`/`familiar-syntax` separate crates so the app crate stays small.
- **Licensing** (verified): zeron MIT (© 2026 Wing; keep LICENSE + THIRD_PARTY_NOTICES entries). zui: `gpui*`,
  `gpui_platform`, `gpui_tokio`, `gpui_windows`, `collections`, `sum_tree`, `scheduler`, `refineable`, `media`,
  `http_client*` are Apache-2.0; the **only GPL crate is `path` (GPL-3.0-or-later)**, used solely by zui's `util` crate;
  neither `path` nor `util` appears in `zeron/Cargo.lock` (only `gpui_util`, Apache) → not in our graph. zui `NOTICE`
  (egoist/zed overlay adaptation) must be kept. gpui-base Apache-2.0 (Longbridge). Solar Icons CC BY 4.0 (attribution
  required). Geist OFL 1.1. tree-sitter + grammars MIT (`THIRD_PARTY_NOTICES.md` table). mermaid-rs-renderer MIT (if
  ever). zeron's `assets/sounds/*.wav` carry no licence note → do not copy. Brand SVGs → exclude.
- **Binary size**: `familiar-desktop.exe` debug is 47 MB today; a release native exe with gpui + 11 grammars + sqlx +
  reqwest is plausibly 30-60 MB (unverified; zeron publishes no size). Embedded Postgres (~tens of MB) is downloaded at
  first run, not bundled — unchanged from Tauri.
- **Accessibility**: zui ships accesskit + `accesskit_windows` (UIA), but only elements that set roles are exposed
  (zeron sets `Role::MultilineTextInput` on its composer); keyboard focus order is hand-built with `FocusHandle`s. Plan a
  Narrator pass; treat full screen-reader support as post-v0.
- **Tray/hide integration** is the least verified part (gpui `hide` no-op on Windows, tray-icon message pump). Prototype
  it in week 1 alongside the host extraction.
- **Two shells during transition**: Tauri + native both embed the daemon and the API on 47080; without the host-level
  lock the second one fails to bind 47080 and runs a second daemon against the same database. Build `familiar-host`
  first and make both apps use it.
- **Composer quality**: gpui-base `Textarea` vs zeron's hand-rolled `ComposerInput` (IME marked ranges, caret blink,
  drag selection, auto-grow 76-260 px). Validate IME (CJK) early; keep extraction of `ComposerInput` as the fallback.
- **Text/measurement**: zeron deliberately did not use pretext-style analytic heights on desktop (`list()` measures);
  cold-open scroll offsets for long threads are estimated — acceptable, messages are paged (`?limit=200`).
