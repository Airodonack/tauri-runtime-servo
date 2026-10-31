# helloworld/ui: the page

The page the app shows, plain HTML and JavaScript with no build step (`withGlobalTauri` gives it `window.__TAURI__`). On load it calls each command, listens for the `tick` event, uses the clipboard plugin, and prints a `RESULT` line per check on its console; then it calls `page_ready` so the Rust side evaluates scripts in it. In production mode (`custom-protocol`) Tauri embeds this folder and serves it from `tauri://localhost`; in development mode `src/main.rs` serves the same file on the `devUrl`.

| File | What it holds |
|---|---|
| `index.html` | The page, its styles and its IPC checks. |
