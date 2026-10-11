# Plan: the macOS app, and updates from inside the app

Two workstreams, run in parallel by two Opus agents; Fable reviews both. The desktop app (`apps/native`) is the product.

## A. In-app updates (Windows first, macOS through the same path)

The owner never downloads from GitHub again: Familiar finds, verifies, installs and restarts into new versions.

- **Version baked in.** Today every build says 0.1.0. The release workflow passes the tag's version to the build
  (`FAMILIAR_VERSION`, read with `option_env!`, falling back to `CARGO_PKG_VERSION`), shown in Settings.
- **Where updates come from.** GitHub Releases of `dhruvkhator/familiar` (public API, no token): checked at start and
  every 6 hours. While Familiar is in alpha, pre-releases are included ("Early versions", on by default; a setting).
- **Trust: signatures, not just checksums.** A checksum file published next to the installer proves nothing if the
  release itself is tampered with. Every artifact gets an Ed25519 signature (minisign-compatible) made in CI with a
  private key kept in the repo's Actions secrets; the public key is compiled into the app. The app refuses anything
  whose signature doesn't verify, and never downgrades. (The key pair is generated once; the private key is stored
  as a GitHub secret and a backup the owner keeps offline.)
- **Experience.** Settings → Updates: current version, "Check now", what's new (release notes as plain text), and
  "Version X is ready · Restart to update". A quiet badge on Settings in the sidebar when one is ready. Downloading
  happens in the background; restarting is always the owner's click, and is held back (with a note) while a teammate
  is running or an approval is waiting.
- **Applying on Windows.** The app quits gracefully (the existing `--quit` drain), a small detached helper waits for
  it to exit, runs the verified installer silently (`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`, same AppId = in-place
  upgrade, per-user, no admin), then starts the new version. If the installer fails, the old version starts again and
  says why.
- **Applying on macOS.** The update is a signed `Familiar.app` archive; the app is replaced atomically (staged next to
  the old one, then swapped) and relaunched. No quarantine flag is added, so no Gatekeeper prompt on update.
- **Never breaks data.** Migrations already only move forward; an app older than the data refuses to start with a
  clear "a newer Familiar ran on this data: update" message (today it only says it can't reach the engine).

## B. The macOS app

GPUI runs on macOS (the zui fork ships `gpui_macos`). Most of the app is portable; the Windows-only parts:

| Area | Windows today | macOS |
| --- | --- | --- |
| Title bar | Custom drawn (`titlebar.rs`) | System traffic lights (custom later) |
| One copy at a time, `--quit` | Named mutex + events (`desktop.rs`) | Lock file + local socket, or GPUI's reopen handling |
| Tray | `tray-icon` | `tray-icon` menu bar item (same crate) |
| Notifications | Windows toasts (`notify.rs`) | macOS notifications |
| Desktop control | Windows-MCP | Not offered on macOS (hidden; a macOS server can come later) |
| Folders | Windows protected paths | macOS ones too (`~/Library`, `/System`, `/Applications`, keychains, `~/.ssh`...) |
| Finding `claude` / `codex` / Node | Windows PATH | GUI apps on macOS don't inherit the shell's PATH: resolve from the login shell and the usual install places |
| Built-in Postgres, Chrome/Edge | Supported | Supported (`postgresql_embedded`, existing macOS Chrome paths) |
| Packaging | Inno Setup installer | `Familiar.app` (Info.plist, `.icns`), a `.dmg` for first install, the signed archive for updates; Apple Silicon and Intel (universal binary) |

- **Signing.** Without an Apple Developer account the app is ad-hoc signed and not notarized: the first launch needs
  right-click → Open. Notarization is a later step once there is an account.
- **CI.** A macOS job builds and tests the native app on every push; the release workflow builds the `.dmg` and the
  update archive next to the Windows installer.
- **Testing.** There is no Mac here: CI compiles and runs the tests on macOS runners, and a launch smoke test
  (`--bench`, screenshot) runs there. Real-Mac testing by the owner when available.
