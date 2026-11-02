# examples

Tauri apps on this runtime. Each is a member of the workspace and builds against `../src`.

```sh
export CC=clang CXX=clang++
cargo run --release -p helloworld                              # page from a local dev server
cargo run --release -p helloworld --features custom-protocol   # page from tauri://localhost
cargo run --release -p menubar
```

| Folder | What it holds |
|---|---|
| [`helloworld/`](helloworld/README.md) | Checks every IPC surface and prints a `RESULT` line for each. CI runs it as the smoke test. |
| [`menubar/`](menubar/README.md) | A window with a menu bar. Checks that the page sits below the bar and follows it. |
