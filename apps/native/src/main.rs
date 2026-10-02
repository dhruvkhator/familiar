//! Familiar native desktop (GPUI) — spike: a window that signs in to the local Familiar API and lists your
//! teammates with their live status. Proves the toolchain (GPUI on Windows via zeronsh/zui) and the API path
//! before the real screens are ported.

use gpui::{
    App, AppContext, Bounds, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, WindowBounds,
    WindowOptions, div, point, px, rgb, size,
};
use serde::Deserialize;

const API: &str = "http://127.0.0.1:47080/api";

#[derive(Debug, Clone, Deserialize)]
struct Bot {
    name: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

enum Load {
    Loading,
    Ready(Vec<Bot>),
    Failed(String),
}

struct Root {
    load: Load,
}

impl Root {
    fn new(cx: &mut Context<Self>) -> Self {
        // Network on the Tokio runtime gpui_tokio provides; the result comes back to the UI thread.
        let task = gpui_tokio::Tokio::spawn(cx, fetch_bots());
        cx.spawn(async move |this, cx| {
            let load = match task.await {
                Ok(Ok(bots)) => Load::Ready(bots),
                Ok(Err(e)) => Load::Failed(format!("{e:#}")),
                Err(e) => Load::Failed(format!("{e:#}")),
            };
            let _ = this.update(cx, |this, cx| {
                this.load = load;
                cx.notify();
            });
        })
        .detach();
        Root { load: Load::Loading }
    }
}

async fn fetch_bots() -> anyhow::Result<Vec<Bot>> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    let password = std::env::var("FAMILIAR_PASSWORD").or_else(|_| {
        std::fs::read_to_string(std::path::Path::new(&home).join(".familiar").join("owner_password.txt"))
    })?;
    let email = std::env::var("FAMILIAR_EMAIL").unwrap_or_else(|_| "owner@familiar.local".into());
    let http = reqwest::Client::new();
    let login: serde_json::Value = http
        .post(format!("{API}/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": password.trim() }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["token"].as_str().ok_or_else(|| anyhow::anyhow!("no token in login response"))?;
    Ok(http.get(format!("{API}/bots")).bearer_auth(token).send().await?.error_for_status()?.json().await?)
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.load {
            Load::Loading => div().text_color(rgb(0x8c8d97)).child("Connecting to Familiar…"),
            Load::Failed(e) => div().text_color(rgb(0xd9534f)).child(SharedString::from(format!("Couldn't load: {e}"))),
            Load::Ready(bots) if bots.is_empty() => div().text_color(rgb(0x8c8d97)).child("No teammates yet."),
            Load::Ready(bots) => div().flex().flex_col().gap_2().children(bots.iter().map(|b| {
                let status = b.status.clone().unwrap_or_else(|| "idle".into());
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(0x23252e))
                    .child(div().size(px(28.)).rounded_full().bg(rgb(0x5269bb)))
                    .child(div().flex().flex_col().child(div().text_color(rgb(0xf3f3f5)).child(SharedString::from(b.name.clone()))).child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x8c8d97))
                            .child(SharedString::from(format!("{status} · {}", b.model.clone().unwrap_or_default()))),
                    ))
            })),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(0x17181d))
            .child(div().text_xl().text_color(rgb(0xf3f3f5)).child("Familiar"))
            .child(div().text_color(rgb(0x8c8d97)).child("Your teammates"))
            .child(body)
    }
}

fn main() {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    gpui_platform::application().run(|cx: &mut App| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(900.), px(640.)));
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |_, cx| cx.new(Root::new),
        )
        .expect("open window");
        cx.activate(true);
    });
}
