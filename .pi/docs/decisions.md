# Decisions

## Public name

Use `helm-herdr` for the plugin id, Cargo package/binary, config directory, and action prefix. The repository is `black-atom-industries/helm.herdr`. First run copies missing files from the legacy Helm config directories.

## Minimum release quality

Keep these files:
- README
- LICENSE
- CHANGELOG
- SECURITY
- CONTRIBUTING
- RELEASE
- GitHub CI/release workflows
- issue/PR templates

But do not add enterprise boilerplate beyond that.

## No new dependencies for picker UX

The plugin is itself a Rust TUI. Do not depend on `fzf`, `tv`, etc.
`zoxide` is optional because it is a data source, not UI.

## Herdr Plus dependency stays optional

If Herdr Plus config dirs are absent, project/quick sources should degrade quietly.
No hard failure on missing Herdr Plus.

## Theme implementation is local mapping

Known limitation: Herdr plugin v1 does not provide active theme palette.
Local mapping + custom override is the accepted solution for now.

## Simplicity bias

This project should stay a compact plugin. Avoid speculative abstractions, plugin SDK wrappers, or multi-file refactors unless code size starts blocking safe changes.

## Server access uses remote handoff

Treat a remote server as a Herdr remote target, not a remote session. `Ctrl-S` filters servers; remote rows run `herdr --remote TARGET --handoff` to avoid nested Herdr. Open reads workspaces and tabs from the current local session. Picker should not own SSH config parsing, `.herdr-server.toml`, autossh tabs, or remote terminal attach listing unless terminal-level search becomes an explicit UX goal.

## Integration contract v1

Use a command/JSON list-open contract before building a plugin SDK. This keeps contributor burden low and avoids a speculative framework. Herdr Plus remains built in because it needs Herdr-specific workspace/tab bootstrap behavior.

Helm owns notifications for integration open success/failure so plugin authors only implement list/open.

## Agent search feature shape

The standalone Query token `agent` scopes the flat Result list to agent Entries. `!name` matches an agent name, `@text` matches agent workspace or status text, `/path` matches a path, and `#status` matches status text. `@` without text shows all agent Entries, equivalent to Ctrl-A, using `picker.agent_sort`. Aliases add search terms without changing the visible destination.

Agent rows use Source `pane` or `bookmark`, a status symbol and word, and the destination. Enter focuses the exact Pane ID represented by a pane-backed agent Entry.
