//! A Tauri [`Runtime`](tauri_runtime::Runtime) over libservo.
//!
//! Windows are tao windows (GTK 3 on Linux), so everything Tauri and its
//! plugins reach through the native window (menus, dialogs' parent, the GTK
//! window and its vbox) works as with the wry runtime. The webview is a Servo
//! [`WebView`](servo::WebView) painting through a `WindowRenderingContext` on
//! a native child surface the runtime owns: on X11 a child window of the
//! toplevel's, with an empty input region so GTK keeps receiving the pointer
//! and the keyboard; the runtime forwards those events to Servo itself.
//!
//! Use it as `tauri::Builder::<ServoRuntime<tauri::EventLoopMessage>>::new()`
//! with `tauri`'s default features off.
//!
//! The dispatcher and window plumbing (the `Message` enum, the main-thread
//! context, the window builder and the window getters and setters) is adapted
//! from tauri-runtime-wry 2.12.1, Copyright 2019-2024 Tauri Programme within
//! The Commons Conservancy, Apache-2.0 OR MIT. The input conversion follows
//! servoshell's (MPL-2.0).
//!
//! Status: a skeleton. Linux on X11 only; custom URI schemes (and with them
//! Tauri's IPC), initialization scripts and the remaining webview operations
//! return [`tauri_runtime::Error`]s until they are wired.

#[cfg(not(target_os = "linux"))]
compile_error!("tauri-runtime-servo supports Linux (X11) only for now");

mod dialog;
mod input;
mod monitor;
mod runtime;
mod webview;
mod window;
mod x11;

pub use runtime::{EventProxy, Message, ServoRuntime, ServoRuntimeHandle};
pub use webview::{ServoWebviewDispatcher, WebviewMessage};
pub use window::{ServoWindowDispatcher, WindowBuilderWrapper, WindowMessage};

pub use servo;
pub use tao;
