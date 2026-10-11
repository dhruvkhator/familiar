# Plan: desktop control on macOS (Peekaboo)

Desktop control (a teammate using the owner's screen, mouse and keyboard) exists on Windows through Windows-MCP. macOS
gets the same feature through [Peekaboo](https://github.com/openclaw/Peekaboo) (MIT; macOS CLI and MCP server for
screen capture, accessibility inspection and UI automation). macOS 15 or later only: the owner's Macs are current, so
no fallback for older versions. Owner: Opus 5.5. Review: Fable 5.1.

- **Install** pinned, once, into `~/.familiar/tools` like Playwright MCP and Windows-MCP (never fetched at run time);
  verify its checksum; no telemetry.
- **Same rules as Windows** (`crates/familiar-core/src/desktop.rs`): off by default per teammate; every tool call asks
  the owner (screenshots and UI reads too); never "Always allow"; one teammate at a time; "Stop desktop control" in
  the menu bar; plain-words approval cards with a picture of the target area; tools that run shell commands, AppleScript
  or edit files are excluded (allow-list the tools Familiar serves).
- **macOS permissions:** Peekaboo needs Accessibility and Screen Recording. Familiar checks both before offering the
  switch, opens the right System Settings pane, and explains what to tick. With an ad-hoc signature macOS may reset these
  grants after an update: detect it and guide the owner through re-granting (an Apple Developer ID removes this later).
- **UI:** the existing Windows desktop-control switch, Set up row and tray item, now shown on macOS too.
- **CI:** unit tests for tool classification and the permission checks on the macOS runner; a smoke check that the
  pinned Peekaboo installs and lists its tools.
