# linux-backup-tool

A Rust desktop tool for backing up and restoring a Linux user's environment — program configs, SSH keys, KDE Plasma layouts, shell setups, user directories, installed-package manifests, and custom paths — with smart interactive defaults and a manifest that makes restore possible.

## Domain model

Read before working on any feature:

- **`CONTEXT.md`** at the repo root — the project's ubiquitous language (glossary). Use these exact terms in code, issues, and tests.
- **`docs/adr/`** — architectural decision records. Read the ones touching your area before changing it, and flag contradictions rather than silently overriding.

## Agent skills

### Issue tracker

Issues live in GitHub Issues (repo `payouri/linux-backup-tool`) via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five canonical roles plus an `epic` label for parent issues. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
