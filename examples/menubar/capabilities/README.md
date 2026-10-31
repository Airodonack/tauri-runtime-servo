# menubar/capabilities: what the page may call

Tauri 2 grants a window's pages access to commands and plugins through capability files; `tauri-build` reads every file here. The page calls nothing, so Tauri's core defaults are all it gets.

| File | What it holds |
|---|---|
| `default.json` | The `main` window's permissions: Tauri's core defaults. |
