# Prototype (throwaway): Custom-path autocomplete interaction

> **Throwaway artifact** resolving wayfinder ticket
> [#52](https://github.com/payouri/linux-backup-tool/issues/52) (part of map
> [#45](https://github.com/payouri/linux-backup-tool/issues/45)). This is an *interaction
> storyboard*, not runnable code — the ratatui app it targets does not exist yet (it is the
> destination the map is still speccing). Its job is to make the autocomplete behavior concrete
> enough to react to and decide. The decisions below feed the implementation spec (#53); this file
> is not merged to `trunk`.

## Why a storyboard, not a runnable prototype

A runnable prototype would require standing up the ratatui/crossterm app scaffolding — which is
exactly what is still being specced. So the cheapest artifact that raises discussion fidelity is a
storyboard of annotated terminal frames. Behavior is grounded in the real model: `expand_path`
(`~`/`$VAR`, ADR-0004-clean env resolution via injected closure), `validate_custom_path` (privilege
scope, lexical), and ADR-0005 Decision 6 (core does the directory listing; tui renders it).

## The interaction

```
┌─ Add custom path ──────────────────────────────────────────────┐
│ Path:  ~/Documents▊                                             │   input buffer + ghost-text
│ ┌────────────────────────────────────────────────────────────┐ │   (ghost = greyed top match)
│ │ Documents/            dir              ← selected           │ │   live candidate list,
│ │ Downloads/            dir                                   │ │   recomputed each keystroke
│ │ Docker/               dir →symlink                          │ │   ↑/↓ move · Enter accepts
│ └────────────────────────────────────────────────────────────┘ │
│ scope: under ~ (non-root)     Tab complete · → accept · ↵ add   │   standing scope hint + legend
└────────────────────────────────────────────────────────────────┘

Tab → longest common prefix, one component at a time, stops at each dir's trailing /:
  ~/Do  →Tab→  ~/Do   (list: Documents/ · Downloads/ · Docker/→)   [ambiguous: extends to "Do"]
  ~/Doc →Tab→  ~/Documents/                                        [single dir: adds /, stops]

→ / End  → accept the whole ghost-text suggestion inline (fish-style, eager).

Hidden files — dotfiles appear only once the current segment starts with ".":
  ~/          → visible entries only
  ~/.         → .config/  .local/  .ssh/  …   (reveal-all toggle = optional, #51)

Large directory — type to narrow, never a giant scroll:
┌─ Add custom path ──────────────────────────────────────────────┐
│ Path:  ~/Documents/▊                                            │
│ │ +231 more — keep typing to narrow                           │ │   cap footer
│ │ notes.md              file                                  │ │   files are terminal (Enter adds)
│ │ projects/             dir                                   │ │   dirs first, then files, A–Z
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘

Non-root, out-of-scope input — proactive block, no candidates offered:
┌─ Add custom path ──────────────────────────────────────────────┐
│ Path:  /etc/ssh▊                                                │
│ ⚠ outside ~ — non-root can only add paths under your home       │
│ scope: under ~ (non-root)     Tab complete · → accept · ↵ add   │
└────────────────────────────────────────────────────────────────┘

Root sees the symmetric hint and ranges the whole filesystem:
│ scope: full filesystem (root)                                   │
```

## Decisions (settled with the user)

1. **Trigger/display** — live candidate list **and** inline ghost-text. `Tab` = longest-common-prefix
   / descend on unambiguous single dir; `→`/`End` = accept full ghost suggestion; `↑`/`↓`/`Enter`
   drive the list. Tab is conservative (extend as far as unambiguous), `→` is eager (take the
   suggestion) — no collision.
2. **Completion unit** — component-by-component; Tab commits one path segment and stops at each
   directory boundary with its trailing `/`. No greedy multi-level leap.
3. **Hidden files** — dotfiles hidden until the current segment starts with `.` (shell convention).
   A reveal-all toggle is an optional enhancement for the keymap ticket (#51).
4. **Privilege scope** — proactive. Non-root never lists or descends outside `$HOME`; out-of-scope
   input shows inline `⚠ outside ~` and offers no candidates. Standing scope hint: `under ~
   (non-root)` / `full filesystem (root)`. Root ranges everywhere symmetrically.
5. **File vs directory** — a Custom path may be a **file or a directory**; completion covers both
   (dirs descendable with `/`, files terminal). Completion/add is over **existing paths only**.
6. **Presentation** — type-to-narrow with a `+M more` footer (no giant scroll); directories first
   then files, alphabetical case-insensitive; the literal `~`/`$VAR` stays in the buffer, expansion
   is internal only (candidates shown relative to what was typed).
7. **Symlinks** — marked (`→`) and descendable. The non-root scope check stays **lexical** (matches
   `validate_custom_path`); symlink-based escape (`~/link → /etc`) is a **documented known
   limitation**, with "canonicalize the non-root scope check" flagged as possible future core
   hardening.

## Downstream follow-ups (for the spec #53 / sibling tickets)

- **Core:** drop `create_dir_all` from custom-path resolution — autocomplete/add is existing-paths-only,
  so silently creating an empty directory to "add" a backup source is wrong (Decision 5).
- **Core:** the `list_dir`-style listing this UI consumes is already mandated by ADR-0005 Decision 6;
  this ticket defines what the UI does with it (dirs-first ordering, hidden-file filtering,
  scope-aware pruning happen tui-side over core's listing; the listing itself is core).
- **#51 keymap:** place the reveal-all-dotfiles toggle if a chord is free (Decision 3).
- **#50 layout:** the candidate-list pane height determines the cap `N` (Decision 6).
- **Future core hardening (optional):** canonicalizing the non-root scope check to close the symlink
  escape (Decision 7).
