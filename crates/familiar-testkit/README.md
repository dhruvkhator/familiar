# familiar-testkit

Deterministic, free integration tests of the Familiar daemon (`familiar_core::run`): a real Postgres, the real
daemon (queue, permissions, MCP server, gates, schedules, restart handling) and scripted fakes in place of the CLIs.
No Claude or Codex install, no subscription, no network.

- `fake-claude` speaks `claude -p --input-format stream-json --output-format stream-json` (system/init,
  stream_event deltas, assistant/user messages, `can_use_tool` control requests, `interrupt`, `result`). It calls
  the daemon's Familiar MCP server over real HTTP using the `--mcp-config` it was given.
- `fake-codex` speaks the `codex app-server` JSON-RPC subset `codex.rs` uses (initialize, config/read, model/list,
  thread/start|resume, turn/start|interrupt, item notifications, exec approval requests, turn/completed).
- `tests/daemon.rs` runs one daemon per test against its own fresh database (`CREATE DATABASE t_<uuid>`), its own
  temp `bots_dir`, and `FAMILIAR_HOME` pointing at a temp folder (never the real `~/.familiar`).

## Running

```sh
docker run -d --name familiar-test-pg -e POSTGRES_PASSWORD=pw -p 55450:5432 postgres:16
TEST_DATABASE_URL=postgres://postgres:pw@localhost:55450/postgres cargo test -p familiar-testkit
docker rm -f familiar-test-pg
```

If `TEST_DATABASE_URL` is unset, every daemon test prints a skip message and passes. `RUST_LOG=info` shows the daemon's logs.
Databases of failed tests are left behind for inspection (they go away with the container).

## Scenarios

A test writes `fake-claude.json` (or `fake-codex.json`) into its `bots_dir`. The daemon starts the CLI in
`<bots_dir>/<slug>`, and the fake takes the first scenario file it finds in its working directory or a parent folder.
That keeps parallel tests apart without per-test environment variables. You can also set `FAKE_CLAUDE_SCENARIO` /
`FAKE_CODEX_SCENARIO` explicitly. The log goes to `FAKE_CLAUDE_LOG` / `FAKE_CODEX_LOG`, or by default to
`<name>.log.jsonl` next to the scenario.

```json
{ "invocations": [ [ ...steps of the 1st spawn... ], [ ...2nd spawn... ] ] }
```

The n-th spawn plays `invocations[n]`, and the last entry repeats after that. `{"steps": [...]}` or a bare `[...]`
applies to every spawn. Each step is an object with one key.

**fake-claude** waits for the user message on stdin, then plays its steps:

| step | effect |
|---|---|
| `{"init": {}}` | `system/init` with the `--session-id`/`--resume` id |
| `{"delta": "txt"}` | `stream_event` `content_block_delta` / `text_delta` |
| `{"text": "txt"}`, `{"thinking": "txt"}` | an assistant message with that block |
| `{"tool": {"name", "input", "ok"?, "timeout_ms"?}}` | assistant `tool_use`, then a `can_use_tool` control_request; waits for the matching control_response, then sends a `tool_result` (allowed → `ok` text; denied → the deny message, `is_error: true`) |
| `{"mcp": {"tool", "arguments", "server"?}}` | real streamable-HTTP MCP call (initialize, initialized, tools/call) on that server (default `familiar`) from `--mcp-config`; tool_use + tool_result |
| `{"sleep_ms": n}` | sleep (an `interrupt` that arrives meanwhile is acknowledged and ends the run with an error result) |
| `{"rate_limit": {...}}` | `rate_limit_event` with that `rate_limit_info` |
| `{"stderr": "txt"}` | write a line to stderr (e.g. `No conversation found with session ID: x`) |
| `{"result": "txt"}` | `result` success (cost 0.0123), exit 0 |
| `{"error_result": "msg"}` | `result` `error_during_execution`, exit 1 |
| `{"exit": code}` | exit without a result |
| `{"raw": {...}}` | print any JSON line |

`--version` prints a version and exits (the daemon's heartbeat calls it).

**fake-codex** answers the setup requests by itself (`thread/start` → id `thr_fake_<n>`, model `gpt-fake`, plus one
owner MCP server `owner_own` in `config/read`). After `turn/start` it plays `{"text"}`, `{"reasoning"}`,
`{"exec": {"command", "output"?}}` (commandExecution item + `item/commandExecution/requestApproval`, waits for
accept/decline), `{"sleep_ms"}`, `{"complete": {}}` (token usage + `turn/completed`), `{"fail": "msg"}`, `{"exit"}`
and `{"raw"}`. `{"no_rollout": {}}` anywhere in an invocation makes `thread/resume` fail with "no rollout found".
`turn/interrupt` completes the turn as `interrupted`.

**Log entries** (JSONL, each with `n` = invocation and `pid`): `start` (argv, cwd, selected env), `stdin` (every
line received), `permission` (fake-claude: the daemon's decision), `mcp` (tool, text, is_error), `approval`
(fake-codex: accept/decline), `exit` (code, reason).

## Not covered yet

- fake-codex: `item/fileChange/requestApproval`, MCP elicitation approvals, interrupt-on-cancel, rate limits.
- Reviewer (`review` rules spawn `claude --model haiku --json-schema`), browser/live view, Telegram, artifacts.
- The 30-minute approval timeout (not configurable).
