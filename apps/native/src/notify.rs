//! Desktop notifications (gpui's `SystemNotification`: WinRT toasts on Windows). Host mode forwards the daemon's
//! signals (approval needed, run finished, a teammate's `notify_user`); attached mode derives the same from the live
//! data (new pending approvals, runs that finished). Quiet while the window is focused, like the Tauri app. Clicking
//! one shows the window on the page it is about.

use futures::StreamExt as _;
use gpui::{App, SharedString, SystemNotification};
use gpui_tokio::Tokio;

use crate::desktop::{self, MainWindow};

/// Wire up clicks. Call once at startup.
pub fn init(cx: &mut App) {
    cx.on_system_notification_response(|response, cx| {
        desktop::show_main(cx);
        // The tag is `<target>|<kind>`; the target is what `Shell::open` takes.
        let target = response.tag.split('|').next().unwrap_or_default().to_owned();
        let Some(handle) = cx.try_global::<MainWindow>().map(|m| m.0) else { return };
        let _ = handle.update(cx, |root, _, cx| {
            if let Some(shell) = root.shell() {
                shell.update(cx, |s, cx| {
                    s.open(&target, cx);
                });
            }
        });
    });
    // Debug builds: `FAMILIAR_TEST_NOTIFY=1` posts a sample a few seconds after launch (hide the window first).
    if cfg!(debug_assertions) && std::env::var_os("FAMILIAR_TEST_NOTIFY").is_some() {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(25)).await;
            cx.update(|cx| show("needs", "approval", "Approval needed", "Scout wants to use Bash", cx));
        })
        .detach();
    }
}

/// Show a notification unless the window is on screen and focused. `target`: `needs`, `bot:<id>` or a teammate's name.
pub fn show(target: &str, kind: &str, title: impl Into<SharedString>, body: impl Into<SharedString>, cx: &mut App) {
    let title = title.into();
    if desktop::main_focused(cx) {
        tracing::debug!("notification skipped (window focused): {title}");
        return;
    }
    tracing::info!("notification: {title}");
    cx.show_system_notification(SystemNotification {
        tag: format!("{target}|{kind}").into(),
        title,
        body: body.into(),
        actions: Vec::new(),
    });
}

/// Host mode: forward the daemon's signals for as long as the host runs.
pub fn watch_host(host: familiar_host::Host, cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<familiar_host::Signal>();
    let mut signals = host.signals();
    let forward = Tokio::spawn(cx, async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match signals.recv().await {
                Ok(s) => {
                    if tx.unbounded_send(s).is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    });
    cx.spawn(async move |cx| {
        let _forward = forward;
        while let Some(signal) = rx.next().await {
            cx.update(|cx| on_signal(signal, cx));
        }
    })
    .detach();
}

fn on_signal(signal: familiar_host::Signal, cx: &mut App) {
    use familiar_host::Signal;
    match signal {
        Signal::ApprovalPending { bot, tool } if tool == "propose_draft" => {
            show("needs", "draft", "Draft to review", draft_body(&bot, "", ""), cx)
        }
        Signal::ApprovalPending { bot, tool } => {
            show("needs", "approval", "Approval needed", format!("{bot} wants to use {tool}"), cx)
        }
        Signal::RunFinished { bot, status } => {
            let (title, body) = finished(&bot, &status);
            show(&bot, "finished", title, body, cx)
        }
        Signal::Notify { bot, message, .. } => show(&bot, "notify", bot.clone(), message, cx),
        Signal::Desktop { bot } => crate::tray::desktop_changed(bot, cx),
    }
}

/// The body of a new draft's notification: "Ada drafted a post for X. Review it in Needs you." (kind and channel when
/// known; the daemon's signal only names the teammate).
pub fn draft_body(bot: &str, kind: &str, channel: &str) -> String {
    let what = match (kind.trim(), channel.trim()) {
        ("", _) => "something to send".to_owned(),
        ("dm", "") => "a message".to_owned(),
        ("dm", c) => format!("a message for {c}"),
        ("email", _) => "an email".to_owned(),
        (k, "") => format!("a {k}"),
        (k, c) => format!("a {k} for {c}"),
    };
    format!("{bot} drafted {what}. Review it in Needs you.")
}

/// Title and body for a finished run.
pub fn finished(bot: &str, status: &str) -> (String, String) {
    match status {
        "succeeded" => (format!("{bot} is done"), "Open Familiar to see what it did.".to_owned()),
        "failed" => (format!("{bot} ran into a problem"), "The run failed. Open Familiar for the details.".to_owned()),
        "cancelled" => (format!("{bot} stopped"), "The run was cancelled.".to_owned()),
        other => (format!("{bot} finished"), format!("Run {other}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_notifications() {
        assert_eq!(draft_body("Ada", "post", "X"), "Ada drafted a post for X. Review it in Needs you.");
        assert_eq!(draft_body("Ada", "email", "Gmail"), "Ada drafted an email. Review it in Needs you.");
        assert_eq!(draft_body("Ada", "", ""), "Ada drafted something to send. Review it in Needs you.");
    }
}
