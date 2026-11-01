# src/: how the runtime works

This is the whole runtime: a Rust library that implements Tauri's `tauri-runtime` traits over a tao event loop and libservo. Tauri calls it to make windows and webviews and to send them commands; it answers by driving GTK windows (through tao) and Servo `WebView`s, and by turning Tauri's URI schemes into Servo protocol handlers. The [root README](../README.md) says what the crate is for and how to use it; this one says how the code is laid out.

## The shape of it

Three threads matter. **Tauri's threads** (commands, async tasks, the app's own) hold *dispatchers*, which are `Send + Sync` and only send messages. **The main thread** runs tao's event loop and owns everything that is not thread-safe: the GTK windows, the one `Servo` instance, its `WebView`s and their rendering contexts. **Servo's own threads** (network, constellation, script) call back through the event-loop waker and through the protocol handlers.

A dispatcher call made on the main thread runs at once; from anywhere else it is posted to the loop as a `Message` and, for a getter, waits on a channel for the answer. After every event the loop spins Servo, which calls the webview's delegate (repaints, loads, dialogs), so nothing in Servo runs outside the main thread's loop. An IPC request from a page reaches a `protocol::Forwarder` on Servo's network thread, which posts Tauri's handler to the main thread (where WebKitGTK would call it under wry) and waits for its answer.

## Where to start reading

1. `lib.rs`: the crate's doc comment, the overview in prose.
2. `runtime.rs`: the `Message` enum, the main-thread context, the event loop and how tao events become Tauri's `RunEvent`s; `Context::servo` builds Servo.
3. `webview.rs`: how a webview is made (`create_webview`), its Servo delegate, and the webview messages.
4. `protocol.rs`: Tauri's IPC and the app's `tauri://localhost` pages.
5. The rest as needed: windows, input, the X11 surface, dialogs, WebDriver.

## Tests

Each file's unit tests sit at its end, in a `tests` module. They cover the pure conversions, which run without a display or Servo: window and monitor geometry at several scale factors (`window.rs`, `monitor.rs`), keys, modifiers and pointer points (`input.rs`), the app's URLs, the IPC request, its `Origin` header and the scheme routing (`protocol.rs`), `eval` results as JSON and the initialization-script wrapper (`webview.rs`). The code that calls tao, GTK or libservo stays thin around them. One test in `runtime.rs` builds the runtime and checks the dispatchers' contract in Tauri's terms (unsupported methods return errors, getters on a gone window fail rather than hang); GTK needs a display for it, so it is `#[ignore]`d and run with `xvfb-run cargo test ... -- --include-ignored`. What only a running Servo shows is the example's [smoke test](../examples/helloworld/README.md#the-smoke-test).

## What is in this directory

| File | What it holds |
|---|---|
| `lib.rs` | The crate root: the overview, the module list, the public re-exports (`ServoRuntime`, the dispatchers, `servo`, `tao`), and the refusal to build off Linux. |
| `runtime.rs` | `ServoRuntime` and its handle: the tao event loop, the messages dispatchers send, the main-thread state (windows, webviews, the one `Servo`), window events to Tauri and to Servo, the app's data folder. |
| `window.rs` | Windows: Tauri's window builder over tao's, the window messages and `WindowDispatch`, window creation (adapted from tauri-runtime-wry). |
| `webview.rs` | Webviews: a Servo `WebView` on an X11 child window, its delegate (loads, navigation, dialogs, console), initialization scripts, `eval` results as JSON, the webview messages and `WebviewDispatch`. |
| `protocol.rs` | Tauri's URI schemes (`ipc`, `tauri`, `asset`, the plugins') as Servo protocol handlers, and the app's pages served as `http://tauri.localhost` through request interception; an app page it cannot route, or one Servo loads for no webview (the runtime's Servo delegate), gets an error response instead of a network fetch. |
| `x11.rs` | The webview's native surface: an X11 child window that takes no input, with the visual Servo's EGL config needs; and the page area of the window's GTK box (below a menu bar) that the window's page is placed on. |
| `input.rs` | tao's pointer, wheel and key events converted to Servo input events, with the wheel clicks tao drops caught from GTK. |
| `dialog.rs` | The page's `alert()`, `confirm()` and `prompt()` as GTK message dialogs, which WebDriver can also answer. |
| `monitor.rs` | tao's monitors as Tauri's `Monitor`, work areas and centring (adapted from tauri-runtime-wry). |
| `webdriver.rs` | Servo's WebDriver server for end-to-end tests (the `webdriver` feature): starting it on 127.0.0.1 and answering the commands it sends the embedder. |
