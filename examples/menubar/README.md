# menubar: a menu bar above the page

A small Tauri app on `tauri-runtime-servo` whose window has a menu bar made with Tauri's menu API (one "File" menu). Tauri packs the bar into the window's GTK box, as it does under wry; the runtime must place the page below the bar, not under it, and keep it there when the window is resized or the bar is hidden or shown. The page draws a red band along its own top edge, so a screenshot shows at once whether anything covers it. Every check prints a `RESULT` line: the page's on its console, Rust's on stdout.

## Running it

From the repository root:

```sh
export CC=clang CXX=clang++
cargo run --release -p menubar
```

The page loads from the app's own `tauri://localhost` (the config has no `devUrl`). Headless, on a virtual display, with the app's data in a scratch folder:

```sh
Xvfb :99 -screen 0 1024x768x24 -nolisten tcp &
data=$(mktemp -d)
DISPLAY=:99 GDK_BACKEND=x11 XDG_DATA_HOME=$data/share XDG_CONFIG_HOME=$data/config \
  MENUBAR_CLOSE_AFTER=20 RUST_LOG=warn,console=debug,menubar=info \
  target/release/menubar
```

`MENUBAR_CLOSE_AFTER=<seconds>` closes the window after that long, so the run ends by itself. Drive the menu with `xdotool` (click "File", then an item) or by hand.

## What the RESULT lines check

- `RESULT layout: ...`: where the menu bar and the page are, in physical pixels from the window's corner, and whether they are `clear` of each other or the page `overlaps` the bar. Printed five seconds after start, by the menu's "Check the layout" item, and one second after the bar is hidden and shown again.
- `RESULT menu event: <id>`: a menu item reached the app (`hello`, `layout`, `hide`).
- `RESULT page size WxH`: the page's viewport, at load and on every resize; it loses the bar's height while the bar is shown.
- `RESULT page click at X,Y`: where a click landed in the page; a click just below the bar reads a Y near 0, so input is offset by the bar too.

"Hide the menu bar for 3 seconds" hides the bar and shows it again, so a run sees the page follow it both ways.

## What is in this directory

| Path | What it holds |
|---|---|
| [`src/`](src/README.md) | The app's Rust code: the menu, the menu events, the layout check. |
| [`ui/`](ui/README.md) | The page (`index.html`): the red band, its size and the clicks it receives. |
| [`capabilities/`](capabilities/README.md) | The main window's permissions. |
| `Cargo.toml` | The app's dependencies: `tauri` without its default features (no wry), the runtime by path, `gtk` to read the menu bar's place. |
| `build.rs` | Runs `tauri-build`, which reads `tauri.conf.json` and the capabilities and writes the generated schemas to `gen/`. |
| `tauri.conf.json` | The app's Tauri config: identifier, `frontendDist`, the window; the icon is helloworld's. |
| `gen/` | Written by the build (Tauri's schemas); not in git. |
