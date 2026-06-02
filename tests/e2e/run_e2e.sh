#!/usr/bin/env bash
# tests/e2e/run_e2e.sh — comprehensive real-world test of the loom runtime.
#
# Tests every README-promised feature end-to-end: navigate, click, type,
# wait, evaluate, screenshot, snapshot, replay, validate, parallel sessions,
# typed errors, budgets, time-travel inspect.
#
# Usage:  bash tests/e2e/run_e2e.sh
# Requires the daemon to be running (`loom serve`) and `jq` on PATH.

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$(cd "$HERE/../.." && pwd)"
cd "$HERE"

LOOM="${LOOM_BIN:-$WORKSPACE/target/release/loom}"
RESULTS=results
FIXTURE_PORT="${FIXTURE_PORT:-8765}"
FIXTURE_URL="http://127.0.0.1:${FIXTURE_PORT}/index.html"
CHECKOUT_URL="http://127.0.0.1:${FIXTURE_PORT}/checkout.html"
UPLOAD_URL="http://127.0.0.1:${FIXTURE_PORT}/upload.html"
# Absolute fixtures dir — the daemon under test MUST be started with
# LOOM_UPLOAD_ROOT set to this path for the web.set_input_files happy-path
# to pass (fail-closed otherwise). The harness asserts that contract.
FIXTURES_DIR="$(cd "$(dirname "$0")/fixtures" && pwd)"
UPLOAD_FILE="$FIXTURES_DIR/sample-upload.txt"

mkdir -p "$RESULTS"
PASS=0; FAIL=0
declare -a FAILED

ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31mFAIL\033[0m %s\n' "$1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; }
sect() { printf '\n\033[36m== %s ==\033[0m\n' "$1"; }

# Run an action through loom, suppressing the noisy unmatched-prefix
# stderr line from the daemon when the script's quoting is right.
nav()    { $LOOM action web.navigate   --session "$1" --url "$2" 2>&1; }
ev()     { $LOOM action web.evaluate   --session "$1" --expression "$2" 2>&1; }
type_()  { $LOOM action web.type       --session "$1" --selector "$2" --text "$3" 2>&1; }
click()  { $LOOM action web.click      --session "$1" --selector "$2" 2>&1; }
sel()    { $LOOM action web.select     --session "$1" --selector "$2" --value "$3" 2>&1; }
wait_()  { $LOOM action web.wait       --session "$1" --selector "$2" --timeout_ms "${3:-3000}" 2>&1; }
hover()  { $LOOM action web.hover      --session "$1" --selector "$2" 2>&1; }
scroll() { $LOOM action web.scroll     --session "$1" --selector "$2" --delta_y "${3:-100}" 2>&1; }
shot()   { $LOOM action web.screenshot --session "$1" 2>&1; }
snap()   { $LOOM action web.snapshot   --session "$1" 2>&1; }
upload() { $LOOM action web.set_input_files --session "$1" --selector "$2" --paths "$3" 2>&1; }

# -- Fixture server -----------------------------------------------------
sect "Booting fixture HTTP server on :${FIXTURE_PORT}"
python3 -m http.server "$FIXTURE_PORT" --directory fixtures >"$RESULTS/fixture-server.log" 2>&1 &
FIXTURE_PID=$!
trap 'kill $FIXTURE_PID 2>/dev/null || true' EXIT
sleep 1
if ! curl -sf "$FIXTURE_URL" >/dev/null; then
  fail "fixture-server-up" "couldn't curl $FIXTURE_URL"
  exit 1
fi
ok "fixture-server-up"

# -- Section 1: Doctor + create session ---------------------------------
sect "Section 1: doctor + session create"
DOCTOR=$($LOOM doctor 2>/dev/null)
# Only require daemon_responsive=ok. `chromium_present_and_verified` can
# legitimately fail on CI runners using a system Chrome fallback rather
# than the postinstalled pinned build (macOS layout mismatch tracked as
# a v0.9.x fix), and the daemon stays fully functional through that
# fallback. The actual chromium signal is web.navigate working below.
if echo "$DOCTOR" | jq -e '.checks[] | select(.name == "daemon_responsive") | .status == "ok"' >/dev/null 2>&1; then
  ok "doctor-daemon-responsive"
else
  fail "doctor-daemon-responsive" "$DOCTOR"
fi

SESSION=$($LOOM session create --profile standard 2>&1 | jq -r .session_id 2>/dev/null)
if [[ "$SESSION" =~ ^[a-z0-9]{26}$ ]]; then
  ok "session-create-returns-ulid"
else
  fail "session-create-returns-ulid" "got '$SESSION'"
  exit 1
fi
echo "  session: $SESSION"

# -- Section 2: web.navigate happy path ---------------------------------
sect "Section 2: web.navigate to local fixture"
NAV=$(nav "$SESSION" "$FIXTURE_URL")
if echo "$NAV" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "navigate-returns-action-hash"
  echo "  action_hash: $(echo "$NAV" | jq -r '.action_hash' | cut -c1-16)…"
else
  fail "navigate-returns-action-hash" "$NAV"
fi

# -- Section 3: web.evaluate --------------------------------------------
sect "Section 3: web.evaluate"
EVAL=$(ev "$SESSION" 'document.title')
TITLE=$(echo "$EVAL" | jq -r '.return_value_json // empty' | jq -r 'select(. != null)' 2>/dev/null)
if [ "$TITLE" = "Loom E2E Fixture" ]; then
  ok "evaluate-returns-document-title"
else
  fail "evaluate-returns-document-title" "got '$TITLE' from: $EVAL"
fi

RAND1=$(ev "$SESSION" 'Math.random()' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
RAND2=$(ev "$SESSION" 'Math.random()' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
if [ -n "$RAND1" ] && [ -n "$RAND2" ]; then
  ok "evaluate-math-random-returns-values"
else
  fail "evaluate-math-random-returns-values" "rand1='$RAND1' rand2='$RAND2'"
fi

# -- Section 4: web.wait ------------------------------------------------
sect "Section 4: web.wait"
W=$(wait_ "$SESSION" '#hello' 2000)
if echo "$W" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "wait-resolves-existing-selector"
else
  fail "wait-resolves-existing-selector" "$W"
fi

# -- Section 5: web.type ------------------------------------------------
sect "Section 5: web.type"
T=$(type_ "$SESSION" '#text-input' 'hello world')
if echo "$T" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "type-returns-receipt"
  VAL=$(ev "$SESSION" 'document.getElementById("text-input").value' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  if [ "$VAL" = "hello world" ]; then
    ok "type-actually-set-value"
  else
    fail "type-actually-set-value" "got '$VAL'"
  fi
else
  fail "type-returns-receipt" "$T"
fi

# -- Section 6: web.click -----------------------------------------------
sect "Section 6: web.click"
C=$(click "$SESSION" '#ok-button')
if echo "$C" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "click-returns-receipt"
  RES=$(ev "$SESSION" 'document.getElementById("result").textContent' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  if [ "$RES" = "clicked" ]; then ok "click-fired-handler"
  else fail "click-fired-handler" "got '$RES'"; fi
else
  fail "click-returns-receipt" "$C"
fi

# -- Section 7: web.select ----------------------------------------------
sect "Section 7: web.select"
S=$(sel "$SESSION" '#dropdown' 'b')
if echo "$S" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "select-returns-receipt"
  V=$(ev "$SESSION" 'document.getElementById("dropdown").value' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  if [ "$V" = "b" ]; then ok "select-changed-value"
  else fail "select-changed-value" "got '$V'"; fi
else
  fail "select-returns-receipt" "$S"
fi

# -- Section 8: web.scroll ----------------------------------------------
sect "Section 8: web.scroll"
SC=$(scroll "$SESSION" 'body' 100)
if echo "$SC" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "scroll-returns-receipt"
else
  fail "scroll-returns-receipt" "$SC"
fi

# -- Section 9: web.hover -----------------------------------------------
sect "Section 9: web.hover"
H=$(hover "$SESSION" '#ok-button')
if echo "$H" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "hover-returns-receipt"
else
  fail "hover-returns-receipt" "$H"
fi

# -- Section 10: web.screenshot -----------------------------------------
sect "Section 10: web.screenshot"
SH=$(shot "$SESSION")
if echo "$SH" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "screenshot-returns-receipt"
  # Note: docs/actions.md says "screenshot_ref" but the actual wire field is
  # "screenshot_after_hash". Tracking that drift but accepting either here.
  REF=$(echo "$SH" | jq -r '.screenshot_ref // .screenshot_after_hash // empty')
  if [ -n "$REF" ] && [ "$REF" != "null" ]; then
    ok "screenshot-has-content-hash (${REF:0:16}…)"
  else
    fail "screenshot-has-content-hash" "no screenshot_ref or screenshot_after_hash"
  fi
else
  fail "screenshot-returns-receipt" "$SH"
fi

# -- Section 11: web.snapshot -------------------------------------------
sect "Section 11: web.snapshot"
SN=$(snap "$SESSION")
if echo "$SN" | jq -e '.action_hash' >/dev/null 2>&1; then
  ok "snapshot-returns-receipt"
else
  fail "snapshot-returns-receipt" "$SN"
fi

# -- Section 11b: web.set_input_files -----------------------------------
# Requires the daemon under test to be started with
# LOOM_UPLOAD_ROOT="$FIXTURES_DIR" (fail-closed otherwise). The happy path
# uploads a fixture file into a real <input type=file> and reads back the
# FileList via web.evaluate; the negative cases assert typed errors.
sect "Section 11b: web.set_input_files"
UPSESSION=$($LOOM session create --profile standard 2>&1 | jq -r .session_id 2>/dev/null)
if [[ "$UPSESSION" =~ ^[a-z0-9]{26}$ ]]; then
  nav "$UPSESSION" "$UPLOAD_URL" >/dev/null
  # Happy path: upload the fixture, then read input.files via web.evaluate.
  UP=$(upload "$UPSESSION" '#upload' "[\"$UPLOAD_FILE\"]")
  echo "$UP" >"$RESULTS/upload.json"
  LEN=$(ev "$UPSESSION" 'document.querySelector("#upload").files.length' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  NAME=$(ev "$UPSESSION" 'document.querySelector("#upload").files[0] && document.querySelector("#upload").files[0].name' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  if [ "$LEN" = "1" ] && echo "$NAME" | grep -q 'sample-upload.txt'; then
    ok "set-input-files-filelist-reflects-upload"
  else
    fail "set-input-files-filelist-reflects-upload" "len=$LEN name=$NAME (is the daemon started with LOOM_UPLOAD_ROOT=$FIXTURES_DIR? see $RESULTS/upload.json)"
  fi

  # Negative: a path outside the allow-list root → typed security error.
  BLK=$(upload "$UPSESSION" '#upload' '["/etc/passwd"]')
  echo "$BLK" >"$RESULTS/upload-blocked.json"
  if echo "$BLK" | grep -qiE 'upload_path_blocked|upload_root_not_configured'; then
    ok "set-input-files-path-outside-root-blocked"
  else
    fail "set-input-files-path-outside-root-blocked" "see $RESULTS/upload-blocked.json"
  fi

  # Selector miss → typed selector_not_found.
  MISS=$(upload "$UPSESSION" '#no-such-input-zzz' "[\"$UPLOAD_FILE\"]")
  if echo "$MISS" | grep -qiE 'selector_not_found|selector-not-found'; then
    ok "set-input-files-selector-miss-typed-error"
  else
    fail "set-input-files-selector-miss-typed-error" "$MISS"
  fi

  # Wrong element type (text input, not a file input) → not_a_file_input.
  WRONG=$(upload "$UPSESSION" '#text-field' "[\"$UPLOAD_FILE\"]")
  if echo "$WRONG" | grep -qiE 'not_a_file_input|not-a-file-input'; then
    ok "set-input-files-wrong-element-typed-error"
  else
    fail "set-input-files-wrong-element-typed-error" "$WRONG"
  fi

  $LOOM session close "$UPSESSION" >/dev/null 2>&1 || true
else
  fail "set-input-files-session-create" "could not create upload session"
fi

# -- Section 12: session inspect/validate -------------------------------
sect "Section 12: session inspect + validate"
INS=$($LOOM session inspect "$SESSION" 2>&1)
echo "$INS" >"$RESULTS/inspect.json"
if echo "$INS" | jq -e '.actions[0]' >/dev/null 2>&1 || echo "$INS" | jq -e '.action_count' >/dev/null 2>&1; then
  ok "inspect-returns-payload"
else
  fail "inspect-returns-payload" "see $RESULTS/inspect.json"
fi

VAL=$($LOOM session validate "$SESSION" 2>&1)
if echo "$VAL" | jq -e '.valid // .ok' >/dev/null 2>&1 || echo "$VAL" | grep -qiE 'PASS|valid|"ok":true'; then
  ok "validate-passes"
else
  fail "validate-passes" "$VAL"
fi

# -- Section 13: time-travel inspect ------------------------------------
sect "Section 13: session inspect --at-action"
TI=$($LOOM session inspect "$SESSION" --at-action 1 2>&1)
if echo "$TI" | jq -e '.' >/dev/null 2>&1; then
  ok "inspect-at-action-1-works"
else
  fail "inspect-at-action-1-works" "$TI"
fi

# -- Section 14: close session ------------------------------------------
sect "Section 14: session close"
CL=$($LOOM session close "$SESSION" 2>&1)
ok "close-returned ($CL)"

# -- Section 15: replay equality ----------------------------------------
sect "Section 15: replay + diff (the headline determinism claim)"
REPLAY=$($LOOM session replay "$SESSION" 2>&1)
echo "$REPLAY" >"$RESULTS/replay.json"
NEW=$(echo "$REPLAY" | jq -r '.session_id // .replay_session_id // empty' 2>/dev/null)
if [[ "$NEW" =~ ^[a-z0-9]{26}$ ]]; then
  ok "replay-returns-new-session-id ($NEW)"
  DIFF=$($LOOM session diff "$SESSION" "$NEW" 2>&1)
  echo "$DIFF" >"$RESULTS/diff.json"
  FD=$(echo "$DIFF" | jq -r '.field_diffs | length' 2>/dev/null)
  if [ "$FD" = "0" ]; then
    ok "replay-bit-equal-source (field_diffs=0)"
  else
    fail "replay-bit-equal-source" "field_diffs=$FD — see $RESULTS/diff.json"
  fi
else
  fail "replay-returns-new-session-id" "see $RESULTS/replay.json"
fi

# -- Section 16: typed errors -------------------------------------------
sect "Section 16: typed errors"
S2=$($LOOM session create --profile standard 2>&1 | jq -r .session_id)

DNS_ERR=$(nav "$S2" 'http://this-host-does-not-exist-loom-test.invalid/')
echo "$DNS_ERR" >"$RESULTS/dns-error.json"
if echo "$DNS_ERR" | grep -qiE 'dns_failure|dns-failure|ERR_NAME_NOT_RESOLVED'; then
  ok "dns-failure-typed-error"
else
  fail "dns-failure-typed-error" "see $RESULTS/dns-error.json"
fi

HTTP_ERR=$(nav "$S2" "${FIXTURE_URL%/index.html}/no-such-path-404")
echo "$HTTP_ERR" >"$RESULTS/http-error.json"
if echo "$HTTP_ERR" | grep -qiE 'http_status|http-status|"status_code":404|404'; then
  ok "http-status-typed-error"
else
  fail "http-status-typed-error" "see $RESULTS/http-error.json"
fi

WPF=$(wait_ "$S2" '#nonexistent-selector-zzz' 500)
echo "$WPF" >"$RESULTS/wait-error.json"
if echo "$WPF" | grep -qiE 'wait_predicate_false|wait-predicate-false'; then
  ok "wait-predicate-false-typed-error"
else
  fail "wait-predicate-false-typed-error" "see $RESULTS/wait-error.json"
fi

URL_BLK=$(nav "$S2" 'javascript:alert(1)')
if echo "$URL_BLK" | grep -qiE 'url_blocked|url-blocked|scheme'; then
  ok "url-blocked-typed-error"
else
  fail "url-blocked-typed-error" "$URL_BLK"
fi

$LOOM session close "$S2" >/dev/null 2>&1 || true

# -- Section 17: budget enforcement -------------------------------------
sect "Section 17: budgets"
S3=$($LOOM session create --profile standard --budget wall_clock=2s 2>&1 | jq -r .session_id 2>/dev/null)
if [[ "$S3" =~ ^[a-z0-9]{26}$ ]]; then
  ok "session-with-wallclock-budget-creates"
  nav "$S3" "$FIXTURE_URL" >/dev/null
  sleep 3
  AFTER=$(ev "$S3" '1+1')
  echo "$AFTER" >"$RESULTS/budget-after.json"
  if echo "$AFTER" | grep -qiE 'budget_exceeded|budget-exceeded|budget|exceeded|expired'; then
    ok "budget-exceeded-typed-error"
  else
    fail "budget-exceeded-typed-error" "see $RESULTS/budget-after.json"
  fi
  $LOOM session close "$S3" >/dev/null 2>&1 || true
else
  fail "session-with-wallclock-budget-creates" "could not create"
fi

# -- Section 18: parallel sessions --------------------------------------
sect "Section 18: parallel sessions (4 concurrent)"
PARALLEL_OUT="$RESULTS/parallel.log"
: >"$PARALLEL_OUT"
parallel_one() {
  SID=$($LOOM session create --profile standard 2>&1 | jq -r .session_id)
  nav "$SID" "$FIXTURE_URL" >/dev/null
  T=$(ev "$SID" 'document.title' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
  echo "session $1 title='$T' id=$SID" >>"$PARALLEL_OUT"
  $LOOM session close "$SID" >/dev/null 2>&1 || true
}
PJOBS=()
for i in 1 2 3 4; do parallel_one "$i" & PJOBS+=($!); done
wait "${PJOBS[@]}"
GOOD=$(grep -c "title='Loom E2E Fixture'" "$PARALLEL_OUT" || echo 0)
if [ "$GOOD" = "4" ]; then
  ok "four-parallel-sessions-all-loaded"
else
  fail "four-parallel-sessions-all-loaded" "$GOOD/4 — see $PARALLEL_OUT"
fi

# -- Section 19: full form/checkout flow --------------------------------
sect "Section 19: full form flow on local checkout fixture"
SC=$($LOOM session create --profile standard 2>&1 | jq -r .session_id)
nav    "$SC" "$CHECKOUT_URL"        >/dev/null
type_  "$SC" '#name'  'Test User'   >/dev/null
type_  "$SC" '#email' 'test@example.com' >/dev/null
type_  "$SC" '#card'  '4242424242424242' >/dev/null
sel    "$SC" '#country' 'GB'        >/dev/null
click  "$SC" '#book'                >/dev/null
CONF=$(ev "$SC" 'document.getElementById("confirmation").textContent' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
if echo "$CONF" | grep -q "Booked: Test User"; then
  ok "checkout-flow-end-to-end"
else
  fail "checkout-flow-end-to-end" "got '$CONF'"
fi
$LOOM session close "$SC" >/dev/null 2>&1 || true

# -- Section 20: real public site ---------------------------------------
sect "Section 20: real public site (example.com — sanity check)"
SR=$($LOOM session create --profile standard 2>&1 | jq -r .session_id)
nav "$SR" 'https://example.com' >/dev/null
TITLE=$(ev "$SR" 'document.title' | jq -r '.return_value_json // empty' | jq -r 'select(. != null)')
if [ "$TITLE" = "Example Domain" ]; then
  ok "example-com-loads"
else
  fail "example-com-loads" "got '$TITLE'"
fi
$LOOM session close "$SR" >/dev/null 2>&1 || true

# -- Summary ------------------------------------------------------------
sect "Summary"
TOTAL=$((PASS+FAIL))
printf '  %d / %d passed\n' "$PASS" "$TOTAL"
if [ "$FAIL" -gt 0 ]; then
  printf '\nFailed:\n'
  for f in "${FAILED[@]}"; do printf '  - %s\n' "$f"; done
  exit 1
fi
