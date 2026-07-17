//! The `manifest.json` schema and (de)serialization via `serde`/`serde_json`: schema/tool
//! version, `created_at`/`host`/`user`/`uid`/`home`/`os` (from `RunContext`), the selected
//! Entries (root, root-tag, Category, sensitivity), a per-file SHA-256 checksum map, a
//! top-level `archive_sha256`, the `vanished` array, and per-manager Package lists.
