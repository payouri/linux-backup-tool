/**
 * Mirrors `crates/gui/src/tree.rs`'s `TreeNodeDto`/`TreeNodeKindDto` and
 * `crates/gui/src/dto.rs`'s `DirectoryStateDto` — the lazy `["tree", prefix]` query
 * response (ADR-0008 Decision 2 "Views out", issue #100). Kept hand-written rather than
 * codegen, same call as `selection-summary.ts` (#99's walking skeleton).
 */
export type DirectoryState = 'all' | 'partial' | 'none'

export type TreeNodeKind = 'category' | 'directory' | 'file'

export interface TreeNodeDto {
  id: string
  name: string
  category: string
  relativePath: string | null
  kind: TreeNodeKind
  state: DirectoryState
  hasChildren: boolean
  addAffordance: boolean
}
