# helloworld/capabilities: what the page may call

Tauri 2 grants a window's pages access to commands and plugins through capability files; `tauri-build` reads every file here. The app's own commands need no entry (an app without an ACL manifest allows its commands to its local pages); plugins' commands do.

| File | What it holds |
|---|---|
| `default.json` | The `main` window's permissions: Tauri's core defaults and the clipboard plugin's read and write text commands. |
