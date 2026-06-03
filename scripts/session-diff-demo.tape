# scripts/session-diff-demo.tape — Cut A: "determinism you can diff"
#
# Prereqs (see `loom doctor`):
#   - loom built + on PATH, `loom doctor` green (Chromium present + verified)
#   - vhs + ttyd + ffmpeg on PATH  (brew install vhs)
#   - jq on PATH; outbound network for example.com
# Render:
#   just record-demo            # both cuts
#   vhs scripts/session-diff-demo.tape   # this cut only
#
# The tape runs the REAL loom binary — it is the executable spec for the demo.
# Do not hand-edit the captured output.

Output docs/assets/session-diff-demo.gif

Set Shell zsh
Set FontSize 20
Set Width 1000
Set Height 600
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set CursorBlink false

# --- off camera: sanitize the prompt (no real $HOME/user/host) + prep a session ---
Hide
Type "export PS1='loom-demo $ '"
Enter
Type "setopt interactive_comments"
Enter
Type "loom serve >/dev/null 2>&1 || true"
Enter
Type "clear"
Enter
Type "SESSION=$(loom session create --profile standard | jq -r .session_id)"
Enter
Type "loom action web.navigate --session $SESSION --url https://example.com >/dev/null"
Enter
Type "clear"
Enter
Show

# --- on camera ---
Sleep 700ms
Type "# a flow that touches the usual flake sources: RNG + clock"
Enter
Sleep 400ms
Type "loom action web.evaluate --session $SESSION --expression 'JSON.stringify([Math.random(), Date.now()])'"
Enter
Sleep 2s
Type "loom session close $SESSION"
Enter
Sleep 900ms
Type "# replay it — an independent run, same seed + epoch"
Enter
Sleep 400ms
Type "NEW=$(loom session replay $SESSION | jq -r .session_id)"
Enter
Sleep 1500ms
Type "# diff the two runs: determinism = an empty diff"
Enter
Sleep 400ms
Type "loom session diff $SESSION $NEW"
Enter
Sleep 1500ms
Type "echo exit: $?"
Enter
Sleep 2800ms
