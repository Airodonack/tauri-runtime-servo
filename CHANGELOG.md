# Changelog

What changed in each release of tauri-runtime-servo, for someone building a
Tauri app on it. New behaviour comes first in each entry; fixes are grouped
under the closing "Fixes" bullet.

## Unreleased

- Servo draws the app's windows. First release: Tauri 2.12 on Linux (X11),
  with tao windows.
- Tauri IPC works as on wry. Commands, events, plugins and their
  permissions, the app's `tauri://localhost` pages, initialization scripts
  and `eval` with its result.
- Page dialogs open over the window. `alert()`, `confirm()` and `prompt()`.
- Menu bars sit above the page. The page moves with the bar when it is
  shown or hidden, as on wry.
- Storage and cookies persist per app. They live in its local data folder.
- WebDriver drives apps in test builds. Needs the `webdriver` feature; the
  server answers on 127.0.0.1 only.
- IPC uses each webview's own permissions. A request from an unknown
  webview is refused when several could take it.
- Unhandled app pages show an error. They are never fetched from the
  network.
- Console output lands in debug logs.
- Fixes:
  - Oversized margins no longer crash. A `preventOverflow` margin wider
    than the screen shrinks the window to the room left; debug builds
    used to panic.
  - Works with NVIDIA drivers.
