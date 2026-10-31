# .cargo: Cargo configuration for this workspace

Cargo reads `.cargo/config.toml` from the directory a build starts in and its parents, never from dependencies, so this one applies to builds of this repository (the crate and its example). An app that depends on the crate needs the same setting in its own `.cargo/config.toml`; the [root README](../README.md#using-it-in-a-tauri-app) shows it.

| File | What it holds |
|---|---|
| `config.toml` | `RUSTC_BOOTSTRAP` for mozjs and Servo's script crates, which use nightly features on the stable toolchain (the Servo fork's own setting). |
