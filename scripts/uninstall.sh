#!/usr/bin/env bash
#
# loom uninstaller — removes everything `loom postinstall` (and the install
# scripts) put on disk: the four binaries, the macOS LaunchDaemon, the
# Chromium cache + AOT surfaces under ~/.config/loom, and the session/blob
# data store. Safe by default: prints a plan and asks before deleting.
#
# Usage:
#   scripts/uninstall.sh            # interactive — show plan, confirm, remove
#   scripts/uninstall.sh --dry-run  # show what would be removed, delete nothing
#   scripts/uninstall.sh --yes      # skip the confirmation prompt (for scripts)
#   scripts/uninstall.sh --keep-data  # remove binaries/Chromium but keep sessions
#
# It never touches a custom location set via LOOM_DATA_ROOT / LOOM_CHROMIUM_PATH
# / a Homebrew prefix — those are reported at the end for manual cleanup.

set -euo pipefail

DRY_RUN=0
ASSUME_YES=0
KEEP_DATA=0

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --yes|-y)  ASSUME_YES=1 ;;
    --keep-data) KEEP_DATA=1 ;;
    -h|--help)
      # Print the leading comment header (everything after the shebang up to
      # the first blank/non-comment line), stripped of the leading "# ".
      awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
      exit 0
      ;;
    *)
      echo "uninstall: unknown flag '$arg' (try --help)" >&2
      exit 2
      ;;
  esac
done

OS="$(uname -s)"
HOME_DIR="${HOME:?HOME must be set}"

# --- Resolve the platform-specific paths -----------------------------------
# Config dir is ~/.config/loom on BOTH platforms (postinstall hardcodes it):
# holds chromium/ (the ~150 MB cache), surfaces/ (AOT .cwasm), schemas/.
CONFIG_DIR="$HOME_DIR/.config/loom"

# Data dir + auxiliary binaries (loom-daemon/loom-mcp/loom-shim-chromium live
# under <data>/bin) follow the OS data dir.
if [ "$OS" = "Darwin" ]; then
  DATA_DIR="$HOME_DIR/Library/Application Support/loom"
else
  DATA_DIR="${XDG_DATA_HOME:-$HOME_DIR/.local/share}/loom"
fi

PLIST="/Library/LaunchDaemons/com.loom.daemon.plist"
PLIST_LABEL="com.loom.daemon"

# The `loom` CLI itself can land in a few places depending on install method.
LOOM_BIN_CANDIDATES=(
  "$HOME_DIR/.cargo/bin/loom"
  "$HOME_DIR/.local/bin/loom"
)
# Resolve the live one (and surface a Homebrew install for manual removal).
RESOLVED_LOOM=""
if command -v loom >/dev/null 2>&1; then
  RESOLVED_LOOM="$(command -v loom)"
fi

# Honour the header contract: never touch a Homebrew prefix. A brew-installed
# loom resolves to $(brew --prefix)/bin/loom (a symlink into the Cellar);
# rm -rf'ing it would leave the keg with a broken link and bypass brew's own
# bookkeeping. Detect both the path itself and its symlink target against
# `brew --prefix` plus the standard Homebrew locations, and route the binary
# to the LEFTOVERS report (brew uninstall loom) instead of the removal plan.
is_homebrew_path() {
  local p="$1" real brew_prefix="" candidate
  real="$(readlink "$p" 2>/dev/null || true)"
  if command -v brew >/dev/null 2>&1; then
    brew_prefix="$(brew --prefix 2>/dev/null || true)"
  fi
  for candidate in "$p" "$real"; do
    [ -n "$candidate" ] || continue
    case "$candidate" in
      */Cellar/*|/opt/homebrew/*|/usr/local/Cellar/*|/home/linuxbrew/.linuxbrew/*) return 0 ;;
    esac
    if [ -n "$brew_prefix" ]; then
      case "$candidate" in
        "$brew_prefix"/*) return 0 ;;
      esac
    fi
  done
  return 1
}

HOMEBREW_LOOM=""
if [ -n "$RESOLVED_LOOM" ] && is_homebrew_path "$RESOLVED_LOOM"; then
  HOMEBREW_LOOM="$RESOLVED_LOOM"
  RESOLVED_LOOM=""
fi

# --- Build the removal plan -------------------------------------------------
TARGETS=()
add_target() { [ -e "$1" ] && TARGETS+=("$1") || true; }

for c in "${LOOM_BIN_CANDIDATES[@]}"; do add_target "$c"; done
if [ -n "$RESOLVED_LOOM" ]; then
  case " ${TARGETS[*]:-} " in
    *" $RESOLVED_LOOM "*) : ;;
    *) add_target "$RESOLVED_LOOM" ;;
  esac
fi
add_target "$CONFIG_DIR"
if [ "$KEEP_DATA" -eq 0 ]; then
  add_target "$DATA_DIR"
else
  # Still remove the bundled aux binaries even when keeping session data.
  add_target "$DATA_DIR/bin"
fi

HAS_PLIST=0
if [ "$OS" = "Darwin" ] && [ -e "$PLIST" ]; then
  HAS_PLIST=1
fi

echo "loom uninstall plan ($OS):"
if [ "$HAS_PLIST" -eq 1 ]; then
  echo "  • stop + remove LaunchDaemon $PLIST_LABEL ($PLIST)  [needs sudo]"
fi
if [ "${#TARGETS[@]}" -eq 0 ] && [ "$HAS_PLIST" -eq 0 ]; then
  echo "  (nothing found — loom does not appear to be installed for this user)"
  if [ -n "$HOMEBREW_LOOM" ]; then
    echo "  (Homebrew-managed loom at $HOMEBREW_LOOM is out of scope — run: brew uninstall loom)"
  fi
  exit 0
fi
for t in "${TARGETS[@]:-}"; do
  [ -n "$t" ] && echo "  • rm -rf $t"
done
if [ -n "$HOMEBREW_LOOM" ]; then
  echo "  • leave Homebrew-managed loom at $HOMEBREW_LOOM (run: brew uninstall loom)"
fi

if [ "$DRY_RUN" -eq 1 ]; then
  echo
  echo "--dry-run: nothing was removed."
  exit 0
fi

if [ "$ASSUME_YES" -eq 0 ]; then
  echo
  printf "Proceed? [y/N] "
  read -r reply
  case "$reply" in
    y|Y|yes|YES) : ;;
    *) echo "Aborted."; exit 1 ;;
  esac
fi

# --- Execute ----------------------------------------------------------------
if [ "$HAS_PLIST" -eq 1 ]; then
  echo "Stopping LaunchDaemon (sudo)…"
  # `bootout` is the modern verb; fall back to `unload` on older macOS.
  sudo launchctl bootout "system/$PLIST_LABEL" 2>/dev/null \
    || sudo launchctl unload "$PLIST" 2>/dev/null || true
  sudo rm -f "$PLIST"
fi

for t in "${TARGETS[@]:-}"; do
  [ -n "$t" ] || continue
  echo "Removing $t"
  rm -rf "$t"
done

echo
echo "Done. loom has been removed."

# --- Report anything we deliberately didn't touch ---------------------------
LEFTOVERS=()
[ -n "${LOOM_DATA_ROOT:-}" ] && LEFTOVERS+=("LOOM_DATA_ROOT=$LOOM_DATA_ROOT (custom data dir — remove manually)")
[ -n "${LOOM_CHROMIUM_PATH:-}" ] && LEFTOVERS+=("LOOM_CHROMIUM_PATH=$LOOM_CHROMIUM_PATH (custom Chromium — remove manually)")
if [ -n "$HOMEBREW_LOOM" ]; then
  LEFTOVERS+=("Homebrew-managed loom at $HOMEBREW_LOOM — run: brew uninstall loom")
elif command -v brew >/dev/null 2>&1 && brew list loom >/dev/null 2>&1; then
  LEFTOVERS+=("Homebrew formula still installed — run: brew uninstall loom")
fi
if [ "$OS" = "Darwin" ]; then
  LEFTOVERS+=("Vault secrets in the macOS Keychain (labels com.loom.auth / com.loom.vault.user) are left intact; delete via Keychain Access if you used the vault.")
fi
if [ "${#LEFTOVERS[@]}" -gt 0 ]; then
  echo
  echo "Not removed (out of this script's scope):"
  for l in "${LEFTOVERS[@]}"; do echo "  • $l"; done
fi
