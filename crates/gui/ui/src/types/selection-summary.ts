/**
 * Mirrors `crates/gui/src/dto.rs`'s `SelectionSummaryDto` — the `["selection-summary"]`
 * query response. Kept as a hand-written type rather than codegen for this walking
 * skeleton (#99); a shared schema/codegen step is a candidate for a later build if the DTO
 * surface grows.
 */
export interface SelectionSummaryDto {
  entryCount: number
  totalSizeBytes: number
  isEmpty: boolean
  encryptionRequired: boolean
  warnings: string[]
}
