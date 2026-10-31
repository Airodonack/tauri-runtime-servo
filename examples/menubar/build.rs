//! Tauri's build step: `tauri-build` reads `tauri.conf.json` and the
//! capabilities, checks them, writes the generated schemas to `gen/` and
//! prepares the context `tauri::generate_context!()` embeds (the config,
//! the icon, and in production mode the page from `ui/`).

fn main() {
    tauri_build::build();
}
