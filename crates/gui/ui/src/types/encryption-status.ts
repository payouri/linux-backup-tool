/**
 * Mirrors `crates/gui/src/dto.rs`'s `EncryptionStatusDto` — the `["encryption-status"]` query
 * response (issue #101, ADR-0007 D2/D4/D5): Auto-insist plus the gui-side `EncryptionDecision`'s
 * completeness/invalidation. Kept hand-written rather than codegen, same call as the other DTOs
 * (#99's walking skeleton).
 */
export interface EncryptionStatusDto {
  autoInsistRequired: boolean
  sensitiveCategories: string[]
  hasPassphrase: boolean
  isComplete: boolean
  isInvalidated: boolean
}
