# scripts/session-diff-divergence.tape — Cut B: "find the one action that diverged"
#
# Prereqs (see `loom doctor`):
#   - loom built + on PATH, `loom doctor` green (Chromium present + verified)
#   - vhs + ttyd + ffmpeg on PATH  (brew install vhs)
#   - jq on PATH; outbound network for example.com + example.org
# Render:
#   just record-demo                          # both cuts
#   vhs scripts/session-diff-divergence.tape  # this cut only
#
# The tape runs the REAL loom binary — it is the executable spec for the demo.
# Do not hand-edit the captured output.

Output docs/assets/session-diff-divergence.gif

Set Shell zsh
Set FontSize 20
Set Width 1000
Set Height 640
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set CursorBlink false

# --- off camera: sanitize prompt + build two sessions that should match but don't ---
Hide
Type "export PS1='loom-demo $ '"
Enter
Type "setopt interactive_comments"
Enter
Type "loom serve >/dev/null 2>&1 || true"
Enter
Type "clear"
Enter
Type "A=$(loom session create --profile standard | jq -r .session_id)"
Enter
Type "loom action web.navigate --session $A --url https://example.com >/dev/null"
Enter
Type "loom session close $A >/dev/null"
Enter
Type "B=$(loom session create --profile standard | jq -r .session_id)"
Enter
Type "loom action web.navigate --session $B --url https://example.org >/dev/null"
Enter
Type "loom session close $B >/dev/null"
Enter
Type "clear"
Enter
Show

# --- on camera ---
Sleep 700ms
Type "# two runs that should match — but one navigated somewhere else"
Enter
Sleep 400ms
Type 'loom session diff $A $B --json | jq "{diverged: (.field_diffs|length), fields: [.field_diffs[].field_path]}"'
Enter
Sleep 3s
Type "# diff names every field that parted ways (incl. final_url)"
Enter
Sleep 600ms
Type "# exit code 6 = SessionsDiffer — a distinct code, not a crash"
Enter
Sleep 400ms
Type "loom session diff $A $B > /dev/null 2>&1; echo exit: $?"
Enter
Sleep 2800ms
