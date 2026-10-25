# Glance: open Helm on the glance, move through the rows, jump a group, search, and close.
# Record from a scratch directory: REPO=<repo> vhs "$REPO/docs/assets/tapes/glance.tape"
# Herdr repaints its popup only on the next input, so every key is followed by Ctrl+y, which Helm ignores.
# A row label opens its row and closes the popup, so the tape pauses on the labels without pressing one.

Output frames/
Set Shell zsh
Set FontSize 20
Set Width 1400
Set Height 760
Set Framerate 50
Set TypingSpeed 60ms

Hide
Type 'source "$REPO/docs/assets/tapes/demo-env.sh" && clear'
Enter
Type "herdr"
Enter
Sleep 3s
Type '"$REPO/docs/assets/tapes/demo-layout.sh"'
Enter
Sleep 3s
Show

Sleep 1s
Ctrl+g
Sleep 500ms
Ctrl+y
Sleep 2s

Type "j"
Ctrl+y
Sleep 900ms
Type "j"
Ctrl+y
Sleep 900ms
Type "k"
Ctrl+y
Sleep 1.2s
Type "]"
Ctrl+y
Sleep 1.2s

Type "/"
Ctrl+y
Sleep 600ms
Type "d"
Sleep 400ms
Type "o"
Ctrl+y
Sleep 1.5s
Escape
Ctrl+y
Sleep 1.2s
Escape
Sleep 1.5s

Hide
Type "herdr server stop"
Enter
Sleep 1s
Type 'cd / && rm -rf "$HOME"'
Enter
Sleep 500ms
