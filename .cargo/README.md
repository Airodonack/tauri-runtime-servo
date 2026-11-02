# .cargo

Cargo settings for builds started in this repository. An app that depends on the crate needs the same settings in its own `.cargo/config.toml`.

| File | What it holds |
|---|---|
| `config.toml` | `RUSTC_BOOTSTRAP` for mozjs and Servo's script crates, which use nightly features on stable. |
