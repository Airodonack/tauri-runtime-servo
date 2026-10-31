# examples/: Tauri apps on this runtime

Each folder here is a complete Tauri app that uses `tauri-runtime-servo` instead of wry, and a member of this repository's Cargo workspace, so it builds against the runtime in `../src` with the same pinned dependencies and the same Stylo patch. They are the quickest way to see the runtime work and the place to reproduce a bug.

## How to run one

From the repository root (the [root README](../README.md) lists what the build needs):

```sh
export CC=clang CXX=clang++
cargo run --release -p helloworld                              # development mode: the page from a loopback server
cargo run --release -p helloworld --features custom-protocol   # production mode: the page from tauri://localhost
cargo run --release -p menubar                                 # a window with a menu bar above the page
```

The first build compiles Servo; later ones only the runtime and the app.

## What is in this directory

| Folder | What it holds |
|---|---|
| [`helloworld/`](helloworld/README.md) | Tauri's hello world, extended to exercise every IPC surface an app uses (commands of each kind, events, a plugin, `eval`), printing a `RESULT` line for each check; with the smoke test CI runs it under. |
| [`menubar/`](menubar/README.md) | A window with a menu bar from Tauri's menu API: checks that the page starts below the bar and follows it when the bar is hidden or shown, and that the menu's items reach the app, printing a `RESULT` line for each check. |
