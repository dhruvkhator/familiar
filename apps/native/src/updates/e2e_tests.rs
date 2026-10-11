//! End to end against a fake release server on 127.0.0.1: the release list, the signed manifest and the artifact,
//! as GitHub serves them, into a temporary updates folder. Nothing is installed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::source::{self, CheckError, Found, ListCache, Source};
use super::verify::{self, Platform};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/updates/fixtures");

#[derive(Clone)]
enum Reply {
    Ok(Vec<u8>),
    /// 302 to this URL.
    Redirect(String),
}

#[derive(Default)]
struct Server {
    routes: HashMap<String, Reply>,
    hits: HashMap<String, usize>,
}

/// Serve `routes` (path → reply) until the test ends; `/releases` answers 304 to its own ETag.
async fn serve(routes: HashMap<String, Reply>) -> (String, Arc<Mutex<Server>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(Server { routes, hits: HashMap::new() }));
    let shared = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let state = shared.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = sock.read(&mut buf).await else { return };
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let req = String::from_utf8_lossy(&req).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let route = path.split('?').next().unwrap().to_owned();
                let reply = {
                    let mut s = state.lock().unwrap();
                    *s.hits.entry(route.clone()).or_default() += 1;
                    s.routes.get(&route).cloned()
                };
                let etag_hit = route == "/releases" && req.to_ascii_lowercase().contains("if-none-match: \"v1\"");
                let head = match (&reply, etag_hit) {
                    (_, true) => "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nContent-Length: 0\r\n".to_owned(),
                    (Some(Reply::Ok(body)), _) => format!("HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {}\r\n", body.len()),
                    (Some(Reply::Redirect(to)), _) => format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n"),
                    (None, _) => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n".to_owned(),
                };
                let _ = sock.write_all(format!("{head}Connection: close\r\n\r\n").as_bytes()).await;
                if let (Some(Reply::Ok(body)), false) = (reply, etag_hit) {
                    let _ = sock.write_all(&body).await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, state)
}

fn fixture(release: &str, file: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(release).join(file)).unwrap()
}

fn release_list(tag: &str, prerelease: bool) -> Vec<u8> {
    let v = tag.trim_start_matches('v');
    serde_json::to_vec(&serde_json::json!([{
        "tag_name": tag,
        "draft": false,
        "prerelease": prerelease,
        "assets": [
            {"name": "familiar-update.json"},
            {"name": "familiar-update.json.minisig"},
            {"name": format!("Familiar-{v}-setup.exe")},
            {"name": format!("Familiar-{v}-setup.exe.minisig")},
        ],
    }]))
    .unwrap()
}

/// The 9.9.9 fixture release, with `edit` applied to the routes.
fn release_999(release_dir: &str, edit: impl FnOnce(&mut HashMap<String, Reply>)) -> HashMap<String, Reply> {
    let mut routes = HashMap::new();
    routes.insert("/releases".into(), Reply::Ok(release_list("v9.9.9", false)));
    for f in ["familiar-update.json", "familiar-update.json.minisig", "Familiar-9.9.9-setup.exe", "Familiar-9.9.9-setup.exe.minisig"] {
        routes.insert(format!("/download/v9.9.9/{f}"), Reply::Ok(fixture(release_dir, f)));
    }
    edit(&mut routes);
    routes
}

async fn run(base: &str, cache: &Mutex<ListCache>, current: &str, dir: &Path) -> Result<Found, CheckError> {
    let source = Source::local(base);
    let http = source.client().unwrap();
    let key = verify::public_key(&std::fs::read_to_string(Path::new(FIXTURES).join("test.pub")).unwrap()).unwrap();
    let current = verify::parse_version(current).unwrap();
    source::check(&source, &http, cache, &key, &current, true, Platform::Windows, dir, &|_, _, _| {}).await
}

fn failed(r: Result<Found, CheckError>) -> String {
    match r {
        Err(CheckError::Failed(e)) => e,
        Err(CheckError::RateLimited(_)) => panic!("rate limited"),
        Ok(Found::UpToDate) => panic!("up to date"),
        Ok(Found::Ready(s)) => panic!("staged {}", s.version),
    }
}

/// Nothing runnable was left behind: no artifact under its real name, no partial download.
fn nothing_staged(dir: &Path) {
    let stage = dir.join("9.9.9");
    assert!(!stage.join("Familiar-9.9.9-setup.exe").exists(), "a refused artifact was put in place");
    assert!(!stage.join("Familiar-9.9.9-setup.exe.part").exists(), "a partial download was left");
}

#[tokio::test]
async fn a_signed_release_is_downloaded_verified_and_staged_once() {
    let (base, server) = serve(release_999("good", |_| {})).await;
    let dir = tempfile::tempdir().unwrap();
    let cache = Mutex::new(ListCache::default());
    let Ok(Found::Ready(staged)) = run(&base, &cache, "0.1.0-alpha.3", dir.path()).await else { panic!("not staged") };
    assert_eq!(staged.version.to_string(), "9.9.9");
    assert!(staged.notes.starts_with("Faster chats."));
    assert_eq!(staged.artifact, dir.path().join("9.9.9").join("Familiar-9.9.9-setup.exe"));
    assert_eq!(std::fs::read(&staged.artifact).unwrap(), fixture("good", "Familiar-9.9.9-setup.exe"));
    for f in ["familiar-update.json", "familiar-update.json.minisig", "Familiar-9.9.9-setup.exe.minisig"] {
        assert!(dir.path().join("9.9.9").join(f).is_file(), "{f} kept for the helper");
    }
    // Again: the list is a 304 and the staged installer is re-verified, not downloaded again.
    let Ok(Found::Ready(_)) = run(&base, &cache, "0.1.0-alpha.3", dir.path()).await else { panic!("not staged") };
    let hits = server.lock().unwrap().hits.clone();
    assert_eq!(hits["/releases"], 2);
    assert_eq!(hits["/download/v9.9.9/Familiar-9.9.9-setup.exe"], 1);
}

#[tokio::test]
async fn a_tampered_installer_is_refused_and_never_put_in_place() {
    let mut bad = fixture("good", "Familiar-9.9.9-setup.exe");
    bad[3] ^= 0x20;
    let (base, _) = serve(release_999("good", |r| {
        r.insert("/download/v9.9.9/Familiar-9.9.9-setup.exe".into(), Reply::Ok(bad));
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let e = failed(run(&base, &Mutex::default(), "0.1.0", dir.path()).await);
    assert!(e.contains("checksum"), "{e}");
    nothing_staged(dir.path());
}

#[tokio::test]
async fn an_installer_larger_than_signed_is_refused() {
    let mut big = fixture("good", "Familiar-9.9.9-setup.exe");
    big.extend_from_slice(&[0u8; 4096]);
    let (base, _) = serve(release_999("good", |r| {
        r.insert("/download/v9.9.9/Familiar-9.9.9-setup.exe".into(), Reply::Ok(big));
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let e = failed(run(&base, &Mutex::default(), "0.1.0", dir.path()).await);
    assert!(e.contains("size") || e.contains("larger"), "{e}");
    nothing_staged(dir.path());
}

#[tokio::test]
async fn a_release_signed_with_another_key_is_refused() {
    let (base, server) = serve(release_999("otherkey", |_| {})).await;
    let dir = tempfile::tempdir().unwrap();
    let e = failed(run(&base, &Mutex::default(), "0.1.0", dir.path()).await);
    assert!(e.contains("isn't signed"), "{e}");
    nothing_staged(dir.path());
    // Refused at the manifest: the installer was never even requested.
    assert!(!server.lock().unwrap().hits.contains_key("/download/v9.9.9/Familiar-9.9.9-setup.exe"));
}

#[tokio::test]
async fn an_older_release_is_never_offered() {
    let mut routes = HashMap::new();
    routes.insert("/releases".to_owned(), Reply::Ok(release_list("v0.0.1", false)));
    for f in ["familiar-update.json", "familiar-update.json.minisig", "Familiar-0.0.1-setup.exe", "Familiar-0.0.1-setup.exe.minisig"] {
        routes.insert(format!("/download/v0.0.1/{f}"), Reply::Ok(fixture("old", f)));
    }
    let (base, server) = serve(routes).await;
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(run(&base, &Mutex::default(), "0.1.0-alpha.1", dir.path()).await, Ok(Found::UpToDate)));
    assert_eq!(server.lock().unwrap().hits.len(), 1, "only the list was fetched");
}

#[tokio::test]
async fn a_genuine_manifest_replayed_under_a_newer_tag_is_refused() {
    // 0.0.1's real, signed manifest served as release 9.9.9's.
    let (base, _) = serve(release_999("good", |r| {
        r.insert("/download/v9.9.9/familiar-update.json".into(), Reply::Ok(fixture("old", "familiar-update.json")));
        r.insert("/download/v9.9.9/familiar-update.json.minisig".into(), Reply::Ok(fixture("old", "familiar-update.json.minisig")));
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let e = failed(run(&base, &Mutex::default(), "0.1.0", dir.path()).await);
    assert!(e.contains("another file"), "{e}");
    nothing_staged(dir.path());
}

#[tokio::test]
async fn redirects_off_the_allowed_hosts_are_refused() {
    let (base, _) = serve(release_999("good", |r| {
        r.insert("/download/v9.9.9/Familiar-9.9.9-setup.exe".into(), Reply::Redirect("http://localhost:9/evil.exe".into()));
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let e = failed(run(&base, &Mutex::default(), "0.1.0", dir.path()).await);
    assert!(e.contains("redirect"), "{e}");
    nothing_staged(dir.path());
}

#[tokio::test]
async fn stale_stages_are_pruned_and_the_apply_report_kept() {
    let (base, _) = serve(release_999("good", |_| {})).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("1.0.0")).unwrap();
    std::fs::create_dir_all(dir.path().join("not-a-version")).unwrap();
    std::fs::write(dir.path().join(super::apply::RESULT), b"{}").unwrap();
    let Ok(Found::Ready(_)) = run(&base, &Mutex::default(), "0.1.0", dir.path()).await else { panic!("not staged") };
    assert!(!dir.path().join("1.0.0").exists());
    assert!(dir.path().join("not-a-version").exists(), "only version folders are removed");
    assert!(dir.path().join(super::apply::RESULT).exists());
}
