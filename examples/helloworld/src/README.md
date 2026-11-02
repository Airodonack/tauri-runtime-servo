# helloworld/src

One file. It serves the page in dev mode, registers the commands and the clipboard plugin, makes the window, emits a `tick` event every second, and runs `eval` checks when the page reports ready.

| File | What it holds |
|---|---|
| `main.rs` | The page server, the commands (`greet`, `add_later`, `fail`, `reverse_bytes`, `page_ready`), the builder on `ServoRuntime`, `HELLOWORLD_CLOSE_AFTER`. |
