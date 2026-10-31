# Changelog

What changed in each release of tauri-runtime-servo, for someone building a
Tauri app on it. New behaviour comes first in each entry; fixes are grouped
under the closing "Fixes" bullet.

## Unreleased

- First release: a Tauri 2.12 runtime that draws the app's windows with
  Servo on Linux (X11), with tao windows.
- Tauri's IPC works as on wry: commands, events, plugins and their
  permissions, the app's own `tauri://localhost` pages, initialization
  scripts and `eval` with its result.
- The page's `alert()`, `confirm()` and `prompt()` open as dialogs over the
  window.
- The page's storage and cookies are kept in the app's local data folder.
- End-to-end tests can drive an app over WebDriver in builds with the
  `webdriver` feature; the WebDriver server answers on this machine only
  (127.0.0.1), not from the network.
- A page's IPC requests are answered with its own webview's permissions,
  and refused when the webview they came from is unknown and several could
  take them.
- An app page the runtime can't hand to the app gets an error page; it is
  never fetched from the network instead.
- The page's console appears in the log at debug level only.
- Works with NVIDIA's drivers as with Mesa's.
