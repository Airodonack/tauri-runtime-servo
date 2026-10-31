//! A Tauri app with a menu bar on tauri-runtime-servo: the window gets a
//! "File" menu through Tauri's menu API, and the page must start below the
//! bar, not under it. Every check prints a `RESULT` line: the page's (its
//! size, where a click lands) on its console, Rust's (menu events) on
//! stdout.
//!
//! The menu's "Check the layout" item, and a timer five seconds after
//! start, print where the menu bar and the page are (`RESULT layout: ...`,
//! "clear" or "overlaps"). "Hide the menu bar for 3 seconds" hides the bar
//! and shows it again, checking the layout each time, so a run sees the
//! page follow the bar both ways.

use std::time::Duration;

use gtk::prelude::*;
use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{WebviewWindow, WebviewWindowBuilder};
use tauri_runtime_servo::ServoRuntime;

type Runtime = ServoRuntime<tauri::EventLoopMessage>;

/// Prints where the menu bar and the page are, in physical pixels from the
/// window's top-left corner, and whether they overlap. GTK's widgets live on
/// the main thread, so the check runs there.
fn report_layout(window: &WebviewWindow<Runtime>) {
    let window = window.clone();
    let main = window.clone();
    let result = main.run_on_main_thread(move || {
        let scale = window.scale_factor().unwrap_or(1.0);
        let bar = window.default_vbox().ok().and_then(|vbox| {
            vbox.children()
                .into_iter()
                .find(|child| child.is::<gtk::MenuBar>())
        });
        let (bar_text, bar_bottom) = match bar {
            Some(bar) if bar.is_visible() => {
                let a = bar.allocation();
                let px = |v: i32| (v as f64 * scale).round() as i32;
                let text = format!(
                    "menu bar shown at {},{} {}x{}",
                    px(a.x()),
                    px(a.y()),
                    px(a.width()),
                    px(a.height())
                );
                (text, px(a.y() + a.height()))
            }
            Some(_) => ("menu bar hidden".to_owned(), 0),
            None => ("no menu bar".to_owned(), 0),
        };
        let page = match window.as_ref().bounds() {
            Ok(bounds) => bounds,
            Err(error) => {
                println!("RESULT layout: {bar_text}, page bounds error: {error}");
                return;
            }
        };
        let position = page.position.to_physical::<i32>(scale);
        let size = page.size.to_physical::<u32>(scale);
        let verdict = if position.y >= bar_bottom {
            "clear"
        } else {
            "overlaps"
        };
        println!(
            "RESULT layout: {bar_text}, page at {},{} {}x{}: {verdict}",
            position.x, position.y, size.width, size.height
        );
    });
    if let Err(error) = result {
        println!("RESULT layout not checked: {error}");
    }
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,tauri_runtime_servo=info,console=debug,menubar=info"),
    )
    .init();

    tauri::Builder::<Runtime>::new()
        .setup(|app| {
            let hello = MenuItemBuilder::with_id("hello", "Say hello").build(app)?;
            let layout = MenuItemBuilder::with_id("layout", "Check the layout").build(app)?;
            let hide =
                MenuItemBuilder::with_id("hide", "Hide the menu bar for 3 seconds").build(app)?;
            let file = SubmenuBuilder::new(app, "File")
                .item(&hello)
                .item(&layout)
                .item(&hide)
                .build()?;
            let menu = MenuBuilder::new(app).item(&file).build()?;

            // The window from tauri.conf.json, on the app's own
            // `tauri://localhost/index.html`.
            let config = app.config().app.windows[0].clone();
            let window = WebviewWindowBuilder::from_config(app.handle(), &config)?
                .menu(menu)
                .build()?;

            let menu_window = window.clone();
            window.on_menu_event(move |_, event| {
                println!("RESULT menu event: {}", event.id().as_ref());
                if event.id() == "layout" {
                    report_layout(&menu_window);
                }
                if event.id() == "hide" {
                    if let Err(error) = menu_window.hide_menu() {
                        println!("RESULT hide_menu failed: {error}");
                        return;
                    }
                    let window = menu_window.clone();
                    std::thread::spawn(move || {
                        // GTK lays the window out again on its next frame.
                        std::thread::sleep(Duration::from_secs(1));
                        report_layout(&window);
                        std::thread::sleep(Duration::from_secs(2));
                        if let Err(error) = window.show_menu() {
                            println!("RESULT show_menu failed: {error}");
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                        report_layout(&window);
                    });
                }
            });

            let first_check = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(5));
                report_layout(&first_check);
            });

            // For headless runs: close the window after MENUBAR_CLOSE_AFTER
            // seconds, as an app would.
            if let Some(seconds) = std::env::var("MENUBAR_CLOSE_AFTER")
                .ok()
                .and_then(|s| s.parse().ok())
            {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(seconds));
                    log::info!("closing the window");
                    if let Err(error) = window.close() {
                        log::error!("close failed: {error}");
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the application");
}
