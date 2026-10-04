//! Familiar native desktop (GPUI).
//!
//! `familiar-native` runs the whole local install in-process (familiar-host: built-in database, API, daemon; see
//! `engine.rs`), or attaches to the Familiar desktop app when that already runs it, and opens the app shell: a
//! sidebar of teammates with their live status, Today (needs you, happening now, recently done, coming up) and each teammate's chat. `familiar-native --gallery` opens the design-system gallery used to review the
//! look: every component, both themes, the mascot in every state, and the motion primitives.
//!
//! Flags (both windows): `--theme light|dark|system`, `--reduce-motion`; gallery only: `--section <name>`; shell only:
//! `--open needs|<teammate name>|first` (opens that page once the data is in).

// Release builds are GUI-subsystem binaries (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod approval;
mod bot_settings;
mod chat;
mod data;
mod desktop;
mod engine;
mod gallery;
mod markdown;
mod notify;
mod prefs;
mod root;
mod settings;
mod shell;
mod text_input;
mod tray;

use familiar_ui::AppearanceMode;
use gpui::{
    App, AppContext as _, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

struct Args {
    gallery: bool,
    /// `None`: the saved preference.
    theme: Option<AppearanceMode>,
    reduce_motion: bool,
    section: Option<String>,
    open: Option<String>,
    /// Start in the tray (start at login).
    hidden: bool,
}

fn parse_args() -> Args {
    let mut args = Args { gallery: false, theme: None, reduce_motion: false, section: None, open: None, hidden: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--gallery" => args.gallery = true,
            "--hidden" => args.hidden = true,
            "--reduce-motion" => args.reduce_motion = true,
            "--theme" => {
                args.theme = Some(match it.next().as_deref() {
                    Some("light") => AppearanceMode::Light,
                    Some("dark") => AppearanceMode::Dark,
                    _ => AppearanceMode::System,
                })
            }
            "--section" => args.section = it.next(),
            "--open" => args.open = it.next(),
            "--version" => {
                println!("familiar-native {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => {}
        }
    }
    args
}

fn window_options(cx: &App, title: &str, width: f32, height: f32) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx))),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(title.to_owned())),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_min_size: Some(size(px(820.0), px(560.0))),
        app_id: Some("dev.familiar.desktop".into()),
        ..Default::default()
    }
}

fn main() {
    let args = parse_args();
    // One copy per user: a second launch shows the running window and exits.
    let nudges = match (!args.gallery).then(desktop::single_instance) {
        Some(desktop::Instance::Second) => return,
        Some(desktop::Instance::First(rx)) => Some(rx),
        None => None,
    };
    // Debug builds log to the console; release builds have none and log to ~/.familiar/logs/native.log.
    if cfg!(debug_assertions) {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into());
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        familiar_host::init_logging("native");
    }
    // One runtime for the client, the stream and the hosted engine (database, API, daemon): more than
    // gpui_tokio's default two workers.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("familiar-rt")
        .build()
        .expect("start the tokio runtime");
    let handle = runtime.handle().clone();
    gpui_platform::application().with_assets(familiar_ui::icons::Assets).run(move |cx: &mut App| {
        gpui_tokio::init_from_handle(cx, handle);
        gpui_base::init(cx);
        let saved = prefs::load();
        familiar_ui::init(args.theme.unwrap_or(saved.theme), cx);
        if args.reduce_motion || saved.reduce_motion {
            familiar_ui::motion::set_preference(familiar_ui::motion::ReduceMotion::On, cx);
        }
        if args.gallery {
            let section = args.section.clone();
            cx.open_window(window_options(cx, "Familiar — Gallery", 1240.0, 860.0), move |window, cx| {
                cx.new(|cx| gallery::Gallery::new(section.as_deref(), window, cx))
            })
            .expect("open window");
            cx.activate(true);
            return;
        }
        // Toasts need an app identity when the app isn't packaged (the same id the window and installer use).
        cx.set_app_identity("dev.familiar.desktop", "Familiar");
        notify::init(cx);
        let open = args.open.clone();
        let options = WindowOptions { show: !args.hidden, focus: !args.hidden, ..window_options(cx, "Familiar", 1180.0, 780.0) };
        let window = cx.open_window(options, move |window, cx| cx.new(|cx| root::Root::new(open, window, cx))).expect("open window");
        cx.set_global(desktop::MainWindow(window));
        tray::install(cx);
        if let Some(mut nudges) = nudges {
            cx.spawn(async move |cx| {
                use futures::StreamExt as _;
                while nudges.next().await.is_some() {
                    cx.update(desktop::show_main);
                }
            })
            .detach();
        }
        if !args.hidden {
            cx.activate(true);
        }
    });
    // Normally the window drained the engine before quitting; if the app ended another way, drain it now.
    let host = engine::HOSTED.lock().unwrap().take();
    if let Some(host) = host {
        runtime.block_on(host.shutdown());
    }
}
