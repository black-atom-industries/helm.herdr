# Query view: search finds a project that is not open yet, and Enter opens it as a new workspace.
# Record from a scratch directory: REPO=<repo> vhs "$REPO/docs/assets/tapes/query.tape"
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
Sleep 1.2s

Type "/"
Ctrl+y
Sleep 600ms
Type "d"
Sleep 900ms
Type "o"
Ctrl+y
Sleep 1.5s
Ctrl+u
Type "/"
Sleep 400ms
Type "inf"
Sleep 500ms
Type "ra"
Ctrl+y
Sleep 1.5s
Enter
Sleep 2.5s

Hide
Type "herdr server stop"
Enter
Sleep 1s
Type 'cd / && rm -rf "$HOME"'
Enter
Sleep 500ms
