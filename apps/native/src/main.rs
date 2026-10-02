//! Familiar native desktop (GPUI).
//!
//! `familiar-native` opens the app shell: a sidebar of teammates with their live status (signed in to the local
//! Familiar API) and a "Today" mock. `familiar-native --gallery` opens the design-system gallery used to review the
//! look: every component, both themes, the mascot in every state, and the motion primitives.
//!
//! Flags (both windows): `--theme light|dark|system`, `--reduce-motion`; gallery only: `--section <name>`.

// Release builds are GUI-subsystem binaries (no console window); debug builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod data;
mod gallery;
mod shell;

use familiar_ui::AppearanceMode;
use gpui::{
    App, AppContext as _, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

struct Args {
    gallery: bool,
    theme: AppearanceMode,
    reduce_motion: bool,
    section: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args { gallery: false, theme: AppearanceMode::System, reduce_motion: false, section: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--gallery" => args.gallery = true,
            "--reduce-motion" => args.reduce_motion = true,
            "--theme" => {
                args.theme = match it.next().as_deref() {
                    Some("light") => AppearanceMode::Light,
                    Some("dark") => AppearanceMode::Dark,
                    _ => AppearanceMode::System,
                }
            }
            "--section" => args.section = it.next(),
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
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let args = parse_args();
    gpui_platform::application().with_assets(familiar_ui::icons::Assets).run(move |cx: &mut App| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        familiar_ui::init(args.theme, cx);
        if args.reduce_motion {
            familiar_ui::motion::set_preference(familiar_ui::motion::ReduceMotion::On, cx);
        }
        let opened = if args.gallery {
            let section = args.section.clone();
            cx.open_window(window_options(cx, "Familiar — Gallery", 1240.0, 860.0), move |window, cx| {
                cx.new(|cx| gallery::Gallery::new(section.as_deref(), window, cx))
            })
            .map(|_| ())
        } else {
            cx.open_window(window_options(cx, "Familiar", 1180.0, 780.0), |window, cx| {
                cx.new(|cx| shell::Shell::new(window, cx))
            })
            .map(|_| ())
        };
        opened.expect("open window");
        cx.activate(true);
    });
}
