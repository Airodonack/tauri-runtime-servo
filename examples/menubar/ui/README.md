# menubar/ui: the page

The page the app shows, plain HTML and JavaScript with no build step. It draws a red band along its top edge (hidden if the page sits under the menu bar) and prints a `RESULT` line with its size at load and on every resize, and with the place of every click it receives. Tauri embeds this folder and serves it from `tauri://localhost`.

| File | What it holds |
|---|---|
| `index.html` | The page, its styles and its checks. |
