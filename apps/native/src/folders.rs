//! Folders on this PC shared with a teammate: the system folder picker, and sharing the picked folder. The API decides
//! what may be shared (never a drive, the home folder, a hidden settings folder, app data, Familiar's data, a workspace
//! or a system folder) and says why in plain words.

use std::path::PathBuf;

use familiar_client::{Client, Folder};
use gpui::{Context, PathPromptOptions};
use gpui_tokio::Tokio;
use uuid::Uuid;

/// Open the system folder picker; `then` gets the folder (nothing happens when it's cancelled).
pub fn pick<T: 'static>(cx: &mut Context<T>, then: impl FnOnce(&mut T, PathBuf, &mut Context<T>) + 'static) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Share this folder".into()),
    });
    cx.spawn(async move |this, cx| {
        let Ok(Ok(Some(mut paths))) = rx.await else { return };
        let Some(path) = paths.pop() else { return };
        let _ = this.update(cx, |p, cx| then(p, path, cx));
    })
    .detach();
}

/// Share `path` with the teammate, read only; `done` gets the new folder or the API's reason.
pub fn share<T: 'static>(
    client: Client,
    bot: Uuid,
    path: PathBuf,
    cx: &mut Context<T>,
    done: impl FnOnce(&mut T, Result<Folder, String>, &mut Context<T>) + 'static,
) {
    let path = path.display().to_string();
    let task = Tokio::spawn(cx, async move { client.add_folder(bot, &path, "read").await });
    cx.spawn(async move |this, cx| {
        let r = task.await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.message()));
        let _ = this.update(cx, |p, cx| done(p, r, cx));
    })
    .detach();
}

/// The last part of a folder's path ("Taxes" for `C:\Users\me\Documents\Taxes`).
pub fn short_name(f: &Folder) -> String {
    std::path::Path::new(&f.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.path.clone())
}
