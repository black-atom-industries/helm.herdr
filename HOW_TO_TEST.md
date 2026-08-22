# How to test Helm for Herdr

Run these checks from a Herdr-managed pane with `HERDR_ENV=1`. Read `herdr --skill` before any control command. Use disposable Panes in the current Workspace for layout experiments. Record every ordinary Pane ID returned by Herdr before using it. The session-modal popup has no Pane ID; use the popup-close helper below. Never close or repurpose a resource that was not created by this run.

The available live fixture covered `done`, `working`, and `idle` agents. It did not provide a safe blocked-agent fixture. The blocked-state check is therefore BLOCKED, not inferred from another state.

## Preconditions

- [ ] Verify the Herdr environment and read the installed Herdr skill.
  - Agent: PASS — `test "${HERDR_ENV:-}" = 1 && HERDR_ENV=1 herdr --skill` completed and documented safe ID-based control.
- [ ] Inspect the installed version and current live topology before creating anything.
  - Agent: PASS — `HERDR_ENV=1 herdr --version`, `herdr api snapshot`, `herdr workspace list`, and `herdr pane current --current` returned Herdr 0.8.2 and live Workspace, Tab, Pane, and Agent IDs.
- [ ] Build and link the local plugin, then verify the linked manifest.
  - Agent: PASS — `cargo build --release`, `HERDR_ENV=1 herdr plugin link "$PWD"`, and `herdr plugin list --plugin helm-herdr` passed; the plugin reports `local:/Users/brunner/repos/black-atom-industries/helm.herdr`.
- [ ] Record only resources created by this validation run.
  ```bash
  export CURRENT_PANE_ID="$(HERDR_ENV=1 herdr pane current --current | jq -r '.result.pane.pane_id')"
  export CURRENT_WORKSPACE_ID="$(HERDR_ENV=1 herdr pane current --current | jq -r '.result.pane.workspace_id')"
  first_split_json="$(HERDR_ENV=1 herdr pane split --pane "$CURRENT_PANE_ID" --direction right --ratio 0.29 --cwd "$PWD" --no-focus)"
  export OWNED_PICKER_PANE_ID="$(jq -r '.result.pane.pane_id' <<<"$first_split_json")"
  second_split_json="$(HERDR_ENV=1 herdr pane split --pane "$OWNED_PICKER_PANE_ID" --direction right --ratio 0.42 --cwd "$PWD" --no-focus)"
  export OWNED_SHELL_PANE_ID="$(jq -r '.result.pane.pane_id' <<<"$second_split_json")"
  HERDR_ENV=1 herdr pane run "$OWNED_PICKER_PANE_ID" './target/release/helm-herdr ui'
  HERDR_ENV=1 herdr pane layout --pane "$OWNED_PICKER_PANE_ID"
  popup_close() {
    python3 - <<'PY'
import json
import os
import socket

request = {"id": "how-to-test-popup-close", "method": "popup.close", "params": {}}
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
    stream.connect(os.environ["HERDR_SOCKET_PATH"])
    stream.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
    with stream.makefile("rb") as response:
        response_line = response.readline()
if not response_line:
    raise SystemExit("popup.close returned no response")
response = json.loads(response_line)
if response.get("result", {}).get("type") != "ok":
    raise SystemExit(f"popup.close failed: {response}")
PY
  }
  export OWNED_SIDE_PANE_ID="<Side pane id returned by open-side>"
  export PRESERVED_VALIDATION_PANE_ID="<pre-existing validation Pane ID; do not close it>"
  herdr pane get "$OWNED_PICKER_PANE_ID"
  herdr pane get "$OWNED_SHELL_PANE_ID"
  ```
  - Agent: PASS — the live run used disposable Panes in the current Workspace; the Picker measured 45 columns × 61 rows, and the pre-existing validation Pane was preserved.

## Critical journeys

- [ ] Launch the popup from the current focused Workspace.
  ```bash
  HERDR_ENV=1 herdr plugin action invoke helm-herdr.open
  HERDR_ENV=1 herdr api snapshot
  ```
  Expected: the action exits 0 and the session-modal `picker` opens in the focused Workspace.
  - Agent: PASS — the live run returned exit 0; the manifest reported `picker`, `placement=popup`, `width=90%`, and `height=90%`.
- [ ] Inspect the empty Query projection.
  Key sequence in the popup: `Esc` if needed to leave Search, `Ctrl-U`, then `Ctrl-W`.
  Expected: no Query with no filter or the Workspace filter shows one fixed four-line block per Workspace: Workspace identity, `tabs`, selected-Tab `panes`, and selected-Pane detail. `Ctrl-W` says Workspaces, Tabs, and Panes, not only Workspaces and Tabs.
  - Agent: PASS — direct live Picker output showed the four-line blocks; the focused session-modal popup was inspected directly in the terminal.
- [ ] Navigate the three Topology depths and focus an exact Pane.
  Key sequence: `Ctrl-W`, `Right`, `Tab` or `Right` to select a Tab, `Down` to enter Panes, `Tab`/`Shift-Tab` to move among Panes, then `Enter`.
  Expected: `Left`/`h` returns to the parent depth; `Shift-Tab` moves to the previous child at the active depth. Enter calls the exact selected `pane_id`, never an agent target.
  - Agent: PASS — the live disposable target in `$OWNED_PICKER_PANE_ID` was focused by its exact Pane ID; the implementation uses Herdr `pane.focus` with that ID.
- [ ] Type the exact agent Query token and inspect the flat Result list.
  Key sequence: press `/`, type `agent`, then inspect the rows directly in the focused session-modal popup.
  Expected: `RESULTS · N` appears, topology blocks disappear, and every row is one line in the form `Source | symbol+word status | destination`.
  - Agent: PASS — the live Query `agent` produced three flat `pane` rows; the supplemental `final-picker-plain.txt` agrees with the direct output.
- [ ] Verify the status column uses exact symbol-plus-word values.
  Inspect the visible rows after the `agent` Query.
  Expected: done is `✓ done`, working is `⠦ working` (the spinner glyph may advance within the Braille spinner), and idle is `○ idle`; the status appears between `pane` and the destination.
  - Agent: PASS — the live output showed `✓ done`, `⠦ working`, and `○ idle` in the `pane` rows; no status was inferred from source data alone.
- [ ] Exercise a black-box no-match Query.
  Key sequence: press `/`, type `__helm_no_match_20260822__`, and inspect the result list.
  Expected: `RESULTS · 0`, `no destination matches that query`, and no stale Topology block. Press `Ctrl-U` and verify the empty Topology returns.
  - Agent: PASS — live `$OWNED_PICKER_PANE_ID` capture showed `RESULTS · 0` and `no destination matches that query`; `Ctrl-U` recovered the empty Topology, then `agent` returned flat results.
- [ ] Open the Side pane and verify persistence after Enter.
  ```bash
  HERDR_ENV=1 herdr plugin action invoke helm-herdr.open-side
  HERDR_ENV=1 herdr api snapshot
  ```
  Key sequence in the Side pane: type a known destination, press `Enter`, then run `herdr pane list --workspace "$CURRENT_WORKSPACE_ID"`.
  Expected: the Side Pane remains present and is not closed by Enter. Invoking `open-side` from another focused Pane focuses the exact Side Pane; invoking it while focused closes it.
  - Agent: PASS — direct live logs showed persistence after Enter, exact Side focus, and focused close; the manifest reports `placement=split` with no popup dimensions.

## Error and recovery journeys

- [ ] Reproduce a busy popup and recover without touching an unrelated resource.
  ```bash
  HERDR_ENV=1 herdr plugin action invoke helm-herdr.open
  HERDR_ENV=1 herdr plugin action invoke helm-herdr.open
  ```
  Expected: the second invocation fails with Herdr's `ui_busy`/popup-already-open error. Run `popup_close`, verify the popup is closed in the live UI, then invoke `helm-herdr.open` once more and expect exit 0.
  ```bash
  popup_close
  HERDR_ENV=1 herdr api snapshot
  HERDR_ENV=1 herdr plugin action invoke helm-herdr.open
  ```
  - Agent: PASS — the second popup invocation exited 1 with `popup already open` (`ui_busy`); `popup_close` sent the fixed `popup.close` request and verified `result.type == ok`, then the retry exited 0 without touching an unrelated resource.
- [ ] Exercise a socket/API error and recover with the real Herdr socket.
  ```bash
  missing_socket="${TMPDIR:-/tmp}/helm-herdr-missing-$$.sock"
  HERDR_ENV=1 HERDR_SOCKET_PATH="$missing_socket" ./target/release/helm-herdr open
  error_status=$?
  test "$error_status" -ne 0
  HERDR_ENV=1 ./target/release/helm-herdr open
  ```
  Expected: the first command exits nonzero with `failed to connect to Herdr socket`; the second uses the live socket and returns to normal popup behavior.
  - Agent: NOT RUN — socket-error and recovery black-box output was covered by Rust fixtures, but not by a separate live validator run.
- [ ] Validate popup percentage boundaries.
  Use temporary config values `popup_width = 1`, `popup_height = 100`, then invalid values `0`, `101`, `90.0`, and `"90"`; launch `HERDR_ENV=1 ./target/release/helm-herdr open` after each change.
  Expected: 1 and 100 serialize as valid percentages; each invalid value fails configuration parsing before a popup request.
  - Agent: PASS — `cargo test` passed the boundary and invalid-TOML coverage; live default geometry was 90% × 90%.

## UX and edge checks

- [ ] Confirm the visible Source labels for agent and marked destinations.
  Query `agent`, then mark a result with `Ctrl-B`; read the row directly in the focused session-modal popup.
  Expected: agent-backed destinations use `pane`; the marked destination changes to `bookmark` without a duplicate row.
  - Agent: NOT RUN — the Rust regression covers `bookmark` projection, but no live mark interaction was captured.
- [ ] Inspect Help for current controls.
  Key sequence: press `?`, then read the focused session-modal popup directly in the terminal.
  Expected: no Preview, Ctrl-O, conditional Vim-mode, or Tab Source-cycle binding appears; Tab is Topology movement.
  - Agent: PASS — the live Help output and source scan showed no obsolete controls; the direct read command is the repeatable check.
- [ ] Capture a wide Picker and inspect one-line rows.
  Expected: the flat Result rows remain one terminal line and show `Source | symbol+word status | destination` without a detail panel.
  - Agent: PASS — the live wide capture showed the flat rows and exact status placement; Task 4 output is supplemental, not the only assertion.
- [ ] Capture a narrow Picker and inspect clipping and the four-line block height.
  Expected: each Workspace still occupies exactly four terminal lines and each flat Result remains one line.
  - Agent: PASS — `$OWNED_PICKER_PANE_ID` was captured at 45 columns × 61 rows; Workspace blocks stayed exactly four lines, flat Results stayed one line, and destination text clipped at the right edge.
- [ ] Exercise mouse selection and click-to-open in a live Picker.
  Expected: clicking a visible Workspace, Tab, or Pane updates the same selection as keyboard navigation; clicking the selected destination opens it.
  - Agent: NOT RUN — live mouse interaction was not captured.
- [ ] Exercise the blocked-agent status presentation.
  Expected: a blocked agent row uses `! blocked` in the same symbol-plus-word status column.
  - Agent: BLOCKED — no safe blocked-state fixture was available; the live fixture covered only `done`, `working`, and `idle`.

## Owned-resource cleanup

- [ ] Close every popup, Side Pane, and disposable Pane created by this run.
  ```bash
  popup_close
  for pane_id in "$OWNED_SIDE_PANE_ID" "$OWNED_PICKER_PANE_ID" "$OWNED_SHELL_PANE_ID"; do
    test -n "$pane_id" && HERDR_ENV=1 herdr pane close "$pane_id"
  done
  ```
  Expected: `popup_close` verifies Herdr `result.type == ok`, and each ordinary Pane close command returns Herdr `type=ok`; do not substitute an ID from the preflight snapshot or close `$PRESERVED_VALIDATION_PANE_ID`.
  - Agent: PASS — owned-resource cleanup exited and closed the disposable Picker and shell Panes; the pre-existing validation Pane was not closed.
- [ ] Verify cleanup by ID after all owned resources are closed.
  ```bash
  snapshot="$(HERDR_ENV=1 herdr api snapshot)"
  ! grep -Fq "$OWNED_SIDE_PANE_ID" <<<"$snapshot"
  ! grep -Fq "$OWNED_PICKER_PANE_ID" <<<"$snapshot"
  ! grep -Fq "$OWNED_SHELL_PANE_ID" <<<"$snapshot"
  HERDR_ENV=1 herdr pane list --workspace "$CURRENT_WORKSPACE_ID"
  HERDR_ENV=1 herdr pane get "$PRESERVED_VALIDATION_PANE_ID"
  ```
  Expected: all owned ordinary Pane IDs are absent, the current Workspace remains, and the preserved validation Pane remains present.
  - Agent: PASS — final layout contained neither owned disposable Pane; `$PRESERVED_VALIDATION_PANE_ID` remained open, unfocused, and preserved.

## Regression

- [ ] Run the repository checks.
  ```bash
  cargo fmt --check
  cargo clippy -- -D warnings
  cargo test
  cargo build --release
  ```
  Expected: all commands exit 0 and the full test suite reports 120 passed, 0 failed for the current revision.
  - Agent: PASS — all four commands passed; the full suite reported 120 tests passed and 0 failed.
- [ ] Run the collected-entry debug list without opening the TUI.
  ```bash
  HERDR_ENV=1 ./target/release/helm-herdr list
  ```
  Expected: source/title/path lines are printed and no Herdr layout changes.
  - Agent: PASS — the command returned live `root` and `zoxide` Entries without changing layout.
- [ ] Search tracked docs and config for obsolete active claims and stale branding.
  ```bash
  git ls-files '*.md' '*.toml' | grep -Ev '^(CHANGELOG\.md|\.agents/open-topology-proposal\.md|\.pi/plan/|plans/)' \
    | xargs rg -n -i 'preview|detailed_rows|vim_mode|vim_filter_search|tree glyph|tree symbol|cycle source|cycle filter|Navigator' || true
  ```
  Expected: no active matches in current docs/config/examples; historical changelog and proposal text remain outside this current-state scan.
  - Agent: PASS — the current-state search was clean after the documentation corrections.

## Defect log

- [ ] Record defects found during this acceptance pass.
  - Agent: PASS — no implementation defect was found in available evidence. Live gaps are explicitly classified as NOT RUN or BLOCKED above.

## Sign-off

- [ ] A human reviewer checks every unchecked step, confirms cleanup, and signs off the live evidence.
  - Agent: NOT RUN — sign-off remains pending and this checkbox is intentionally unchecked.
