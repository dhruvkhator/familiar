//! Finding a newer release on GitHub and downloading it, verified, into the updates folder.
//!
//! The release list comes from GitHub's public API (no token; `If-None-Match` so an unchanged list costs no rate
//! limit, and a rate-limited answer says when to come back). Release files are fetched from fixed URLs built from the
//! repository, the tag and a checked file name (`…/releases/download/v<version>/<file>`), never from URLs in the API's
//! answer, and every hop (redirects included) must pass [`verify::allowed_url`].
//!
//! Staging, per version, in `<updates>/<version>/`: the signed manifest and its signature first; then the artifact's
//! signature; then the artifact itself, downloaded as `<file>.part` (not runnable, and capped at the manifest's size),
//! verified from disk and only then renamed to its real name. A staged artifact is verified again before it is
//! reported ready, and once more by the helper that applies it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use minisign_verify::PublicKey;
use semver::Version;
use serde::Deserialize;

use super::verify::{self, MANIFEST, Manifest, Platform, SIG_EXT};

pub const REPO: &str = "dhruvkhator/familiar";
/// How many newer releases one check looks at (newest first) before giving up on finding one for this platform.
const MAX_CANDIDATES: usize = 3;

/// Where releases come from. Production is [`Source::github`]; tests serve a fake release locally.
pub struct Source {
    pub list_url: String,
    /// Release files: `<download>/v<version>/<file>`.
    pub download: String,
    allow: fn(&reqwest::Url) -> bool,
    https_only: bool,
}

impl Source {
    pub fn github() -> Self {
        Self {
            list_url: format!("https://api.github.com/repos/{REPO}/releases?per_page=30"),
            download: format!("https://github.com/{REPO}/releases/download"),
            allow: verify::allowed_url,
            https_only: true,
        }
    }

    /// A fake release server on this machine (tests only: production never allows plain http).
    #[cfg(test)]
    pub fn local(base: &str) -> Self {
        fn loopback(u: &reqwest::Url) -> bool {
            u.scheme() == "http" && u.host_str() == Some("127.0.0.1")
        }
        Self { list_url: format!("{base}/releases"), download: format!("{base}/download"), allow: loopback, https_only: false }
    }

    /// The HTTP client: rustls, short connect/read timeouts (no overall one: a big download on a slow line is fine
    /// while bytes flow), at most 5 redirects, each to an allowed URL.
    pub fn client(&self) -> Result<reqwest::Client, String> {
        let allow = self.allow;
        reqwest::Client::builder()
            .user_agent(format!("Familiar/{} (+https://github.com/{REPO})", super::VERSION))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .https_only(self.https_only)
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if allow(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("redirected to a host Familiar doesn't download from")
                }
            }))
            .build()
            .map_err(|e| e.to_string())
    }

    fn file_url(&self, version: &Version, file: &str) -> String {
        format!("{}/v{version}/{file}", self.download)
    }
}

/// A release as GitHub's API lists it (the fields used).
#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
}

/// The last release list and its ETag, so an unchanged list is a cheap 304.
#[derive(Default)]
pub struct ListCache {
    etag: Option<String>,
    releases: Vec<GhRelease>,
}

pub enum CheckError {
    /// GitHub's rate limit: try again after this long.
    RateLimited(Duration),
    Failed(String),
}

impl From<String> for CheckError {
    fn from(s: String) -> Self {
        CheckError::Failed(s)
    }
}

/// A verified update in the updates folder, ready to apply.
#[derive(Debug, Clone)]
pub struct Staged {
    pub version: Version,
    /// Plain text (see [`verify::plain_notes`]).
    pub notes: String,
    /// The verified artifact; its signature and the signed manifest sit beside it.
    pub artifact: PathBuf,
}

pub enum Found {
    UpToDate,
    Ready(Staged),
}

/// Newer releases worth looking at, newest first: published (not drafts), tagged `v<semver>`, carrying a manifest and
/// this platform's artifact with their signatures, pre-releases only when `early`.
pub fn candidates(releases: &[GhRelease], current: &Version, early: bool, platform: Platform) -> Vec<Version> {
    let mut out: Vec<Version> = releases
        .iter()
        .filter(|r| !r.draft && (early || !r.prerelease))
        .filter_map(|r| verify::tag_version(&r.tag_name).map(|v| (r, v)))
        .filter(|(_, v)| v > current && (early || v.pre.is_empty()))
        .filter(|(r, v)| {
            let artifact = platform.artifact(v);
            let has = |name: &str| r.assets.iter().any(|a| a.name == name);
            [MANIFEST.to_owned(), format!("{MANIFEST}{SIG_EXT}"), artifact.clone(), format!("{artifact}{SIG_EXT}")]
                .iter()
                .all(|n| has(n))
        })
        .map(|(_, v)| v)
        .collect();
    out.sort_by(|a, b| b.cmp(a));
    out.dedup();
    out.truncate(MAX_CANDIDATES);
    out
}

/// GET `url` (which must be allowed), reading at most `cap` bytes.
async fn fetch(http: &reqwest::Client, source: &Source, url: &str, cap: usize) -> Result<Vec<u8>, String> {
    let parsed: reqwest::Url = url.parse().map_err(|_| format!("bad URL {url}"))?;
    if !(source.allow)(&parsed) {
        return Err(format!("{url} isn't a place Familiar downloads from"));
    }
    let mut resp = http.get(parsed).send().await.map_err(|e| format!("couldn't reach GitHub ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {} for {}", resp.status(), file_of(url)));
    }
    if resp.content_length().is_some_and(|n| n > cap as u64) {
        return Err(format!("{} is too large", file_of(url)));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("download interrupted ({e})"))? {
        if out.len() + chunk.len() > cap {
            return Err(format!("{} is too large", file_of(url)));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn file_of(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// The release list, revalidated with the cached ETag.
async fn list(http: &reqwest::Client, source: &Source, cache: &Mutex<ListCache>) -> Result<Vec<GhRelease>, CheckError> {
    let url: reqwest::Url = source.list_url.parse().map_err(|_| "bad release list URL".to_owned())?;
    if !(source.allow)(&url) {
        return Err(CheckError::Failed("the release list isn't on GitHub".into()));
    }
    let mut req = http
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = cache.lock().unwrap().etag.clone() {
        req = req.header("If-None-Match", etag);
    }
    let mut resp = req.send().await.map_err(|e| format!("couldn't reach GitHub ({e})"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(cache.lock().unwrap().releases.clone());
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(CheckError::RateLimited(retry_after(resp.headers())));
    }
    if !status.is_success() {
        return Err(CheckError::Failed(format!("GitHub answered {status}")));
    }
    let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("couldn't read the release list ({e})"))? {
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err(CheckError::Failed("the release list is too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    let releases: Vec<GhRelease> =
        serde_json::from_slice(&body).map_err(|e| format!("couldn't read the release list ({e})"))?;
    *cache.lock().unwrap() = ListCache { etag, releases: releases.clone() };
    Ok(releases)
}

/// How long GitHub asks us to wait: `retry-after` seconds, else until `x-ratelimit-reset`, else an hour; at least a
/// minute, at most a day.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    let num = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok());
    let secs = num("retry-after").or_else(|| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        num("x-ratelimit-reset").map(|reset| reset.saturating_sub(now))
    });
    Duration::from_secs(secs.unwrap_or(3600).clamp(60, 86_400))
}

/// One check: is there a newer release for this platform, and is it downloaded and verified? Downloads it if not.
/// `progress(version, done, total)` follows the artifact's download.
#[allow(clippy::too_many_arguments)]
pub async fn check(
    source: &Source,
    http: &reqwest::Client,
    cache: &Mutex<ListCache>,
    key: &PublicKey,
    current: &Version,
    early: bool,
    platform: Platform,
    dir: &Path,
    progress: &(dyn Fn(&Version, u64, u64) + Send + Sync),
) -> Result<Found, CheckError> {
    let releases = list(http, source, cache).await?;
    let mut last_err = None;
    for version in candidates(&releases, current, early, platform) {
        match stage(source, http, key, &version, platform, dir, progress).await {
            Ok(staged) => {
                prune(dir, Some(&staged.version));
                return Ok(Found::Ready(staged));
            }
            Err(e) => {
                tracing::warn!("update {version}: {e}");
                last_err = Some(format!("{version}: {e}"));
            }
        }
    }
    match last_err {
        Some(e) => Err(CheckError::Failed(e)),
        None => {
            prune(dir, None);
            Ok(Found::UpToDate)
        }
    }
}

/// Download (or find already downloaded) and verify one release.
async fn stage(
    source: &Source,
    http: &reqwest::Client,
    key: &PublicKey,
    version: &Version,
    platform: Platform,
    dir: &Path,
    progress: &(dyn Fn(&Version, u64, u64) + Send + Sync),
) -> Result<Staged, String> {
    let manifest_bytes = fetch(http, source, &source.file_url(version, MANIFEST), verify::MAX_MANIFEST).await?;
    let manifest_sig =
        fetch(http, source, &source.file_url(version, &format!("{MANIFEST}{SIG_EXT}")), verify::MAX_SIGNATURE).await?;
    let manifest = verify::verify_manifest(&manifest_bytes, &manifest_sig, key, version)?;
    let name = platform.artifact(version);
    let entry = manifest.files.get(&name).ok_or_else(|| format!("the release has no {name}"))?.clone();
    let stage = dir.join(version.to_string());
    tokio::fs::create_dir_all(&stage).await.map_err(|e| format!("couldn't create {}: {e}", stage.display()))?;
    let artifact = stage.join(&name);
    let sig_path = stage.join(format!("{name}{SIG_EXT}"));
    let notes = verify::plain_notes(&manifest.notes);

    // Already here from an earlier check (or an earlier run): verify it again rather than trust the folder.
    if let Ok(sig) = tokio::fs::read(&sig_path).await
        && artifact.is_file()
        && verify_file(&artifact, &sig, key, &name, version, &manifest).await.is_ok()
    {
        write_manifest(&stage, &manifest_bytes, &manifest_sig).await?;
        return Ok(Staged { version: version.clone(), notes, artifact });
    }
    let _ = tokio::fs::remove_file(&artifact).await;

    let sig = fetch(http, source, &source.file_url(version, &format!("{name}{SIG_EXT}")), verify::MAX_SIGNATURE).await?;
    let part = stage.join(format!("{name}.part"));
    download(http, source, &source.file_url(version, &name), &part, entry.size, &|d, t| progress(version, d, t)).await?;
    if let Err(e) = verify_file(&part, &sig, key, &name, version, &manifest).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::write(&sig_path, &sig).await.map_err(|e| format!("couldn't save the signature: {e}"))?;
    write_manifest(&stage, &manifest_bytes, &manifest_sig).await?;
    tokio::fs::rename(&part, &artifact).await.map_err(|e| format!("couldn't move the update into place: {e}"))?;
    Ok(Staged { version: version.clone(), notes, artifact })
}

/// The signed manifest beside the artifact: the helper verifies the artifact against it again before applying it.
async fn write_manifest(stage: &Path, bytes: &[u8], sig: &[u8]) -> Result<(), String> {
    tokio::fs::write(stage.join(MANIFEST), bytes).await.map_err(|e| format!("couldn't save the manifest: {e}"))?;
    tokio::fs::write(stage.join(format!("{MANIFEST}{SIG_EXT}")), sig).await.map_err(|e| format!("couldn't save the manifest: {e}"))
}

/// Verify a file on disk against the manifest and its signature (off the async threads: it reads the whole file).
async fn verify_file(path: &Path, sig: &[u8], key: &PublicKey, name: &str, version: &Version, manifest: &Manifest) -> Result<(), String> {
    let entry = manifest.files.get(name).ok_or_else(|| format!("the release has no {name}"))?.clone();
    let (path, sig, key, name, version) = (path.to_owned(), sig.to_vec(), key.clone(), name.to_owned(), version.clone());
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path).map_err(|e| format!("couldn't read {name}: {e}"))?;
        verify::verify_artifact(std::io::BufReader::new(file), &sig, &key, &name, &version, &entry)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Stream `url` into `path`, never past `size` bytes.
async fn download(
    http: &reqwest::Client,
    source: &Source,
    url: &str,
    path: &Path,
    size: u64,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt as _;
    let parsed: reqwest::Url = url.parse().map_err(|_| format!("bad URL {url}"))?;
    if !(source.allow)(&parsed) {
        return Err(format!("{url} isn't a place Familiar downloads from"));
    }
    let mut resp = http.get(parsed).send().await.map_err(|e| format!("couldn't reach GitHub ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {} for {}", resp.status(), file_of(url)));
    }
    if resp.content_length().is_some_and(|n| n != size) {
        return Err(format!("{} isn't the size the signed manifest says", file_of(url)));
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|e| format!("couldn't write the download: {e}"))?;
    let mut done = 0u64;
    let mut reported = 0u64;
    progress(0, size);
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("download interrupted ({e})"))? {
        done += chunk.len() as u64;
        if done > size {
            drop(file);
            let _ = tokio::fs::remove_file(path).await;
            return Err(format!("{} is larger than the signed manifest says", file_of(url)));
        }
        file.write_all(&chunk).await.map_err(|e| format!("couldn't write the download: {e}"))?;
        if done - reported > size / 100 {
            reported = done;
            progress(done, size);
        }
    }
    file.flush().await.map_err(|e| format!("couldn't write the download: {e}"))?;
    progress(done, size);
    Ok(())
}

/// Remove staged versions other than `keep` (and stray files), keeping the apply result.
pub fn prune(dir: &Path, keep: Option<&Version>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == super::apply::RESULT || keep.is_some_and(|k| k.to_string() == name) {
            continue;
        }
        let path = entry.path();
        // Only what this module made: version folders.
        if path.is_dir() && verify::parse_version(&name).is_some() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            draft: false,
            prerelease: pre,
            assets: assets.iter().map(|a| GhAsset { name: (*a).into() }).collect(),
        }
    }

    fn full(tag: &str, pre: bool) -> GhRelease {
        let v = tag.trim_start_matches('v');
        let exe = format!("Familiar-{v}-setup.exe");
        let sig = format!("{exe}.minisig");
        rel(tag, pre, &[MANIFEST, "familiar-update.json.minisig", &exe, &sig])
    }

    fn v(s: &str) -> Version {
        verify::parse_version(s).unwrap()
    }

    #[test]
    fn candidates_are_newer_complete_releases_newest_first() {
        let releases = vec![
            full("v0.1.0-alpha.2", true),
            full("v0.1.0-alpha.10", true),
            full("v0.1.0-alpha.4", true),
            full("v0.0.9", false),
            rel("v0.1.0-alpha.11", true, &[MANIFEST]), // no installer: skipped
            GhRelease { draft: true, ..full("v0.2.0", false) },
            full("nightly", true),
            full("v0.1.0-alpha.4", true), // listed twice
        ];
        let cur = v("0.1.0-alpha.3");
        assert_eq!(candidates(&releases, &cur, true, Platform::Windows), vec![v("0.1.0-alpha.10"), v("0.1.0-alpha.4")]);
        // Early versions off: only full releases, and none is newer.
        assert!(candidates(&releases, &cur, false, Platform::Windows).is_empty());
        // No macOS archive in these releases.
        assert!(candidates(&releases, &cur, true, Platform::MacOs).is_empty());
    }

    #[test]
    fn never_a_downgrade_or_a_reinstall() {
        let releases = vec![full("v0.1.0", false), full("v0.0.5", false), full("v0.1.0-alpha.9", true)];
        assert!(candidates(&releases, &v("0.1.0"), true, Platform::Windows).is_empty());
        assert!(candidates(&releases, &v("0.2.0"), true, Platform::Windows).is_empty());
        // Stable users skip pre-releases even when GitHub doesn't flag them.
        let unflagged = vec![full("v0.2.0-beta.1", false), full("v0.1.1", false)];
        assert_eq!(candidates(&unflagged, &v("0.1.0"), false, Platform::Windows), vec![v("0.1.1")]);
    }

    #[test]
    fn rate_limits_say_when_to_come_back() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&h), Duration::from_secs(3600));
        h.insert("retry-after", "120".parse().unwrap());
        assert_eq!(retry_after(&h), Duration::from_secs(120));
        h.clear();
        h.insert("x-ratelimit-reset", "1".parse().unwrap()); // long past
        assert_eq!(retry_after(&h), Duration::from_secs(60));
    }

    #[test]
    fn the_github_client_refuses_plain_http() {
        let s = Source::github();
        assert!(s.client().is_ok());
        assert!(!(s.allow)(&"http://github.com/x".parse().unwrap()));
        assert!(s.list_url.starts_with("https://api.github.com/repos/dhruvkhator/familiar/"));
    }
}
