# linux-backup-tool

An interactive Rust tool for backing up and restoring a Linux user's environment.

Backs up program configs, SSH keys, KDE Plasma layouts, shell configs and plugins,
user directories, installed-package manifests, and user-supplied custom paths — with
smart defaults you confirm interactively, a chosen output location on any writable
drive, and a manifest that records what was backed up so it can later be restored.

> 🚧 Early development. Design and planning are tracked as GitHub issues via
> [`/wayfinder`](docs/agents/issue-tracker.md).
