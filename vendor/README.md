# Vendored GPUI-CE

The gallery and the tests build against these copies of the crates.io GPUI-CE packages through `[patch.crates-io]` in the root `Cargo.toml`, because the command bar above a native view needs window ownership that the published backends do not have. The published `vampir` crate excludes this directory, so `[patch]` never reaches a dependent; a host that wants the window form of the command bar patches the same crates in its own manifest.

| Crate | Version | crates.io checksum (sha256 of the `.crate`) |
|---|---|---|
| `gpui-ce` | 0.2.2 | `b0af79c6659e0fea67773cfbd751fc5c8e51be00139827f365d7d1237a468a4d` |
| `gpui_ce_macos` | 0.1.0 | `af0fe6b615e579ce0180198417815547c1c9ae654dc1e2382299f0c3f3564f79` |
| `gpui_ce_windows` | 0.1.0 | `063326fe043ef3f51b0b36877e8d9f9af0e20d6dfe51efcb275a3cd380128e02` |
| `gpui_ce_linux` | 0.1.0 | `ab947c84c6460f6431c99bd6a8b0954591bfa511c0006515ecc3897e7a903c84` |

Each directory is the published package with the registry's bookkeeping files (`.cargo-ok`, `.cargo_vcs_info.json`, the generated `Cargo.lock` and `Cargo.toml.orig`) left out. The first commit that adds them changes nothing else, so `git log -p vendor/` shows every local change on its own. What the changes are, and why, is in [`LOCAL_PATCHES.md`](LOCAL_PATCHES.md).
