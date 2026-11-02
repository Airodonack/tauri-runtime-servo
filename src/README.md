# src/: the runtime

A library that implements Tauri's `tauri-runtime` traits over a tao event loop and libservo.

## Threads

Tauri's threads hold dispatchers, which only send messages. The main thread runs tao's event loop and owns the GTK windows, the one `Servo` instance and its `WebView`s. A dispatcher call from another thread is posted to the loop as a `Message`; a getter waits for the answer. After every event the loop spins Servo, so Servo only runs on the main thread. An IPC request from a page arrives on Servo's network thread and is posted to the main thread for Tauri's handler.

## Reading order

1. `lib.rs`, the overview.
2. `runtime.rs`, the event loop and the `Message` enum.
3. `webview.rs`, how a webview is made and its Servo delegate.
4. `protocol.rs`, Tauri's IPC and the app's pages.

## Tests

Each file's unit tests sit at its end. They cover the pure conversions: geometry, keys and pointers, URLs, IPC requests, `eval` results. One test in `runtime.rs` needs a display and is `#[ignore]`d; run it with `xvfb-run cargo test -- --include-ignored`. Behaviour that needs a running Servo is covered by the example's [smoke test](../examples/helloworld/README.md#the-smoke-test).

## Files

| File | What it holds |
|---|---|
| `lib.rs` | Crate root, module list, re-exports. Refuses to build off Linux. |
| `runtime.rs` | `ServoRuntime`: the event loop, the messages, the main-thread state. |
| `window.rs` | Windows and `WindowDispatch`, adapted from tauri-runtime-wry. |
| `webview.rs` | Webviews: the Servo `WebView`, its delegate, `eval`, `WebviewDispatch`. |
| `protocol.rs` | Tauri's URI schemes as Servo protocol handlers; app pages as `http://tauri.localhost`. |
| `x11.rs` | The X11 child window Servo paints into, and where it sits below a menu bar. |
| `input.rs` | tao's pointer, wheel and key events as Servo input events. |
| `dialog.rs` | `alert()`, `confirm()` and `prompt()` as GTK dialogs. |
| `monitor.rs` | tao's monitors as Tauri's, adapted from tauri-runtime-wry. |
| `webdriver.rs` | Servo's WebDriver server for tests (`webdriver` feature). |
