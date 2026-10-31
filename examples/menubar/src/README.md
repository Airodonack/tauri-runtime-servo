# menubar/src: the app's Rust side

The whole app is one file. It starts logging, builds the "File" menu with Tauri's menu API, makes the window from `tauri.conf.json` with that menu, answers the menu's items, and checks where the menu bar and the page are. The [menubar README](../README.md) says how to run it and what each `RESULT` line checks.

| File | What it holds |
|---|---|
| `main.rs` | The app: the menu and its events, `report_layout` (the menu bar's GTK allocation against the page's bounds, on the main thread), hiding and showing the bar, and `MENUBAR_CLOSE_AFTER` for headless runs. |
