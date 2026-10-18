# Topology view: open Helm, walk Workspace → Tab → Pane, land on the exact pane.
# Record from a scratch directory: REPO=<repo> vhs "$REPO/docs/assets/tapes/topology.tape"
# Herdr repaints its popup only on the next input, so every key is followed by Ctrl+y, which Helm ignores.

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
Sleep 1.5s

Type "J"
Ctrl+y
Sleep 1s
Type "l"
Ctrl+y
Sleep 1s
Type "l"
Ctrl+y
Sleep 1s
Type "j"
Ctrl+y
Sleep 1.2s
Enter
Sleep 2s

Hide
Type "herdr server stop"
Enter
Sleep 1s
Type 'cd / && rm -rf "$HOME"'
Enter
Sleep 500ms
