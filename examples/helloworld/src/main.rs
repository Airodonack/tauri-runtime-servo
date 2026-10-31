//! Tauri's hello world on tauri-runtime-servo, with the IPC surfaces an app
//! uses: commands (sync, async, failing, raw bytes with headers), events
//! from Rust, a plugin's JavaScript API, and `eval` with a result.
//!
//! By default the page is served over `http://127.0.0.1:47123/` by a small
//! loopback server here, the way VODscribe serves its own page. That URL is
//! the config's `devUrl`, so Tauri treats the page as the app's own (local)
//! origin and lets it call the app's commands without an ACL manifest. With
//! the `custom-protocol` feature (Tauri's production mode) the page is
//! embedded from ui/ and loads from the app's `tauri://localhost` instead.
//! Every check prints a `RESULT` line: the page's on its console, Rust's on
//! stdout.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_runtime_servo::ServoRuntime;

type Runtime = ServoRuntime<tauri::EventLoopMessage>;

const INDEX: &str = include_str!("../ui/index.html");
const PAGE_ADDRESS: &str = "127.0.0.1:47123";

/// Serves the page on the `devUrl` port, from a thread of its own.
fn serve_page() -> std::io::Result<()> {
    let listener = TcpListener::bind(PAGE_ADDRESS)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(error) = answer(stream) {
                log::warn!("page server: {error}");
            }
        }
    });
    Ok(())
}

fn answer(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // Skip the headers; the page needs none of them.
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match path {
        "/" | "/index.html" => ("200 OK", INDEX),
        _ => ("404 Not Found", "not found"),
    };
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {name}! This answer came from Rust.")
}

/// An async command: Tauri runs it on its async runtime, off the main thread.
#[tauri::command]
async fn add_later(a: i64, b: i64) -> i64 {
    a + b
}

#[tauri::command]
fn fail() -> Result<(), String> {
    Err("this command always fails".into())
}

/// Raw bytes in, raw bytes out: the body reversed, and a header from the
/// page appended, so the answer proves both arrived.
#[tauri::command]
fn reverse_bytes(request: Request<'_>) -> Result<Response, String> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected a raw body".into());
    };
    let mut answer: Vec<u8> = bytes.iter().rev().copied().collect();
    let probe = request
        .headers()
        .get("x-probe")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("missing");
    answer.extend_from_slice(probe.as_bytes());
    Ok(Response::new(answer))
}

/// The page says it is listening: evaluate scripts in it from Rust and
/// print what comes back.
#[tauri::command]
fn page_ready(window: WebviewWindow<Runtime>) {
    match window.url() {
        Ok(url) => println!("RESULT webview url as Tauri sees it: {url}"),
        Err(error) => println!("RESULT webview url error: {error}"),
    }
    let scripts = [
        ("string", "document.title"),
        ("number", "6 * 7"),
        (
            "object",
            "({ answer: 42, list: [1, 'two', null], flag: true })",
        ),
        ("undefined", "undefined"),
        ("throws", "(() => { throw new Error('boom') })()"),
    ];
    for (name, script) in scripts {
        let result = window.eval_with_callback(script, move |json| {
            println!("RESULT eval {name} -> {json}");
        });
        if let Err(error) = result {
            println!("RESULT eval {name} not run: {error}");
        }
    }
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,tauri_runtime_servo=info,console=debug,helloworld=info"),
    )
    .init();

    if cfg!(not(feature = "custom-protocol")) {
        serve_page().expect("the page's port is taken");
        log::info!("serving the page on http://{PAGE_ADDRESS}/");
    }

    tauri::Builder::<Runtime>::new()
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            add_later,
            fail,
            reverse_bytes,
            page_ready
        ])
        .setup(move |app| {
            // The window from tauri.conf.json: in development on the devUrl
            // (the page server), else on its own `tauri://localhost/index.html`.
            let mut config = app.config().app.windows[0].clone();
            if cfg!(not(feature = "custom-protocol")) {
                config.url = WebviewUrl::External(format!("http://{PAGE_ADDRESS}/").parse()?);
            }
            let window = WebviewWindowBuilder::from_config(app.handle(), &config)?.build()?;
            // Before the page exists: held until it does, as wry holds it.
            window.eval_with_callback("location.href", |json| {
                println!("RESULT eval early -> {json}");
            })?;

            // An event from Rust every second.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for n in 1.. {
                    std::thread::sleep(Duration::from_secs(1));
                    if handle.emit("tick", n).is_err()
                        || handle.get_webview_window("main").is_none()
                    {
                        break;
                    }
                }
            });

            // For headless runs: close the window from another thread after
            // HELLOWORLD_CLOSE_AFTER seconds, as an app would.
            if let Some(seconds) = std::env::var("HELLOWORLD_CLOSE_AFTER")
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
