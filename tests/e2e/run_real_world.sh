#!/usr/bin/env bash
# tests/e2e/run_real_world.sh — exercise loom against real public sites:
# (a) Saucedemo.com — automation-friendly login + checkout flow
# (b) Booking.com — search-only, validates anti-bot behaviour
# (c) The internet — login form, captcha-free, well-known fixture
#
# We avoid actual purchases. Network failures are flagged but not fatal —
# external sites are flaky by design.

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$(cd "$HERE/../.." && pwd)"
cd "$HERE"

LOOM="${LOOM_BIN:-$WORKSPACE/target/release/loom}"
RESULTS=results
mkdir -p "$RESULTS"

PASS=0; FAIL=0; SKIP=0
declare -a FAILED
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31mFAIL\033[0m %s\n' "$1"; [ -n "${2:-}" ] && printf '       %s\n' "$2"; }
skip() { SKIP=$((SKIP+1)); printf '  \033[33mSKIP\033[0m %s — %s\n' "$1" "$2"; }
sect() { printf '\n\033[36m== %s ==\033[0m\n' "$1"; }

nav()   { $LOOM action web.navigate   --session "$1" --url "$2" 2>&1; }
ev()    { $LOOM action web.evaluate   --session "$1" --expression "$2" 2>&1; }
type_() { $LOOM action web.type       --session "$1" --selector "$2" --text "$3" 2>&1; }
click() { $LOOM action web.click      --session "$1" --selector "$2" 2>&1; }
wait_() { $LOOM action web.wait       --session "$1" --selector "$2" --timeout_ms "${3:-10000}" 2>&1; }
# Gate on page-level readiness. Required after an interaction that triggers a
# TOP-LEVEL navigation (e.g. a form-POST submit): loom drives Chromium under a
# deterministic virtual-time clock that is frozen once a navigate drains its
# budget, and web.wait_for re-arms a budget so the post-click navigation
# advances (see #219 / #auth0-ulp). web.wait only selector-polls the *current*
# document and never drives that navigation.
settle() { $LOOM action web.wait_for  --session "$1" --until "${2:-settled}" --timeout_ms "${3:-10000}" 2>&1; }

result() { echo "$1" | jq -r '.return_value_json // empty' 2>/dev/null | jq -r 'select(. != null)' 2>/dev/null; }
got_hash() { echo "$1" | jq -e '.action_hash' >/dev/null 2>&1; }

# -- Saucedemo.com — full e-commerce checkout --------------------------
sect "Saucedemo.com — login + checkout (automation-friendly demo store)"
S=$($LOOM session create --profile standard --budget wall_clock=120s 2>&1 | jq -r .session_id)
NAV=$(nav "$S" 'https://www.saucedemo.com/')
if got_hash "$NAV"; then
  ok "saucedemo-loaded"
  type_ "$S" '#user-name' 'standard_user' >/dev/null
  type_ "$S" '#password'  'secret_sauce'  >/dev/null
  click "$S" '#login-button' >/dev/null
  # Saucedemo is a multi-page app: every step here (#login-button, .shopping_cart_link,
  # #checkout, #continue, #finish) is a TOP-LEVEL navigation to a fresh .html page, so
  # each navigating click is followed by settle to advance it under virtual time before
  # the next selector wait/read (same reason as the the-internet submit above).
  settle "$S" settled 10000 >/dev/null
  W=$(wait_ "$S" '.inventory_list' 10000)
  if got_hash "$W"; then
    ok "saucedemo-login-succeeded (inventory list visible)"
    # Add first item to cart (same-page mutation, no navigation)
    click "$S" '.btn_inventory' >/dev/null
    CART=$(ev "$S" 'document.querySelector(".shopping_cart_badge")?.textContent || "0"')
    if [ "$(result "$CART")" = "1" ]; then
      ok "saucedemo-add-to-cart"
    else
      fail "saucedemo-add-to-cart" "badge='$(result "$CART")'"
    fi
    # Go to cart and checkout
    click "$S" '.shopping_cart_link' >/dev/null
    settle "$S" settled 8000 >/dev/null
    wait_ "$S" '#checkout' 5000 >/dev/null
    click "$S" '#checkout' >/dev/null
    settle "$S" settled 8000 >/dev/null
    wait_ "$S" '#first-name' 5000 >/dev/null
    type_ "$S" '#first-name' 'Test'        >/dev/null
    type_ "$S" '#last-name'  'User'        >/dev/null
    type_ "$S" '#postal-code' '94107'      >/dev/null
    click "$S" '#continue'                 >/dev/null
    settle "$S" settled 8000 >/dev/null
    wait_ "$S" '#finish' 5000 >/dev/null
    click "$S" '#finish'                   >/dev/null
    settle "$S" settled 8000 >/dev/null
    DONE=$(ev "$S" 'document.querySelector(".complete-header")?.textContent || ""')
    if echo "$(result "$DONE")" | grep -qi "thank you"; then
      ok "saucedemo-checkout-completed"
    else
      fail "saucedemo-checkout-completed" "got '$(result "$DONE")'"
    fi
  else
    fail "saucedemo-login-succeeded" "$W"
  fi
else
  skip "saucedemo-loaded" "navigate failed: $(echo "$NAV" | head -c 200)"
fi
$LOOM session close "$S" >/dev/null 2>&1 || true

# -- The Internet — login form (no anti-bot) ---------------------------
sect "the-internet.herokuapp.com — login flow"
S=$($LOOM session create --profile standard --budget wall_clock=60s 2>&1 | jq -r .session_id)
NAV=$(nav "$S" 'https://the-internet.herokuapp.com/login')
if got_hash "$NAV"; then
  ok "the-internet-loaded"
  type_ "$S" '#username' 'tomsmith' >/dev/null
  type_ "$S" '#password' 'SuperSecretPassword!' >/dev/null
  click "$S" 'button[type="submit"]' >/dev/null
  # Submit is a top-level form POST -> /secure navigation; settle to advance it
  # (virtual-time budget) before reading the post-redirect flash banner.
  settle "$S" settled 8000 >/dev/null
  FLASH=$(ev "$S" 'document.querySelector("#flash")?.textContent || ""')
  if echo "$(result "$FLASH")" | grep -qi "logged into"; then
    ok "the-internet-login"
  else
    fail "the-internet-login" "flash='$(result "$FLASH")'"
  fi
else
  skip "the-internet-loaded" "$(echo "$NAV" | head -c 200)"
fi
$LOOM session close "$S" >/dev/null 2>&1 || true

# -- Booking.com — search only, anti-bot territory ---------------------
sect "Booking.com — hotel search (search-only, no payment)"
S=$($LOOM session create --profile standard --budget wall_clock=60s 2>&1 | jq -r .session_id)
NAV=$(nav "$S" 'https://www.booking.com/')
echo "$NAV" >"$RESULTS/booking-nav.json"
if got_hash "$NAV"; then
  ok "booking-loaded"
  TITLE=$(result "$(ev "$S" 'document.title')")
  if [ -n "$TITLE" ]; then
    ok "booking-page-title-readable ('$TITLE')"
  else
    fail "booking-page-title-readable" "no title surfaced"
  fi
  # Try to detect anti-bot challenge
  H=$(ev "$S" 'document.body.innerHTML.length')
  HSIZE=$(result "$H")
  ok "booking-html-size-bytes ($HSIZE)"
  if [ -n "$HSIZE" ] && [ "$HSIZE" -lt 50000 ]; then
    fail "booking-not-anti-bot-challenged" "tiny body html ($HSIZE bytes); likely cloudflare/captcha"
  else
    ok "booking-rendered-real-content"
  fi
else
  skip "booking-loaded" "navigate failed: $(echo "$NAV" | head -c 200)"
fi
$LOOM session close "$S" >/dev/null 2>&1 || true

# -- Wikipedia — content-rich page, JS execution ----------------------
sect "Wikipedia — content + JS evaluation"
S=$($LOOM session create --profile standard --budget wall_clock=60s 2>&1 | jq -r .session_id)
NAV=$(nav "$S" 'https://en.wikipedia.org/wiki/Web_browser')
if got_hash "$NAV"; then
  ok "wikipedia-loaded"
  H1=$(result "$(ev "$S" 'document.querySelector("h1")?.textContent')")
  if [ -n "$H1" ]; then
    ok "wikipedia-h1-readable ('$H1')"
  else
    fail "wikipedia-h1-readable" "no h1"
  fi
fi
$LOOM session close "$S" >/dev/null 2>&1 || true

# -- Summary -----------------------------------------------------------
sect "Summary"
TOTAL=$((PASS+FAIL))
printf '  %d / %d passed (%d skipped)\n' "$PASS" "$TOTAL" "$SKIP"
if [ "$FAIL" -gt 0 ]; then
  printf '\nFailed:\n'
  for f in "${FAILED[@]}"; do printf '  - %s\n' "$f"; done
  exit 1
fi
