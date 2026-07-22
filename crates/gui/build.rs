//! `tauri-build` compiles `tauri.conf.json` into the binary and, at compile time, validates
//! that `build.frontendDist` (`ui/dist/`) exists — the walking-skeleton gotcha issue #99
//! flags. `ui/dist/index.html`'s doc comment explains how that's kept satisfied on a clean
//! checkout without running the frontend build first.
fn main() {
    tauri_build::build();
}
