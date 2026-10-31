# helloworld: every IPC surface, checked

A small Tauri app on `tauri-runtime-servo`. Its page greets you, and on load it runs one check per IPC surface a real app relies on, printing a `RESULT` line for each: on the page's console from JavaScript, and on stdout from Rust. It is the runtime's smoke test: if every line reads as expected, Tauri's IPC works through Servo.

## Running it

From the repository root:

```sh
export CC=clang CXX=clang++
cargo run --release -p helloworld
```

By default the page is served on `http://127.0.0.1:47123/` by a small server in `src/main.rs`; that URL is the config's `devUrl`, so Tauri treats the page as the app's own. With `--features custom-protocol` (Tauri's production mode) the page is embedded from `ui/` and loads from `tauri://localhost`.

Headless, on a virtual display, with the app's data in a scratch folder:

```sh
Xvfb :99 -screen 0 1024x768x24 -nolisten tcp &
data=$(mktemp -d)
DISPLAY=:99 GDK_BACKEND=x11 XDG_DATA_HOME=$data/share XDG_CONFIG_HOME=$data/config \
  HELLOWORLD_CLOSE_AFTER=10 RUST_LOG=warn,console=debug,helloworld=info \
  target/release/helloworld
```

`HELLOWORLD_CLOSE_AFTER=<seconds>` closes the window from another thread after that long, as an app would, so the run ends by itself. Software rendering (Mesa's llvmpipe) is enough.

## The smoke test

`smoke-test.sh` does the headless run above and checks it: the app on an Xvfb display of its own (Xvfb picks a free number), with a scratch home and data folders, closing itself after 20 seconds; then every pattern in `expected-results.txt` must match one of the `RESULT` lines it printed, and the app must exit with status 0 within two minutes. CI runs it after the build; it is the first thing to fail when a libservo change breaks the runtime.

```sh
cargo build --release -p helloworld
examples/helloworld/smoke-test.sh target/release/helloworld
```

It ends with `smoke test: passed (...)`, or lists the missing lines and the end of the app's log. `XVFB` names the Xvfb binary if it is not on the `PATH`; `HELLOWORLD_CLOSE_AFTER` and `SMOKE_TIMEOUT` change the two delays; a second argument keeps the app's log at that path. It checks the development-mode build only (the expected lines name the `devUrl`), and not the input lines (clicks, `confirm()`, scrolling), which need a driver.

## What the RESULT lines check

- the page's origin and size, that Tauri's globals exist in the page and not in a hidden iframe (the initialization scripts run in the main frame only), and that `localStorage` survives a restart (a run counter);
- clicks, a `confirm()` dialog and scrolling, when driven (by hand or by a test harness);
- the webview's URL as Tauri sees it (`tauri://localhost/...` in production mode, the `devUrl` otherwise);
- a synchronous command, an async one (run off the main thread), one that fails, and one that takes and returns raw bytes with a header;
- an event emitted from Rust every second;
- the clipboard plugin's JavaScript API (a plugin with its own permissions);
- `eval` from Rust with its result as JSON, for a string, a number, an object, `undefined` and a script that throws, plus one made before the page exists (held until it does).

## What is in this directory

| Path | What it holds |
|---|---|
| [`src/`](src/README.md) | The app's Rust code: the commands, the page server, the window setup. |
| [`ui/`](ui/README.md) | The page (`index.html`) and its IPC checks. |
| [`capabilities/`](capabilities/README.md) | The main window's permissions. |
| [`icons/`](icons/README.md) | The app icon Tauri's build embeds. |
| `Cargo.toml` | The app's dependencies: `tauri` without its default features (no wry), the runtime by path, the clipboard plugin; the `custom-protocol` feature. |
| `build.rs` | Runs `tauri-build`, which reads `tauri.conf.json` and the capabilities and writes the generated schemas to `gen/`. |
| `tauri.conf.json` | The app's Tauri config: identifier, `devUrl` and `frontendDist`, the window. |
| `smoke-test.sh` | The smoke test: runs a built helloworld headless and checks its `RESULT` lines and its exit status. |
| `expected-results.txt` | The `RESULT` lines the smoke test requires, one regular expression each. |
| `gen/` | Written by the build (Tauri's schemas); not in git. |
