//! Embeds the Windows app icon as resource ID 1 — the ID gpui_windows' `load_icon` asks for — so the window,
//! taskbar and Explorer all show the Familiar mark. Same shape as zeron's `apps/zeron/build.rs`.
fn main() {
    println!("cargo:rerun-if-changed=dist/windows/familiar.rc");
    println!("cargo:rerun-if-changed=dist/windows/familiar.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for("dist/windows/familiar.rc", &["familiar-native"], embed_resource::NONE)
            .manifest_required()
            .expect("Windows app icon resource compilation failed");
    }
}
