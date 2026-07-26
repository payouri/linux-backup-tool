# Copy catalog tooling: one source file, Rust + TypeScript, compile-time keys

Research resolving ticket [#136](https://github.com/payouri/linux-backup-tool/issues/136)
(part of map [#135](https://github.com/payouri/linux-backup-tool/issues/135)).
Scope: survey the 2026 Rust and TypeScript landscape for a **single cross-language copy
catalog** — one source file, codegen to TS, compile-time key checking on both sides — and rank
the options.

Placement ([#137](https://github.com/payouri/linux-backup-tool/issues/137)), key naming
([#140](https://github.com/payouri/linux-backup-tool/issues/140)), the formatting layer
([#139](https://github.com/payouri/linux-backup-tool/issues/139)) and the end-to-end prototype
([#141](https://github.com/payouri/linux-backup-tool/issues/141)) are sibling tickets. This
note supplies the facts they rest on and deliberately does not decide them.

Crate and package facts confirmed against crates.io, docs.rs, the npm registry API and the
projects' own repositories on **2026-07-26**. The prototype measurements were run locally the
same day against this workspace's own toolchains — `rustc 1.97.1`, `rustc 1.95.0`,
`tsc 7.0.2`, `git 2.54.0`, `node v26.2.0`.

## Short answer

**Yes — the honest recommendation is "no library, ~80 lines of codegen."** It is not a
strawman and it is not close.

Every i18n crate and package surveyed either

- resolves keys at **runtime** and returns a fallback on a typo — failing the ticket's first
  hard requirement outright (`rust-i18n`, `fluent-bundle`, `phf`, `@formatjs`), or
- buys compile-time keys only by taking a **locale runtime, a config directory, a Babel/SWC
  transform, or an extractor CLI that owns the pipeline** — failing requirement 4, and in two
  cases failing this workspace's actual toolchain (`i18n-embed-fl`, `typesafe-i18n`, Lingui,
  `paraglide-js`), or
- solves only **half the problem**, leaving the other language uncovered anyway
  (`i18n-embed-fl` is Rust-only; `ts-rs`/`specta` emit types, never values).

The measured generator is **77 non-blank lines**, uses one build-dependency the workspace
already carries (`toml`), is green on `cargo +1.95.0`, regenerates a 200-entry catalog in
~50 ms, and makes a typo'd key a hard error on **both** sides — verified, not asserted.

## What the requirements actually filter on

Restating the ticket's constraints as the filters that do all the work:

1. **Compile-time key checking on both sides.** Not "typed keys" in the marketing sense: a
   typo must fail `cargo build` *and* `tsc -b`. This eliminates nearly the whole i18n field,
   for a structural reason — i18n libraries exist to tolerate a key that resolves in one
   locale and not another. A missing key **cannot** be a compile error in a library whose job
   is graceful degradation.
2. **No ambient reads, `core`-compatible** (ADR-0004, ADR-0001, ADR-0005/0008). Compile-time
   embedding or explicit injection only; UI-free; synchronous.
3. **MSRV 1.95.0**, gated by CI's `cargo +1.95.0 check`.
4. **Locale machinery is a cost, not a feature** (map standing decision 1). English only.

Two more filters are not in the ticket but fall out of this repository, and both are decisive.
They are the parts a generic survey cannot give you:

5. **This frontend is not a normal 2026 React frontend.** Locked versions:
   `typescript@7.0.2` (the native compiler), `vite@8.1.5`, `@vitejs/plugin-react-swc@4.3.2`
   (**SWC, not Babel**), `oxlint@1.75.0` (**not ESLint**), `react@19.2.7`, with
   `erasableSyntaxOnly`, `verbatimModuleSyntax` and `noUnusedLocals` on
   (`crates/gui/ui/package-lock.json`, `crates/gui/ui/tsconfig.app.json`).
6. **The Flatpak builds the frontend *before* cargo.** See
   [Build ordering](#build-ordering-the-constraint-a-generic-survey-cannot-find).

## Rust candidates

| Crate | Latest (date) | MSRV | Health | Compile-time key check? | Runtime disk/locale reads? | Verdict |
|---|---|---|---|---|---|---|
| **`rust-i18n`** | 4.2.1 (2026-07-16) | **not declared**, edition 2021 | 891k recent dl; last push 2026-07-22 | **No** — `t!()` expands to `_rust_i18n_try_translate(...).unwrap_or_else(...)`; a miss is a runtime fallback (optional `log-miss-tr` feature only *logs* it) | Embeds at build time by default; `load-path` feature adds runtime loading. Locale set manually via `set_locale()` | **Out** — fails filter 1. Also depends on the archived `serde_yaml 0.9.33` |
| **`fluent-bundle`** (fluent-rs) | 0.16.0 (2025-05-22) | 1.67.0 | 4.0M recent dl; repo last push 2026-03-27 | **No** — `get_message()` returns `Option`, resolved at runtime | No I/O itself, but **`unic-langid` + `intl_pluralrules` are non-optional core deps** | **Out** — fails filters 1 and 4 |
| **`i18n-embed` + `i18n-embed-fl`** | 0.16.0 (2025-07-09) / 0.10.1 (2026-07-23) | **not declared**, edition 2018 | ~1.5M recent dl each; last push 2026-07-23 | **Yes** — `fl!()` checks the message id *and* argument names against the fallback-language `.ftl` at macro-expansion time. The only surveyed crate that genuinely does this | Proc-macro reads `.ftl` from disk **at compile time** (fine). `desktop-requester`/`web-sys-requester` add OS/browser locale probing — opt-in, but it is the crate's headline feature | **Weak** — passes filter 1, but requires an `i18n.toml`, a declared `fallback_language`, `.ftl` files and `unic-langid`; **and it gives TypeScript nothing at all** |
| **`phf` / `phf_codegen`** | 0.14.0 (2026-06-21) | 1.85 | 117M recent dl; last push 2026-07-24 | **No** — the *hash structure* is built at compile time; `.get()` is still a runtime `Option` | None | **Out** — fails filter 1. It is a fast map, not a checked namespace; a checked namespace needs no map |
| **`ts-rs` / `specta` / `tsify`** | 12.0.1 / 2.0.0-rc.25 / 0.5.6 | 1.78 / — / — | — | n/a | n/a | **Out** — all three emit TypeScript **type** declarations from Rust types. None emits a value/constant module. `tsify` additionally requires `wasm-bindgen` |
| **Plain codegen, no crate** | — | inherits workspace 1.95 | — | **Yes** — a key is an item path; a typo is `E0425`, a wrong param count is `E0061` | None; `include!(concat!(env!("OUT_DIR"), …))` is compile-time embedding | **Best fit** |

The `i18n-embed-fl` result deserves the emphasis: it *is* the crate that does what the ticket
asks, on the Rust side. It is still not the answer, because it solves half the problem at the
price of the whole Fluent stack — `i18n.toml`, a fallback-language declaration, `.ftl` syntax,
`unic-langid`, and `rust-embed` — and the TypeScript half remains entirely unwritten. Paying a
localization framework's setup cost and then still hand-rolling the TS generator is strictly
worse than hand-rolling both halves of one generator.

## TypeScript candidates

| Package | Latest (date) | Weekly dl | Health | Typo'd key fails `tsc`? | Params typed? | Toolchain fit here | Verdict |
|---|---|---|---|---|---|---|---|
| **`typesafe-i18n`** | 5.27.1 (2026-02-11) | 52k | **Caretaker.** Original author died in 2023 ([ivanhofer/typesafe-i18n#739](https://github.com/ivanhofer/typesafe-i18n/issues/739)); community-adopted at `codingcommons/typesafe-i18n`, ~7 mostly-chore commits in 3 months | **Yes** | **Yes**, per-locale | Owns its **own generator** reading its **own base-locale file**; it does not consume a `.ts` module produced by an external tool. Its typing rests on heavy template-literal types — the category `microsoft/typescript-go#2830` reports tsgo diverging on | **Out** — incompatible generator model (fails "one source, our codegen"), plus a TS 7 risk and thin maintenance |
| **Lingui** (`@lingui/core` etc.) | 6.6.0 (2026-07-24) | 1.44M | Healthy, active | **n/a** — Lingui has no key. Messages are JSX (`<Trans>`); tsc checks them as ordinary TSX | Yes, incidentally (they are real TSX expressions) | Needs a macro transform: Babel, or `@lingui/swc-plugin`, which **Lingui's own docs label experimental** because the SWC plugin ABI is not semver-stable and must match `@swc/core` exactly. No published matrix entry ties `@lingui/swc-plugin@6.6.0` to the `@swc/core ^1.15.46` that `@vitejs/plugin-react-swc@4.3.2` pins | **Out** — its model is "extract copy *from* source", the exact inverse of a single external catalog. Toolchain fit is a live gamble |
| **FormatJS** (`react-intl`, `@formatjs/*`) | `react-intl` 10.1.18 (2026-07-17) | 3.37M | Healthy, active | **No** by default — `id` is plain `string`. Typed keys require a `react-intl.d.ts` you write by hand; `@formatjs/cli` does not generate one | **No** — [formatjs#3346](https://github.com/formatjs/formatjs/issues/3346) is open: variable/type mismatches are not caught at build time and render `NaN` | **Babel only.** No SWC or Vite-native plugin from the project | **Out** — fails filter 1 and the typed-params requirement, and needs a bundler transform this repo does not run |
| **`i18next` + `react-i18next`** | 26.3.6 / 17.0.11 | 20.3M / 14.3M | Largest install base surveyed. **The only package here with confirmed TypeScript 7 fixes** — peer range widened to `^5 \|\| ^6 \|\| ^7`, plus a tsgo-specific `<Trans>` inference fix in `react-i18next@17.0.9` | **Yes, but only** when resources are declared as a TS `as const` object or a `.d.ts` — runtime-loaded JSON gets nothing | Partially; interpolation typing has known gaps ([react-i18next#1423](https://github.com/i18next/react-i18next/issues/1423), [#1547](https://github.com/i18next/react-i18next/issues/1547)) | Fine — no Babel, no ESLint dependency | **Redundant.** Its typed path *is* "generate an `as const` TS module". Once you have generated that module, i18next adds a full i18n runtime that buys you nothing |
| **`@inlang/paraglide-js`** | 2.23.0 (2026-07-26) | 478k | Active (2,307 commits, 30 open issues); named as typesafe-i18n's spiritual successor in that project's own farewell issue | **Yes** — messages compile to **typed ESM functions**, so a typo is a missing import. No enums or namespaces in its output (`erasableSyntaxOnly`-safe) | **Yes** — one function signature per message | Requires `project.inlang/settings.json` + its own message-file format + `paraglideVitePlugin`; it **owns the pipeline** and will not consume a foreign `.ts`. Docs cite TypeScript 5.6+; **no statement found about TS 7 / tsgo, Vite 8, or React 19** | **Runner-up** — closest in *shape* to what we would generate. See the hybrid below |
| **Plain generated `.ts`, no library** | — | — | — | **Yes** — verified below: `TS2551` on a typo'd key | **Yes** — `TS2561` on a wrong param, `TS2741` on a missing one | Zero deps, zero plugins, zero runtime | **Best fit** |

Two cross-cutting notes worth recording:

- **ESLint plugins are unavailable to us regardless.** Lingui and FormatJS both ship
  `eslint-plugin-*` for missing-message and ICU-syntax linting. This repo runs **oxlint**, so
  that layer is off the table whichever library wins. Neither library's compile-time safety
  depended on it, so nothing is lost — but nothing is gained either.
- **Paraglide is the shape, not the product.** Its output — one typed function per message,
  no lookup runtime — is exactly what ~40 lines of TS emission produce. It reaches that shape
  through an `inlang` project directory, a Vite plugin, and a compiler whose TS 7 / Vite 8
  story is undocumented. `strategy: ["baseLocale"]` disables locale detection but does not
  remove the `project.inlang` requirement. That is filter 4 in its purest form: mandatory
  locale machinery, opt-out only at the runtime edge.

## Formats

The format question has a twist that reframes it, so take the twist first.

**If a single generator emits both outputs, Node never parses the catalog at all** — the
format only has to survive a Rust reader, and every candidate qualifies. The format question
only *bites* under the two-reader design, where a Rust build script and a Node build step each
parse the same file independently. [Build ordering](#build-ordering-the-constraint-a-generic-survey-cannot-find)
argues that the two-reader design is the one that actually works here, so the format question
is live after all.

| Format | Rust reader | Node reader | Comments? | 200-entry PR diff | Verdict |
|---|---|---|---|---|---|
| **TOML** | `toml` **1.1.3** (2026-07-14), MSRV 1.85, 178M recent dl — **already a `core` dependency** | `smol-toml` **1.7.1** (2026-07-26), **zero deps**, 25.5M weekly dl | **Yes**, `#` — and the house style (CODING_STANDARDS.md) is dense explanatory comments | Best. One stanza per key, and a `.gitattributes` `diff=toml` driver with `xfuncname = ^\[.*\]$` puts the **enclosing key in every hunk header** (verified: `@@ -7 +7 @@ [a.two]`) | **Recommended** |
| **JSON** | `serde_json` **1.0.151**, MSRV 1.71 — already a `core` dependency | Native `JSON.parse` / `import … with { type: 'json' }`; **no dependency at all** | **No.** Comments must be smuggled into a `note` field | One line per entry keeps the key visible in the diff; pretty-printed multi-line entries lose it (verified) | **Viable fallback** — the zero-parser property is real, the no-comments property is the cost |
| **YAML** | `serde_yaml` is **archived and self-deprecated** (`0.9.34+deprecated`, 2024-03-25; the README says "no longer maintained"). Forks `serde_yaml_ng` (last push 2025-09-14) and `serde_norway` (2025-08-04) are both ~1 year stale | `yaml` 2.9.0, 180M weekly dl — healthy | Yes, `#` | Fine, but whitespace-significant and quoting-sensitive for a file that is *nothing but* strings | **Out** — the Rust half has no maintained parser |
| **RON** | `ron` **0.12.2** (2026-06-22), MSRV 1.64, healthy | **None.** The npm `ron` package is an unrelated 2012 artifact with 46 weekly downloads | Yes | Rust-shaped, unfamiliar to a frontend reviewer | **Out** — forces the single-generator design by elimination, for no benefit |
| **Fluent `.ftl`** | `fluent-syntax` 0.12.0, healthy | `@fluent/syntax` 0.19.0, **last published 2023-03-13** | Yes, `#` | Purpose-built for translator review — genuinely good at this | **Out on filter 4** — adopting Fluent syntax adopts Fluent's select/plural machinery, which the map explicitly does not want, and drags a stale JS parser in |

## Build ordering: the constraint a generic survey cannot find

`crates/gui/flatpak/io.github.payouri.linux-backup.yml` runs, in this order:

```
npm ci --offline … --prefix crates/gui/ui
npm run build --prefix crates/gui/ui        # tsc -b && vite build
cargo build --release --offline --manifest-path crates/gui/Cargo.toml
```

The frontend build **must** come first — `tauri-build` validates at Rust compile time that
`build.frontendDist` exists. The whole thing runs offline in a sandbox against vendored
`cargo`/`npm` caches.

That kills one design outright and constrains the rest:

- **A `build.rs` that writes the `.ts` into the source tree is broken here.** At `npm run
  build` time no cargo build has run, so the file does not exist. It also violates cargo's own
  contract — the Cargo Book: *"Build scripts may save any output files or intermediate
  artifacts in the directory specified in the `OUT_DIR` environment variable. **Scripts should
  not modify any files outside of that directory.**"*

That leaves two workable mechanisms:

- **M1 — two independent readers.** `build.rs` reads the catalog and writes `.rs` to
  `OUT_DIR`; an npm `prebuild` script reads the *same* catalog and writes a gitignored `.ts`
  into `crates/gui/ui/src/`. Neither build waits on the other, in any order, in any of the
  five CI jobs. Cost: the emission logic exists twice (~50 lines of Rust, ~40 of JS) and the
  catalog format must have a Node parser. `cargo build --workspace` on a clean checkout stays
  green with no `npm install`, which is exactly the constraint that produced the tracked
  `crates/gui/ui/dist/index.html` placeholder.
- **M2 — one generator, both outputs committed.** A `cargo xtask` emits `.rs` and `.ts`; both
  are tracked; a `git diff --exit-code` drift check folds into an existing CI job. Cost: diff
  noise on every copy edit, and a regeneration step a contributor can forget — the drift check
  is what makes that recoverable rather than silent. Format is unconstrained (RON becomes
  viable).

M1 and M2 are the real fork in #141's road. M1 is the recommendation here, because it is the
only one where a stale artifact is *impossible* rather than *detected*.

## The measured prototype

Built and run at
`…/scratchpad/proto` on 2026-07-26. Catalog in TOML, generator in `build.rs`, emitting Rust to
`OUT_DIR` and TypeScript beside it.

Catalog:

```toml
["err.backup_dir_exists"]
text = "A backup already exists at {path}."
note = "Shown when the target Backup dir is occupied; the run is refused, never merged."

["run.progress_files"]
text = "{done} of {total} files"
```

Generated Rust (`include!`d into a module):

```rust
pub mod err {
    /// Shown when the target Backup dir is occupied; the run is refused, never merged.
    pub fn backup_dir_exists(path: impl std::fmt::Display) -> String { format!("A backup already exists at {path}.") }
}
```

Generated TypeScript:

```ts
export const err = {
  /** Shown when the target Backup dir is occupied; the run is refused, never merged. */
  backup_dir_exists: (p: { path: string }): string => `A backup already exists at ${p.path}.`,
} as const;
```

Measurements, all first-hand:

| Question | Answer |
|---|---|
| Generator size | **77 non-blank lines** (83 with blanks). The ticket's "~80 lines" estimate is accurate to within rounding |
| Dependencies added | `toml` + `serde` as **build-dependencies**. Both are already normal dependencies of `core` |
| MSRV | `cargo +1.95.0 test` **green** |
| Typo'd key, Rust | `error[E0425]: cannot find function 'backup_dir_existz' in module 'crate::copy::err'` |
| Wrong arity, Rust | `error[E0061]: this function takes 1 argument but 2 arguments were supplied` |
| Typo'd key, TS 7.0.2 | `error TS2551: Property 'backup_dir_existz' does not exist on type … Did you mean 'backup_dir_exists'?` |
| Wrong param name, TS | `error TS2561: … 'pathh' does not exist in type '{ path: string; }'. Did you mean to write 'path'?` |
| Missing param, TS | `error TS2741: Property 'path' is missing in type '{}' but required in type '{ path: string; }'` |
| Clean case, TS | `tsc -p … --noEmit` exits **0** under the repo's own `strict` + `erasableSyntaxOnly` + `verbatimModuleSyntax` flags |
| 200-entry catalog | Generates 415 lines of `.rs` and 415 of `.ts`; touch-catalog rebuild **~52 ms** (no-op build ~24 ms) |

Three things the prototype surfaced that a paper survey would not, all of them input to #141:

- **`pub const what_title` trips `non_upper_case_globals`.** Generated code is real crate code,
  and CI runs `clippy -- -D warnings`, which promotes rustc warnings. Either emit
  `SCREAMING_SNAKE` consts or — cleaner — emit **every** entry as a function, so parameterless
  and parameterized copy have one uniform call shape.
- **`impl Display` is looser than it looks.** `backup_dir_exists(42)` compiles. Tightening
  needs a per-param type annotation in the catalog (`path = "path"` → `&Path`), which is a
  catalog-schema decision, not a tooling one. The TS side already types every param `string`,
  so the two sides are *not* symmetric by default. Worth naming in #140.
- **Doc comments come free.** A `note` field becomes a Rust `///` and a TS `/** */`, so the
  reviewer context #135 leaves open is a two-line change, not a format migration. This is
  reason enough to prefer a format that also has real comments.

## Ranked recommendation

1. **No library. One TOML catalog, ~80 lines of codegen, mechanism M1 (two independent
   readers).** Compile-time keys and typed params on both sides — verified, not claimed.
   Zero new runtime dependencies in `core`, `tui`, `gui`, or `gui/ui`. No locale machinery to
   opt out of. No Babel, no SWC plugin, no extractor, no config directory, no `.d.ts` to
   hand-maintain. Nothing to break when `typescript` goes to 7.1 or `@swc/core` bumps its
   plugin ABI. The workspace already generates `crates/gui/gen/` on every build, so build-time
   codegen is an established pattern here rather than a new one — though note honestly that
   that precedent is a *third-party* build script (`tauri-build`), not hand-written codegen,
   so this would be the first of its kind in the tree.
2. **Paraglide-js for the TS half, `build.rs` for the Rust half, over a shared JSON message
   file.** The only arrangement where a library genuinely replaces work: paraglide's output
   shape is what we would generate anyway. Ranked second, not first, because it costs a
   `project.inlang/` config directory, a Vite plugin, and an npm dependency tree, in exchange
   for ~40 lines of TypeScript emission — and because its docs say nothing about TypeScript 7,
   Vite 8, or React 19, which is a live risk on this particular frontend. It also forces the
   catalog to be JSON in paraglide's own message format, losing comments. Revisit only if the
   TS emitter turns out harder than measured.
3. **`i18next` over a generated `as const` resources module.** Coherent, and the only surveyed
   package with *confirmed* TS 7 fixes. But its typed-key path already requires the generated
   module recommendation 1 produces, and then adds a full i18n runtime — detector, backend,
   formatter — on top of it. Interpolation typing has open gaps. Take it only if the project
   later decides it wants a real i18n runtime after all.
4. **`i18n-embed-fl` on the Rust side.** The one crate that truly does compile-time key and
   argument checking. Rejected because it covers only Rust, mandates `i18n.toml` + a
   fallback-language + `.ftl` + `unic-langid`, and declares no MSRV (edition 2018) — so
   nothing stops a future release from crossing 1.95.0 silently.
5. **`typesafe-i18n`.** Would satisfy the type requirements, but its generator owns the source
   file, which contradicts "one cross-language source". Add a caretaker-maintenance signal and
   a template-literal-type approach in the exact category tsgo is known to diverge on.
6. **Disqualified outright:** `rust-i18n`, `fluent-bundle`, `phf` (runtime lookup, filter 1);
   `@formatjs` (untyped params, Babel-only); Lingui (no key model; extraction is the inverse
   of a catalog; experimental SWC plugin); `ts-rs`/`specta`/`tsify` (types, not values).

**Format:** TOML, with a `.gitattributes` `diff=toml` driver so hunk headers carry the key.
JSON is the fallback if a Node TOML dependency is judged not worth it — accepting that a
comment then has to be a `note` field.

**What this note does not decide:** where the catalog lives and who depends on it (#137), the
key naming scheme (#140), whether the generated TS is tracked or gitignored (#141), and the
formatting layer (#139). M1 vs M2 above is evidence for #141, not a ruling.

## Does `docs/research/rust-crates.md` need a superseded-note update?

**No.** Its existing superseded block is accurate and already scoped correctly — it reverses
the `inquire`/`ratatui` call, corrects the `secrecy` pin, and points at each `Cargo.toml`'s
dependency comments as the live record. Nothing in this survey touches its recommended set:
no crate it names is implicated in the copy catalog, and this note adds a *new* concern rather
than revising an old verdict.

One thing is worth adding to it only if someone is editing it anyway: its note says
"serde-first pattern", and `serde_yaml` — which that pattern would have reached for — is now
archived and self-deprecated. That is a fact about the ecosystem, not a correction to its
recommendations, so it does not on its own justify touching an append-only-in-spirit document.

This note deliberately does **not** reuse `rust-crates.md`'s "recommended set + runners-up"
table shape, per #136. It is organised by *requirement filter*, because here the filters —
not the crate quality — are what decide the outcome.

## A naming warning for #140

"Catalog" is already spoken for, twice. `core::catalog` is the ten Category definitions and
their Curated allowlists (`crates/core/src/catalog.rs`), and `CONTEXT.md` lists "catalog" under
**Avoid** for the Manifest. Whatever #135 ships, `core::catalog` and a copy catalog cannot both
be called that. Worth resolving in `CONTEXT.md` before the term hardens in code.

## Sources

Verified 2026-07-26.

**Rust**
- crates.io API `/api/v1/crates/{rust-i18n,fluent-bundle,i18n-embed,i18n-embed-fl,phf,phf_codegen,toml,serde_json,ron,serde_yaml,serde_yaml_ng,serde_norway,prettyplease,quote,ts-rs,specta,tsify,fluent-syntax}`
- [longbridge/rust-i18n](https://github.com/longbridge/rust-i18n) — `crates/macro/src/lib.rs`, `Cargo.toml`, README
- [projectfluent/fluent-rs](https://github.com/projectfluent/fluent-rs) — workspace `Cargo.toml` (`rust-version = "1.67.0"`), `fluent-bundle/Cargo.toml`
- [kellpossible/cargo-i18n](https://github.com/kellpossible/cargo-i18n) — `i18n-embed/Cargo.toml`, `i18n-embed-fl/Cargo.toml` and README (the compile-time-check claim)
- [rust-phf/rust-phf](https://github.com/rust-phf/rust-phf) README (MSRV 1.85, `get()` usage) and [docs.rs/phf](https://docs.rs/phf/latest/phf/)
- [dtolnay/serde-yaml](https://github.com/dtolnay/serde-yaml) — archived; README "This project is no longer maintained."
- [The Cargo Book — Build Scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html) — `OUT_DIR`, "Scripts should not modify any files outside of that directory", `rerun-if-changed`

**TypeScript / npm**
- npm registry + `api.npmjs.org/downloads/point/last-week` for every package named above
- [ivanhofer/typesafe-i18n#739](https://github.com/ivanhofer/typesafe-i18n/issues/739) — maintenance status and the Paraglide hand-off; repo now `codingcommons/typesafe-i18n`
- [lingui.dev/installation](https://lingui.dev) and [lingui.dev/tools/swc-plugin](https://lingui.dev/tools/swc-plugin) + [lingui/swc-plugin#179](https://github.com/lingui/swc-plugin/issues/179) — the "experimental / ABI-locked" caveat
- [formatjs#3346](https://github.com/formatjs/formatjs/issues/3346) — params not type-checked at build time; [formatjs.github.io/docs/react-intl](https://formatjs.github.io/docs/react-intl) — manual `.d.ts` for typed ids
- i18next / react-i18next release notes — `typescript ^5 || ^6 || ^7` peer range, `react-i18next@17.0.9` tsgo `<Trans>` fix; [react-i18next#1423](https://github.com/i18next/react-i18next/issues/1423), [#1547](https://github.com/i18next/react-i18next/issues/1547)
- [paraglidejs.com](https://paraglidejs.com) and [opral/paraglide-js](https://github.com/opral/paraglide-js) — compiled typed message functions, `project.inlang/settings.json`, `strategy: ["baseLocale"]`
- [microsoft/typescript-go#2830](https://github.com/microsoft/typescript-go/issues/2830) — tsgo diverging on deeply nested template-literal/conditional types

**This repository**
- `crates/gui/flatpak/io.github.payouri.linux-backup.yml` (build ordering), `crates/gui/ui/package-lock.json` (locked toolchain versions), `crates/gui/ui/tsconfig.app.json`, `crates/core/Cargo.toml`, `crates/gui/src/error.rs` (`kind_tag`), `CONTEXT.md`, `CODING_STANDARDS.md`
- Local runs: `cargo +1.95.0 test`, `tsc -p … --noEmit` with `crates/gui/ui/node_modules/.bin/tsc` (7.0.2), `git diff -U0` with a `diff.toml.xfuncname` driver (git 2.54.0)

**Not verified** (flagged rather than asserted): exact `@lingui/swc-plugin@6.6.0` ↔
`@swc/core@^1.15.46` compatibility (no published matrix); whether `typesafe-i18n`'s generator
ever emits a literal `namespace`; any TS 7 / tsgo statement from typesafe-i18n, Lingui,
FormatJS or paraglide-js; crates.io's exact "recent downloads" window.
