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

## What is in this directory

| File | What it holds |
|---|---|
| `lib.rs` | The crate root: the overview, the module list, the public re-exports (`ServoRuntime`, the dispatchers, `servo`, `tao`), and the refusal to build off Linux. |
| `runtime.rs` | `ServoRuntime` and its handle: the tao event loop, the messages dispatchers send, the main-thread state (windows, webviews, the one `Servo`), window events to Tauri and to Servo, the app's data folder. |
| `window.rs` | Windows: Tauri's window builder over tao's, the window messages and `WindowDispatch`, window creation (adapted from tauri-runtime-wry). |
| `webview.rs` | Webviews: a Servo `WebView` on an X11 child window, its delegate (loads, navigation, dialogs, console), initialization scripts, `eval` results as JSON, the webview messages and `WebviewDispatch`. |
| `protocol.rs` | Tauri's URI schemes (`ipc`, `tauri`, `asset`, the plugins') as Servo protocol handlers, and the app's pages served as `http://tauri.localhost` through request interception. |
| `x11.rs` | The webview's native surface: an X11 child window that takes no input, with the visual Servo's EGL config needs. |
| `input.rs` | tao's pointer, wheel and key events converted to Servo input events (after servoshell). |
| `dialog.rs` | The page's `alert()`, `confirm()` and `prompt()` as GTK message dialogs, which WebDriver can also answer. |
| `monitor.rs` | tao's monitors as Tauri's `Monitor`, work areas and centring (adapted from tauri-runtime-wry). |
| `webdriver.rs` | Servo's WebDriver server for end-to-end tests (the `webdriver` feature): starting it and answering the commands it sends the embedder. |
