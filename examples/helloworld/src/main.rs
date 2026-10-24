//! Tauri's hello world on tauri-runtime-servo.
//!
//! The page is served over `http://127.0.0.1:<port>/` by a small loopback
//! server here, the way VODscribe serves its own page: the runtime does not
//! register Tauri's `tauri://` scheme yet (and a custom scheme would give the
//! page an opaque origin in Servo), so the window is built in `setup` with
//! that URL instead of the bundled one.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use tauri::{WebviewUrl, WebviewWindowBuilder};
use tauri_runtime_servo::ServoRuntime;

type Runtime = ServoRuntime<tauri::EventLoopMessage>;

const INDEX: &str = include_str!("../ui/index.html");

/// Serves the page on a free loopback port, from a thread of its own.
fn serve_page() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Err(error) = answer(stream) {
                log::warn!("page server: {error}");
            }
        }
    });
    Ok(port)
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

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("warn,tauri_runtime_servo=info,console=info,helloworld=info"),
    )
    .init();

    let port = serve_page().expect("no loopback port for the page");
    log::info!("serving the page on http://127.0.0.1:{port}/");

    tauri::Builder::<Runtime>::new()
        .setup(move |app| {
            // The window from tauri.conf.json, on the loopback URL.
            let mut config = app.config().app.windows[0].clone();
            config.url = WebviewUrl::External(format!("http://127.0.0.1:{port}/").parse()?);
            let window = WebviewWindowBuilder::from_config(app.handle(), &config)?.build()?;
            // For headless runs: close the window from another thread after
            // HELLOWORLD_CLOSE_AFTER seconds, as an app would.
            if let Some(seconds) = std::env::var("HELLOWORLD_CLOSE_AFTER")
                .ok()
                .and_then(|s| s.parse().ok())
            {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(seconds));
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
