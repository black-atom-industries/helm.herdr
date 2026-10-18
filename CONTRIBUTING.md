# Contributing

Thanks for helping improve Helm for Herdr.

## Development

```bash
mise run git:config
lefthook install
mise run verify:full
```

`mise run format` rewrites formatting. The hooks only check: pre-commit runs the format and lint checks, pre-push runs
`verify:full`, the same pass CI runs.

Keep changes small and user-facing behavior documented in `README.md`.

## Pull requests

- Explain the user-visible change.
- Add or update tests for non-trivial logic.
- Commit through the `repo-commit` skill, which holds the commit format release-please builds `CHANGELOG.md` from.
