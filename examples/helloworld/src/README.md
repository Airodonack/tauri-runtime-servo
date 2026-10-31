# helloworld/src: the app's Rust side

The whole app is one file. It starts logging, serves the page on the loopback `devUrl` in development mode, registers the commands the page calls and the clipboard plugin, makes the window from `tauri.conf.json`, emits a `tick` event every second, and on the page's `page_ready` command evaluates scripts in it and prints their JSON results. The [helloworld README](../README.md) says how to run it and what each `RESULT` line checks.

| File | What it holds |
|---|---|
| `main.rs` | The app: the page server, the commands (`greet`, `add_later`, `fail`, `reverse_bytes`, `page_ready`), the builder on `ServoRuntime`, and `HELLOWORLD_CLOSE_AFTER` for headless runs. |
