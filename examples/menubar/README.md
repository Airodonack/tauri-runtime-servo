# menubar

A Tauri app with a "File" menu made with Tauri's menu API. The runtime must place the page below the bar and keep it there when the window is resized or the bar is hidden. The page draws a red band along its top edge, so a screenshot shows if anything covers it. Every check prints a `RESULT` line.

## Running it

```sh
export CC=clang CXX=clang++
cargo run --release -p menubar
```

Headless:

```sh
Xvfb :99 -screen 0 1024x768x24 -nolisten tcp &
data=$(mktemp -d)
DISPLAY=:99 GDK_BACKEND=x11 XDG_DATA_HOME=$data/share XDG_CONFIG_HOME=$data/config \
  MENUBAR_CLOSE_AFTER=20 RUST_LOG=warn,console=debug,menubar=info \
  target/release/menubar
```

Drive the menu with `xdotool` or by hand.

## What the RESULT lines check

- `RESULT layout: ...`: where the bar and the page are, and whether they are `clear` or the page `overlaps` the bar. Printed five seconds after start, by the "Check the layout" item, and after the bar is hidden and shown.
- `RESULT menu event: <id>`: a menu item reached the app.
- `RESULT page size WxH`: the page's viewport at load and on resize.
- `RESULT page click at X,Y`: where a click landed in the page.

## Files

| Path | What it holds |
|---|---|
| [`src/`](src/README.md) | The menu, its events, the layout check. |
| [`ui/`](ui/README.md) | The page with the red band. |
| [`capabilities/`](capabilities/README.md) | The window's permissions. |
| `Cargo.toml` | `tauri` without wry, the runtime, `gtk` to read the bar's position. |
| `build.rs` | Runs `tauri-build`. |
| `tauri.conf.json` | Identifier, `frontendDist`, the window. The icon is helloworld's. |
| `gen/` | Written by the build; not in git. |
