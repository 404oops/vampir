# Vendored GPUI-CE

The gallery and the tests build against these copies of the crates.io GPUI-CE packages through `[patch.crates-io]` in the root `Cargo.toml`, because the command bar above a native view needs window ownership that the published backends do not have. The published `vampir` crate excludes this directory, so `[patch]` never reaches a dependent; a host that wants the window form of the command bar patches the same crates in its own manifest.

| Crate | Version | crates.io checksum (sha256 of the `.crate`) |
|---|---|---|
| `gpui-ce` | 0.3.0 | `7691a905fc9b283228c14b9b5ca93d002be88e203aa5878746152210b33c8dbf` |
| `gpui_ce_macos` | 0.3.0 | `3956b3383e204f0455aea6129da9676c4ffbf09351bf685df5ec8c2bb8572d36` |
| `gpui_ce_windows` | 0.3.0 | `2bd1d66220d98bb604541c74c4922ca27576008d03e6266b241d91bc0e17e6f0` |
| `gpui_ce_linux` | 0.3.0 | `e471c10455ec436ba6d9a41f2e08db60576283ccd1f51a844e4d72810a297e28` |

Each directory is the published package with the registry's bookkeeping files (`.cargo-ok`, `.cargo_vcs_info.json`, the generated `Cargo.lock` and `Cargo.toml.orig`) left out. The commit that adds each version changes nothing else, so `git log -p vendor/` shows every local change on its own. What the changes are, and why, is in [`LOCAL_PATCHES.md`](LOCAL_PATCHES.md).
