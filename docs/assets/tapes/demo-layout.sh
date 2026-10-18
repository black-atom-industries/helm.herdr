#!/bin/sh
# Builds the demo workspaces inside the running demo Herdr session.
set -e

herdr plugin link "$REPO" >/dev/null

workspace_id() {
  herdr workspace list | jq -r --arg label "$1" '.result.workspaces[] | select(.label == $label) | .workspace_id'
}

tab_id() {
  herdr tab list --workspace "$1" | jq -r --arg label "$2" '.result.tabs[] | select(.label == $label) | .tab_id'
}

first_pane() {
  herdr pane list | jq -r --arg tab "$1" '[.result.panes[] | select(.tab_id == $tab)][0].pane_id'
}

name_first_tab() {
  tab=$(herdr tab list --workspace "$1" | jq -r '.result.tabs[0].tab_id')
  herdr tab rename "$tab" "$2" >/dev/null
  herdr pane rename "$(first_pane "$tab")" "$3" >/dev/null
}

add_tab() {
  herdr tab create --workspace "$1" --cwd "$2" --label "$3" --no-focus >/dev/null
  herdr pane rename "$(first_pane "$(tab_id "$1" "$3")")" "$4" >/dev/null
}

herdr workspace create --cwd "$HOME/code/web" --label web --no-focus >/dev/null
herdr workspace create --cwd "$HOME/code/docs" --label docs --no-focus >/dev/null
api=$(herdr workspace list | jq -r '.result.workspaces[] | select(.focused) | .workspace_id')
herdr workspace rename "$api" api >/dev/null
web=$(workspace_id web)
docs=$(workspace_id docs)

name_first_tab "$api" code editor
api_code=$(tab_id "$api" code)
herdr pane split "$(first_pane "$api_code")" --direction right --no-focus >/dev/null
herdr pane rename "$(herdr pane list | jq -r --arg tab "$api_code" '[.result.panes[] | select(.tab_id == $tab)][1].pane_id')" tests >/dev/null
add_tab "$api" "$HOME/code/api" server "dev server"

name_first_tab "$web" code editor
add_tab "$web" "$HOME/code/web" preview "vite"

name_first_tab "$docs" writing editor

clear
