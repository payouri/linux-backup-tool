//! The ten Category definitions (SSH, GPG, Cloud creds, KDE Plasma, Agents, Shell,
//! Program configs, Package lists, User data, Custom paths) and each Category's Curated
//! allowlist of Entries.
//!
//! Category precedence (high to low, for an Entry matching more than one allowlist):
//! SSH · GPG · Cloud-creds -> KDE Plasma -> Agents -> Shell -> Program configs ->
//! Package lists -> User data -> Custom paths.
