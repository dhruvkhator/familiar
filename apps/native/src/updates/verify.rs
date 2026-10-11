//! What a release has to prove before Familiar acts on it. Pure checks, no I/O beyond the reader they are handed.
//!
//! - **Versions** are semver with pre-releases (`0.1.0-alpha.3 < 0.1.0-alpha.10 < 0.1.0`); build metadata is refused
//!   (it doesn't order). An update must be strictly newer than the running version: never a downgrade or a reinstall.
//! - **The manifest** (`familiar-update.json`) lists the release's version, its notes and each file's size and SHA-256.
//!   It is signed with the release key compiled into the app ([`RELEASE_KEY`]); its signed (trusted) comment names the
//!   file and the version, and its version must be the release tag's.
//! - **Each artifact** has its own minisign signature, whose trusted comment names that file and version, and must
//!   match the signed manifest's size and SHA-256 too. The checks stream (nothing is held in memory), so they run over
//!   the downloaded bytes before anything is moved to where it could run.
//! - **Hosts**: requests go over https to GitHub's own hosts only, redirects included ([`allowed_url`]).

use std::collections::BTreeMap;
use std::io::Read;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The release manifest's file name (beside the artifacts in every release).
pub const MANIFEST: &str = "familiar-update.json";
/// A signature's file name: the signed file's plus this.
pub const SIG_EXT: &str = ".minisig";
pub const MAX_MANIFEST: usize = 256 * 1024;
pub const MAX_SIGNATURE: usize = 4 * 1024;
/// No artifact is bigger (the installer is ~15 MB, the macOS archive a few times that).
pub const MAX_ARTIFACT: u64 = 512 * 1024 * 1024;
/// Release notes past this are cut (they are shown as plain text in Settings).
const MAX_NOTES: usize = 6_000;

/// The public half of Familiar's release key (minisign format). Its secret half signs releases in CI.
pub const RELEASE_KEY: &str = include_str!("../../dist/familiar-update.pub");

/// Where requests may go. `github.com` serves release files by redirecting to one of GitHub's asset hosts.
pub const ALLOWED_HOSTS: &[&str] =
    &["github.com", "api.github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];

/// The compiled-in release key.
pub fn release_key() -> PublicKey {
    public_key(RELEASE_KEY).expect("the compiled-in release key is a minisign public key")
}

/// A minisign public key file (`untrusted comment: …` then the key).
pub fn public_key(text: &str) -> Result<PublicKey, String> {
    PublicKey::decode(text.trim()).map_err(|e| format!("bad public key: {e}"))
}

/// A release version: semver, no build metadata.
pub fn parse_version(s: &str) -> Option<Version> {
    Version::parse(s).ok().filter(|v| v.build.is_empty())
}

/// The version a release tag names: exactly `v<version>`.
pub fn tag_version(tag: &str) -> Option<Version> {
    let v = parse_version(tag.strip_prefix('v')?)?;
    (format!("v{v}") == tag).then_some(v)
}

/// The platforms Familiar updates itself on, and the file each installs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// The Inno Setup installer, run silently (an in-place upgrade).
    Windows,
    /// `Familiar.app` in a gzipped tar, swapped in for the running bundle.
    MacOs,
}

impl Platform {
    pub fn current() -> Option<Platform> {
        if cfg!(windows) {
            Some(Platform::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Platform::MacOs)
        } else {
            None
        }
    }

    pub fn artifact(self, version: &Version) -> String {
        match self {
            Platform::Windows => format!("Familiar-{version}-setup.exe"),
            Platform::MacOs => format!("Familiar-{version}-macos.tar.gz"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub files: BTreeMap<String, FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileEntry {
    pub size: u64,
    pub sha256: String,
}

/// A file name Familiar will write: letters, digits, `.`, `_`, `-`; no leading dot; short.
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The `key:value` fields of a trusted comment (`timestamp:…\tfile:…\tversion:…`).
fn trusted_field<'a>(sig: &'a Signature, key: &str) -> Option<&'a str> {
    sig.trusted_comment().split('\t').filter_map(|f| f.split_once(':')).find(|(k, _)| *k == key).map(|(_, v)| v)
}

fn signature(text: &[u8]) -> Result<Signature, String> {
    if text.len() > MAX_SIGNATURE {
        return Err("the signature is too large".into());
    }
    let text = std::str::from_utf8(text).map_err(|_| "the signature isn't text".to_owned())?;
    Signature::decode(text).map_err(|e| format!("the signature is malformed ({e})"))
}

/// The signature must be for `file` of `version`: a valid signature over another file (or another release's copy of
/// it) doesn't count.
fn check_trusted(sig: &Signature, file: &str, version: &Version) -> Result<(), String> {
    if trusted_field(sig, "file") != Some(file) || trusted_field(sig, "version") != Some(version.to_string().as_str()) {
        return Err(format!("the signature is for another file than {file} {version}"));
    }
    Ok(())
}

/// Check a release's manifest: signed by `key`, for `version` (the release tag's), well formed.
pub fn verify_manifest(bytes: &[u8], sig: &[u8], key: &PublicKey, version: &Version) -> Result<Manifest, String> {
    if bytes.len() > MAX_MANIFEST {
        return Err("the update manifest is too large".into());
    }
    let sig = signature(sig)?;
    key.verify(bytes, &sig, false).map_err(|_| "the update manifest isn't signed by Familiar's release key".to_owned())?;
    check_trusted(&sig, MANIFEST, version)?;
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("the update manifest is malformed ({e})"))?;
    if m.schema != 1 {
        return Err(format!("the update manifest's format ({}) is newer than this Familiar understands", m.schema));
    }
    if parse_version(&m.version).as_ref() != Some(version) {
        return Err(format!("the update manifest is for {} instead of {version}", m.version));
    }
    for (name, f) in &m.files {
        if !safe_file_name(name) {
            return Err(format!("the update manifest names an unsafe file ({name:?})"));
        }
        if f.size == 0 || f.size > MAX_ARTIFACT {
            return Err(format!("{name} has an implausible size"));
        }
        if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(format!("{name} has a malformed checksum"));
        }
    }
    Ok(m)
}

/// Stream `reader` (artifact `name` of `version`) through its size, SHA-256 and signature checks against the
/// manifest's `entry`. Reads at most one byte past the expected size.
pub fn verify_artifact(
    reader: impl Read,
    sig: &[u8],
    key: &PublicKey,
    name: &str,
    version: &Version,
    entry: &FileEntry,
) -> Result<(), String> {
    let sig = signature(sig)?;
    check_trusted(&sig, name, version)?;
    let mut stream = key.verify_stream(&sig).map_err(|_| format!("{name} isn't signed by Familiar's release key"))?;
    let mut sha = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    let mut reader = reader.take(entry.size + 1);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("reading {name}: {e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        sha.update(&buf[..n]);
        stream.update(&buf[..n]);
    }
    if total != entry.size {
        return Err(format!("{name} is {total} bytes instead of {}", entry.size));
    }
    if hex(&sha.finalize()) != entry.sha256 {
        return Err(format!("{name} doesn't match its checksum"));
    }
    stream.finalize().map_err(|_| format!("{name} isn't signed by Familiar's release key"))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Is this a URL Familiar may fetch update data from (https, GitHub's hosts, no credentials or odd port)?
pub fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|h| ALLOWED_HOSTS.contains(&h))
}

/// Release notes as plain text: no hidden characters (bidi overrides, zero-width…), Windows line ends folded, blank
/// runs squeezed, cut to a sane length.
pub fn plain_notes(notes: &str) -> String {
    let text = crate::approval::strip_hidden(&notes.replace("\r\n", "\n").replace('\t', "    "), true);
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        blank = if line.is_empty() { blank + 1 } else { 0 };
        if blank < 2 {
            out.push_str(line);
            out.push('\n');
        }
    }
    let out = out.trim();
    if out.chars().count() > MAX_NOTES {
        let cut: String = out.chars().take(MAX_NOTES).collect();
        return format!("{}…", cut.trim_end());
    }
    out.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = include_str!("fixtures/test.pub");
    const OTHER_KEY: &str = include_str!("fixtures/other.pub");
    const GOOD_MANIFEST: &[u8] = include_bytes!("fixtures/good/familiar-update.json");
    const GOOD_MANIFEST_SIG: &[u8] = include_bytes!("fixtures/good/familiar-update.json.minisig");
    const GOOD_EXE: &[u8] = include_bytes!("fixtures/good/Familiar-9.9.9-setup.exe");
    const GOOD_EXE_SIG: &[u8] = include_bytes!("fixtures/good/Familiar-9.9.9-setup.exe.minisig");
    const GOOD_TAR_SIG: &[u8] = include_bytes!("fixtures/good/Familiar-9.9.9-macos.tar.gz.minisig");
    const OTHER_MANIFEST: &[u8] = include_bytes!("fixtures/otherkey/familiar-update.json");
    const OTHER_MANIFEST_SIG: &[u8] = include_bytes!("fixtures/otherkey/familiar-update.json.minisig");
    const OLD_MANIFEST: &[u8] = include_bytes!("fixtures/old/familiar-update.json");
    const OLD_MANIFEST_SIG: &[u8] = include_bytes!("fixtures/old/familiar-update.json.minisig");

    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }

    fn test_key() -> PublicKey {
        public_key(TEST_KEY).unwrap()
    }

    fn good() -> Manifest {
        verify_manifest(GOOD_MANIFEST, GOOD_MANIFEST_SIG, &test_key(), &v("9.9.9")).unwrap()
    }

    #[test]
    fn versions_order_like_semver_with_pre_releases() {
        let order = ["0.1.0-alpha.2", "0.1.0-alpha.3", "0.1.0-alpha.10", "0.1.0-beta.1", "0.1.0", "0.1.1", "0.2.0", "1.0.0"];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(parse_version("0.1.0+build.5").is_none(), "build metadata doesn't order");
        assert!(parse_version("0.1").is_none());
        assert!(parse_version("v0.1.0").is_none());
        assert_eq!(tag_version("v0.1.0-alpha.3"), Some(v("0.1.0-alpha.3")));
        for bad in ["0.1.0", "v0.1", "v01.1.0", "release-0.1.0", "v0.1.0 ", "v0.1.0+x"] {
            assert_eq!(tag_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_compiled_in_release_key_parses_and_verifies_a_release_signed_in_its_name() {
        let key = release_key();
        let entry = FileEntry {
            size: 2,
            sha256: hex(&Sha256::digest(include_bytes!("fixtures/prodkey/Familiar-0.0.0-setup.exe"))),
        };
        verify_artifact(
            &include_bytes!("fixtures/prodkey/Familiar-0.0.0-setup.exe")[..],
            include_bytes!("fixtures/prodkey/Familiar-0.0.0-setup.exe.minisig"),
            &key,
            "Familiar-0.0.0-setup.exe",
            &v("0.0.0"),
            &entry,
        )
        .unwrap();
        // ... and not one signed by any other key.
        let m = good();
        let e = &m.files["Familiar-9.9.9-setup.exe"];
        assert!(verify_artifact(GOOD_EXE, GOOD_EXE_SIG, &key, "Familiar-9.9.9-setup.exe", &v("9.9.9"), e).is_err());
    }

    #[test]
    fn a_signed_manifest_parses() {
        let m = good();
        assert_eq!(m.version, "9.9.9");
        assert!(m.notes.starts_with("Faster chats."));
        assert_eq!(m.files.len(), 2);
        let exe = &m.files[&Platform::Windows.artifact(&v("9.9.9"))];
        assert_eq!(exe.size, GOOD_EXE.len() as u64);
        assert!(m.files.contains_key(&Platform::MacOs.artifact(&v("9.9.9"))));
    }

    #[test]
    fn manifests_are_refused_when_tampered_signed_by_another_key_or_for_another_version() {
        let key = test_key();
        // One flipped byte in the body.
        let mut tampered = GOOD_MANIFEST.to_vec();
        let at = tampered.iter().position(|b| *b == b'9').unwrap();
        tampered[at] = b'8';
        assert!(verify_manifest(&tampered, GOOD_MANIFEST_SIG, &key, &v("9.9.9")).unwrap_err().contains("isn't signed"));
        // Signed with a key the app doesn't trust.
        assert!(verify_manifest(OTHER_MANIFEST, OTHER_MANIFEST_SIG, &key, &v("9.9.9")).is_err());
        // ...and that key verifies its own (the fixture is sound).
        verify_manifest(OTHER_MANIFEST, OTHER_MANIFEST_SIG, &public_key(OTHER_KEY).unwrap(), &v("9.9.9")).unwrap();
        // A genuine manifest served under another release's tag (replaying 0.0.1 as 9.9.9, or 9.9.9 as 10.0.0).
        assert!(verify_manifest(OLD_MANIFEST, OLD_MANIFEST_SIG, &key, &v("9.9.9")).unwrap_err().contains("another file"));
        assert!(verify_manifest(GOOD_MANIFEST, GOOD_MANIFEST_SIG, &key, &v("10.0.0")).is_err());
        // The manifest's signature on another file (the installer's), and garbage signatures.
        assert!(verify_manifest(GOOD_MANIFEST, GOOD_EXE_SIG, &key, &v("9.9.9")).is_err());
        assert!(verify_manifest(GOOD_MANIFEST, b"", &key, &v("9.9.9")).is_err());
        assert!(verify_manifest(GOOD_MANIFEST, &[b'a'; MAX_SIGNATURE + 1], &key, &v("9.9.9")).is_err());
        // A tampered trusted comment.
        let sig = String::from_utf8(GOOD_MANIFEST_SIG.to_vec()).unwrap().replace("version:9.9.9", "version:9.9.8");
        assert!(verify_manifest(GOOD_MANIFEST, sig.as_bytes(), &key, &v("9.9.8")).is_err());
    }

    #[test]
    fn artifacts_verify_and_are_refused_when_tampered_truncated_swapped_or_wrong_key() {
        let key = test_key();
        let m = good();
        let name = "Familiar-9.9.9-setup.exe";
        let entry = &m.files[name];
        let ver = v("9.9.9");
        verify_artifact(GOOD_EXE, GOOD_EXE_SIG, &key, name, &ver, entry).unwrap();
        let mut tampered = GOOD_EXE.to_vec();
        tampered[0] ^= 1;
        assert!(verify_artifact(&tampered[..], GOOD_EXE_SIG, &key, name, &ver, entry).unwrap_err().contains("checksum"));
        // Same checksum claimed for tampered bytes: the signature still refuses it.
        let lying = FileEntry { size: entry.size, sha256: hex(&Sha256::digest(&tampered)) };
        assert!(verify_artifact(&tampered[..], GOOD_EXE_SIG, &key, name, &ver, &lying).unwrap_err().contains("signed"));
        assert!(verify_artifact(&GOOD_EXE[..5], GOOD_EXE_SIG, &key, name, &ver, entry).unwrap_err().contains("bytes"));
        let mut longer = GOOD_EXE.to_vec();
        longer.extend_from_slice(b"more");
        assert!(verify_artifact(&longer[..], GOOD_EXE_SIG, &key, name, &ver, entry).is_err());
        // The macOS archive's (valid) signature isn't the installer's.
        assert!(verify_artifact(GOOD_EXE, GOOD_TAR_SIG, &key, name, &ver, entry).unwrap_err().contains("another file"));
        assert!(verify_artifact(GOOD_EXE, GOOD_EXE_SIG, &public_key(OTHER_KEY).unwrap(), name, &ver, entry).is_err());
        let other_sig = include_bytes!("fixtures/otherkey/Familiar-9.9.9-setup.exe.minisig");
        assert!(verify_artifact(GOOD_EXE, other_sig, &key, name, &ver, entry).is_err());
    }

    #[test]
    fn only_https_github_hosts_are_allowed() {
        let ok = [
            "https://api.github.com/repos/dhruvkhator/familiar/releases",
            "https://github.com/dhruvkhator/familiar/releases/download/v1.0.0/x",
            "https://objects.githubusercontent.com/github-production-release-asset/1",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1",
            "https://github.com:443/x",
        ];
        for u in ok {
            assert!(allowed_url(&u.parse().unwrap()), "{u}");
        }
        let bad = [
            "http://github.com/x",
            "https://github.com.evil.example/x",
            "https://evil.example/github.com",
            "https://user:pw@github.com/x",
            "https://github.com:8443/x",
            "https://raw.githubusercontent.com/x",
            "file:///C:/x",
            "https://127.0.0.1/x",
        ];
        for u in bad {
            assert!(!allowed_url(&u.parse().unwrap()), "{u}");
        }
    }

    #[test]
    fn unsafe_file_names_are_refused() {
        assert!(safe_file_name("Familiar-0.1.0-alpha.3-setup.exe"));
        for bad in ["", "../x.exe", "a/b", "a\\b", ".hidden", "x y", "C:x", &"a".repeat(129), "é.exe"] {
            assert!(!safe_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn notes_are_plain_text() {
        let notes = "Fix\u{202E}exe.txt\r\n\r\n\r\n\r\n* zero\u{200B}width\tand tab\u{0007}";
        assert_eq!(plain_notes(notes), "Fixexe.txt\n\n* zerowidth    and tab");
        let long = "x".repeat(MAX_NOTES + 50);
        assert_eq!(plain_notes(&long).chars().count(), MAX_NOTES + 1);
    }
}
