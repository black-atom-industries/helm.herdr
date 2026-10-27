---
name: repo-release
description:
  "Prepare and ship a helm.herdr release. Use this when cutting a version, preparing release notes, or merging the
  release-please PR. Ask for approval before merging the release PR."
disable-model-invocation: true
argument-hint: "[version, for example 0.7.0; defaults to the release PR's version]"
allowed-tools: Bash Read Edit
---

# Release helm.herdr

Follow [`docs/releases.md`](../../../docs/releases.md). It is the maintainer source of truth for the release PR, the curated
release section, and checking the GitHub Release.

Read the version from the release PR title after release-please has finished its run for the latest push to `main`; each
run can change the version. Curate the section on the release branch with [`repo-changelog`](../repo-changelog/SKILL.md).

Record changed Picker behavior as clips with `nbr-record-clip`; tapes live in `docs/assets/tapes/`.

Check the README against the release. When a highlight changes what the README describes or shows, update that text and
its media in the release branch commit. A project with a visible surface keeps a hero clip of its main view at the top of
the README: the same dark/light pair as the highlight, copied to `docs/assets/readme/<topic>-{dark,light}.webp` and
referenced by relative path so it follows `main`, not the tag.

Nothing lands on `main` between curating and merging. Ask for explicit approval immediately before merging the release PR.
