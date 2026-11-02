//! A Tauri [`Runtime`](tauri_runtime::Runtime) over libservo.
//!
//! Windows are tao windows (GTK 3 on Linux), so menus, dialogs and anything a
//! plugin reaches through the native window work as with wry. The webview is
//! a Servo [`WebView`](servo::WebView) painting into a child window of the
//! toplevel. The child takes no input; the runtime forwards GTK's pointer and
//! key events to Servo.
//!
//! Use it as `tauri::Builder::<ServoRuntime<tauri::EventLoopMessage>>::new()`
//! with `tauri`'s default features off.
//!
//! Tauri's IPC works as on wry: the `ipc`, `tauri`, `asset` and plugin
//! schemes are Servo protocol handlers that call Tauri's handlers (see
//! `protocol`), initialization scripts are Servo user scripts, and `eval`
//! returns its result as JSON.
//!
//! With the `webdriver` feature, the runtime starts Servo's WebDriver server
//! on the port named by `TAURI_RUNTIME_SERVO_WEBDRIVER_PORT`
//! ([`WEBDRIVER_PORT_VARIABLE`]), so end-to-end tests drive the app without
//! tauri-driver. The server listens on the loopback interface and runs any
//! script it is sent: test builds only.
//!
//! Linux on X11 only. What Servo cannot do yet returns a
//! [`tauri_runtime::Error`].
//!
//! The window and dispatcher code is adapted from tauri-runtime-wry 2.12.1,
//! Copyright 2019-2024 Tauri Programme within The Commons Conservancy,
//! Apache-2.0 OR MIT.

#[cfg(not(target_os = "linux"))]
compile_error!("tauri-runtime-servo supports Linux (X11) only for now");

mod dialog;
mod input;
mod monitor;
mod protocol;
mod runtime;
mod webdriver;
mod webview;
mod window;
mod x11;

// The runtime and the types its trait implementations name. The messages
// between dispatchers and the event loop stay inside the crate: they are
// how it works, not what it offers.
pub use runtime::{EventProxy, ServoRuntime, ServoRuntimeHandle};
pub use webdriver::PORT_VARIABLE as WEBDRIVER_PORT_VARIABLE;
pub use webview::ServoWebviewDispatcher;
pub use window::{ServoWindowDispatcher, WindowBuilderWrapper};

pub use servo;
pub use tao;
