#!/usr/bin/env bash
# CI's macOS smoke test (there is no Mac on the team): launches the app on the runner and screenshots it.
#
#   smoke.sh <familiar-native binary> <output folder>
#
# 1. The bench window (synthetic data, no engine): Today on screen, screenshot, then the full bench's report.
# 2. A first start on the runner's clean account, the way Finder starts it: wrapped in Familiar.app and launched with
#    `open` (launchd's minimal PATH, so the login-shell PATH lookup must find the tools). The built-in database is
#    downloaded and started, ~/.familiar is created and the first-run screen shows (screenshot). A second launch must
#    exit at once (one copy), and `--quit` must stop the first gracefully (its database too).
set -uo pipefail

bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
out=$2
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
failed=0
fail() {
  echo "SMOKE FAIL: $*"
  failed=1
}

# Wait up to $2 seconds for process $1 to exit; true when it did.
wait_exit() {
  local pid=$1 secs=$2
  for _ in $(seq 1 "$secs"); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  return 1
}

shot() {
  screencapture -x "$out/$1.png" || fail "screencapture $1"
}

echo "== bench shot"
"$bin" --bench --bench-shot today --bench-hold 20 >"$out/bench-shot.log" 2>&1 &
pid=$!
sleep 14
shot bench-today
wait_exit "$pid" 60 || { fail "bench shot did not quit"; kill -9 "$pid"; }

echo "== bench"
"$bin" --bench --bench-out "$out/bench.txt" >"$out/bench.log" 2>&1 &
pid=$!
if wait_exit "$pid" 300; then
  wait "$pid" || fail "bench exited with an error"
else
  fail "bench did not finish"
  kill -9 "$pid"
fi
[ -s "$out/bench.txt" ] || fail "no bench report"

echo "== first start (Familiar.app via open)"
rm -rf "$HOME/.familiar"
# Outside the output folder: the artifact keeps the screenshots and logs, not a debug build.
appdir=$(mktemp -d)
bash "$here/bundle.sh" "$bin" 0.0.0-smoke "$appdir" >"$out/bundle.log" 2>&1 || fail "bundle.sh (see bundle.log)"
app="$appdir/Familiar.app"
codesign -dv "$app" >>"$out/bundle.log" 2>&1
exe="$app/Contents/MacOS/familiar-native"
# `open` starts it through launchd, with launchd's environment plus these (the token only lifts the GitHub API limit).
envs=(--env RUST_LOG=info)
[ -n "${GITHUB_TOKEN:-}" ] && envs+=(--env "GITHUB_TOKEN=$GITHUB_TOKEN")
open "${envs[@]}" --stdout "$out/app.log" --stderr "$out/app.log" "$app" || fail "open"
pid=""
for _ in $(seq 1 30); do
  pid=$(pgrep -f "$exe" | head -n 1)
  [ -n "$pid" ] && break
  sleep 1
done
[ -n "$pid" ] || fail "the app did not start"
# The built-in database: downloaded once, then started (up to 5 minutes on a cold runner).
for _ in $(seq 1 300); do
  [ -f "$HOME/.familiar/pgdata/postmaster.pid" ] && break
  sleep 1
done
[ -f "$HOME/.familiar/pgdata/postmaster.pid" ] || fail "the built-in database did not start"
sleep 20
shot first-start
ls -la "$HOME/.familiar" >"$out/familiar-home.txt" 2>&1
if [ -n "$pid" ]; then
  # A second copy hands over to the first and exits.
  "$exe" >"$out/second.log" 2>&1 &
  second=$!
  wait_exit "$second" 15 || { fail "a second launch kept running"; kill -9 "$second"; }
  kill -0 "$pid" 2>/dev/null || fail "the first copy died when a second launched"
  shot after-second-launch
  "$exe" --quit >"$out/quit.log" 2>&1
  wait_exit "$pid" 90 || { fail "--quit did not stop the app"; kill -9 "$pid"; }
fi
sleep 2
if pgrep -f "$HOME/.familiar/pgdata" >/dev/null; then
  fail "the built-in database kept running after quit"
  pgrep -fl "$HOME/.familiar/pgdata"
fi

echo "== macOS ends the app (a quit Apple Event, as the Dock's Quit or logging out sends)"
# AppKit exits right after applicationWillTerminate: the bounded stop must still take the database down.
open "${envs[@]}" --stdout "$out/app2.log" --stderr "$out/app2.log" "$app" || fail "open (second start)"
pid2=""
for _ in $(seq 1 30); do
  pid2=$(pgrep -f "$exe" | head -n 1)
  [ -n "$pid2" ] && break
  sleep 1
done
for _ in $(seq 1 120); do
  pgrep -f "$HOME/.familiar/pgdata" >/dev/null && break
  sleep 1
done
sleep 8
if [ -z "$pid2" ]; then
  fail "the app did not start again"
else
  osascript -e 'tell application id "dev.familiar.desktop" to quit' >"$out/osascript.log" 2>&1 &
  osa=$!
  if wait_exit "$osa" 30 && wait "$osa"; then
    wait_exit "$pid2" 40 || { fail "the app did not end on a quit Apple Event"; kill -9 "$pid2"; }
    sleep 2
    if pgrep -f "$HOME/.familiar/pgdata" >/dev/null; then
      fail "the built-in database outlived the app (quit Apple Event)"
      pgrep -fl "$HOME/.familiar/pgdata"
    else
      echo "quit Apple Event: app and database stopped"
    fi
  else
    # The runner may refuse Apple Events (Automation permission): note it and stop the app the usual way.
    kill -9 "$osa" 2>/dev/null
    echo "SMOKE NOTE: could not send a quit Apple Event (see osascript.log); skipped"
    "$exe" --quit >/dev/null 2>&1
    wait_exit "$pid2" 90 || kill -9 "$pid2"
  fi
fi
cp "$HOME/.familiar/logs/"*.log "$out/" 2>/dev/null || true
# Opened by launchd with its bare PATH: the login shell's PATH must have been taken (see login_path.rs).
grep "PATH from" "$out/app.log" || fail "the app did not read the login shell's PATH"
grep -E "claude(:| is not)" "$out/app.log" || true

if [ "$failed" -ne 0 ]; then
  echo "smoke test failed"
  exit 1
fi
echo "smoke test passed"
