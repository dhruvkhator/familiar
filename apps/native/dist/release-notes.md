## Install

**Windows:** download `Familiar-<version>-setup.exe`, run it (no admin rights needed) and open Familiar. The installer
isn't code-signed yet, so SmartScreen may warn: choose *More info → Run anyway*.

**macOS** (Apple Silicon and Intel, macOS 14 or later): download `Familiar-<version>.dmg`, open it and drag Familiar
into Applications. Familiar isn't notarized by Apple yet, so the first launch takes a few extra clicks:

1. Open Familiar from Applications. macOS says it can't verify the app: click **Done** (not *Move to Trash*).
2. Open **System Settings → Privacy & Security**, scroll down to *Security*, and next to "Familiar was blocked" click
   **Open Anyway**; confirm with your password or Touch ID, then **Open**. (On macOS 14, Control-click
   Familiar in Applications → **Open** → **Open** also works.)
3. If macOS instead says Familiar "is damaged and can't be opened", run this in Terminal, then open it again:
   `xattr -dr com.apple.quarantine /Applications/Familiar.app`

After that it opens normally. When the window is closed Familiar keeps working from the menu bar; quit it from there
or with ⌘Q.

**You need** (both systems): [Claude Code](https://docs.claude.com/en/docs/claude-code) installed and signed in
(`npm install -g @anthropic-ai/claude-code`, or on a Mac `brew install --cask claude-code`; then run `claude` once in
a terminal to sign in), Node.js 20 or later (connectors and browser tools), and Google Chrome or Microsoft Edge (the
browser tool). The first launch downloads Familiar's built-in database once (internet needed) and keeps all data in
`~/.familiar` (your user folder). Desktop control works on Windows only for now.

`SHA256SUMS.txt` lists every file's checksum.
