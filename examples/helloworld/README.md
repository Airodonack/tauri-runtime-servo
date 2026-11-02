# helloworld

A small Tauri app on `tauri-runtime-servo`. On load, the page runs one check per IPC surface and prints a `RESULT` line for each: on its console from JavaScript, on stdout from Rust.

## Running it

```sh
export CC=clang CXX=clang++
cargo run --release -p helloworld
```

By default the page is served on `http://127.0.0.1:47123/`, the config's `devUrl`. With `--features custom-protocol` it is embedded from `ui/` and served from `tauri://localhost`.

Headless:

```sh
Xvfb :99 -screen 0 1024x768x24 -nolisten tcp &
data=$(mktemp -d)
DISPLAY=:99 GDK_BACKEND=x11 XDG_DATA_HOME=$data/share XDG_CONFIG_HOME=$data/config \
  HELLOWORLD_CLOSE_AFTER=10 RUST_LOG=warn,console=debug,helloworld=info \
  target/release/helloworld
```

`HELLOWORLD_CLOSE_AFTER=<seconds>` closes the window after that long. Software rendering is enough.

## The smoke test

```sh
cargo build --release -p helloworld
examples/helloworld/smoke-test.sh target/release/helloworld
```

Runs the app headless on its own Xvfb display with scratch data folders, then checks that every pattern in `expected-results.txt` matches a `RESULT` line and that the app exits cleanly. It ends with `smoke test: passed` or lists what is missing. CI runs it after every build. It covers the dev-mode build and not the input checks, which need a driver.

## What the RESULT lines check

- the page's origin and size; Tauri's globals in the main frame only; `localStorage` surviving a restart;
- clicks, a `confirm()` dialog and scrolling, when driven;
- the webview's URL as Tauri sees it;
- a sync command, an async one, a failing one, and one with raw bytes and a header;
- an event from Rust every second;
- the clipboard plugin's JavaScript API;
- `eval` from Rust with its result as JSON, including a script that throws and one sent before the page exists.

## Files

| Path | What it holds |
|---|---|
| [`src/`](src/README.md) | The Rust side: commands, page server, window. |
| [`ui/`](ui/README.md) | The page and its checks. |
| [`capabilities/`](capabilities/README.md) | The window's permissions. |
| [`icons/`](icons/README.md) | The app icon. |
| `Cargo.toml` | `tauri` without wry, the runtime, the clipboard plugin. |
| `build.rs` | Runs `tauri-build`. |
| `tauri.conf.json` | Identifier, `devUrl`, `frontendDist`, the window. |
| `smoke-test.sh` | The smoke test. |
| `expected-results.txt` | The `RESULT` lines it requires, one regular expression each. |
| `gen/` | Written by the build; not in git. |
