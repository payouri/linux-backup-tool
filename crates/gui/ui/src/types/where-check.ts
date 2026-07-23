/**
 * Mirrors `crates/gui/src/dto.rs`'s `WhereCheckDto` — the `["where-check", location]` query
 * response (issue #101): the WHERE step's pre-flight writability/collision check, resolved
 * against the same Backup-dir name `backup()` will later compute.
 */
export interface WhereCheckDto {
  location: string
  backupDir: string
}
