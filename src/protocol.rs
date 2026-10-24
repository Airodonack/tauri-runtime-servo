//! Tauri's custom URI schemes (`ipc`, `tauri`, `asset` and the plugins') as
//! Servo protocol handlers.
//!
//! Servo fixes its protocol registry when it is built, while Tauri hands the
//! schemes over per webview, in `PendingWebview::uri_scheme_protocols`. So
//! the registry holds one [`Forwarder`] per scheme, made when Servo is built
//! (at the first webview), and every webview adds its handlers to a shared
//! map, scheme -> webview label -> Tauri's handler. A request finds its
//! handler through the webview it was made for: Servo's `WebViewId` on the
//! request, mapped to the label when the webview is built.
//!
//! Tauri's handlers run on the main thread, as they do with wry on Linux
//! (WebKitGTK calls them there): a synchronous command then runs where it
//! would with wry. Their answer can come from any thread, through the
//! responder.
//!
//! The app's own pages, `tauri://localhost/...`, are the exception: a page
//! on a custom scheme gets an opaque origin in Servo (no storage, `Origin:
//! null`). The runtime loads them as `http://tauri.localhost/...` instead,
//! the URL wry uses on Windows, and answers those requests itself through
//! Servo's request interception, with Tauri's `tauri` handler: the page has
//! a tuple origin, a secure context (a `.localhost` host), and makes no
//! network request. Tauri sees `tauri://localhost/...` wherever it looks
//! (the webview's URL, page loads, navigations, the IPC `Origin`).

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use http::header::{HeaderValue, ORIGIN};
use ipc_channel::ipc;
use ipc_channel::router::ROUTER;
use net_traits::request::{BodyChunkRequest, BodyChunkResponse, Origin};
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, NetworkError, ProtocolHandler, ProtocolRegistry,
    Request, ResourceFetchTiming, Response, ResponseBody,
};
use servo::{WebResourceLoad, WebResourceResponse, WebViewId};
use url::Url;

/// Tauri's handler for one scheme, as `PendingWebview` carries it.
pub type UriSchemeHandler = dyn Fn(&str, http::Request<Vec<u8>>, Box<dyn FnOnce(http::Response<Cow<'static, [u8]>>) + Send>)
    + Send
    + Sync;

/// Runs a closure on the main thread (posted to the event loop).
pub type MainThreadPoster = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// The schemes Tauri itself registers (`ipc`, `tauri`) and the one its
/// asset protocol adds. They go into Servo's registry even before a webview
/// asks for them, since the registry cannot grow once Servo is built.
const TAURI_SCHEMES: [&str; 3] = ["ipc", "tauri", "asset"];

/// Where the runtime serves the app's `tauri://localhost` pages.
const APP_HOST: &str = "tauri.localhost";

/// `tauri://localhost/...` as the runtime loads it, `http://tauri.localhost/...`.
pub fn to_servo_url(url: Url) -> Url {
    if url.scheme() == "tauri" && url.host_str() == Some("localhost") {
        let rest = &url[url::Position::BeforePath..];
        if let Ok(servo_url) = Url::parse(&format!("http://{APP_HOST}{rest}")) {
            return servo_url;
        }
    }
    url
}

/// What Tauri knows the app's pages as: the inverse of [`to_servo_url`].
pub fn to_tauri_url(url: Url) -> Url {
    if is_app_url(&url) {
        let rest = &url[url::Position::BeforePath..];
        if let Ok(tauri_url) = Url::parse(&format!("tauri://localhost{rest}")) {
            return tauri_url;
        }
    }
    url
}

fn is_app_url(url: &Url) -> bool {
    url.scheme() == "http" && url.host_str() == Some(APP_HOST) && url.port().is_none()
}

type FutureResponse<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;

#[derive(Default)]
struct Handlers {
    /// scheme -> webview label -> handler.
    by_scheme: HashMap<String, HashMap<String, Arc<UriSchemeHandler>>>,
    labels: HashMap<WebViewId, String>,
    /// The schemes in Servo's registry, once it is built.
    registered: Option<BTreeSet<String>>,
    post: Option<MainThreadPoster>,
}

/// The handlers of every webview, shared by the forwarders (on Servo's
/// network threads) and the runtime (on the main thread).
#[derive(Clone, Default)]
pub struct SchemeHandlers(Arc<RwLock<Handlers>>);

impl SchemeHandlers {
    /// Adds a webview's handlers. A scheme Servo's registry does not have
    /// (one first seen after Servo was built) is logged: its requests fail.
    pub fn add_webview(&self, label: &str, protocols: HashMap<String, Box<UriSchemeHandler>>) {
        let mut handlers = self.0.write().unwrap();
        for (scheme, handler) in protocols {
            if let Some(registered) = &handlers.registered
                && !registered.contains(&scheme)
            {
                log::warn!(
                    "webview {label}: the URI scheme `{scheme}` arrived after Servo was built \
                     and cannot be registered; its requests will fail"
                );
            }
            handlers
                .by_scheme
                .entry(scheme)
                .or_default()
                .insert(label.to_owned(), Arc::from(handler));
        }
    }

    /// Ties Servo's id for a webview to its label, once the webview exists.
    pub fn set_webview_id(&self, id: WebViewId, label: &str) {
        self.0.write().unwrap().labels.insert(id, label.to_owned());
    }

    /// Forgets a webview that is gone.
    pub fn remove_webview(&self, id: WebViewId, label: &str) {
        let mut handlers = self.0.write().unwrap();
        handlers.labels.remove(&id);
        for by_label in handlers.by_scheme.values_mut() {
            by_label.remove(label);
        }
    }

    /// Servo's registry: a forwarder for Tauri's schemes and for every
    /// scheme a webview has brought so far. Called once, as Servo is built.
    pub fn registry(&self, post: MainThreadPoster) -> ProtocolRegistry {
        let mut handlers = self.0.write().unwrap();
        let schemes: BTreeSet<String> = TAURI_SCHEMES
            .iter()
            .map(|s| s.to_string())
            .chain(handlers.by_scheme.keys().cloned())
            .collect();
        let mut registry = ProtocolRegistry::default();
        for scheme in &schemes {
            let forwarder = Forwarder {
                scheme: scheme.clone(),
                handlers: self.clone(),
                post: post.clone(),
            };
            if let Err(error) = registry.register(scheme, forwarder) {
                log::error!("the URI scheme `{scheme}` cannot be registered with Servo: {error:?}");
            }
        }
        log::debug!(
            "custom URI schemes registered with Servo: {}",
            schemes.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        handlers.registered = Some(schemes);
        handlers.post = Some(post);
        registry
    }

    /// Answers a load of an app page (`http://tauri.localhost/...`) with
    /// Tauri's `tauri` handler; any other load is left to Servo. Main thread.
    pub fn intercept(&self, webview: WebViewId, load: WebResourceLoad) {
        let request = load.request();
        if !is_app_url(&request.url) {
            return; // dropping the load lets Servo fetch it
        }
        let Some(post) = self.0.read().unwrap().post.clone() else {
            return;
        };
        let Some((label, handler)) = self.find("tauri", Some(webview)) else {
            log::warn!("no `tauri` handler for {}", request.url);
            return;
        };
        let mut tauri_request = http::Request::new(Vec::new());
        *tauri_request.method_mut() = request.method.clone();
        *tauri_request.headers_mut() = request.headers.clone();
        match to_tauri_url(request.url.clone()).as_str().parse() {
            Ok(uri) => *tauri_request.uri_mut() = uri,
            Err(error) => {
                log::warn!("{} is not a valid request URI: {error}", request.url);
                return;
            }
        }

        // The load is not `Send` and the answer may come from any thread:
        // the load waits on the main thread, the answer is posted there.
        let id = NEXT_APP_LOAD.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        APP_LOADS.with(|loads| loads.borrow_mut().insert(id, load));
        handler(
            &label,
            tauri_request,
            Box::new(move |response| post(Box::new(move || finish_app_load(id, response)))),
        );
    }

    /// The label and handler for a request: the target webview's, or, for a
    /// request no webview is named on (a worker's), the first webview's
    /// that has this scheme.
    fn find(
        &self,
        scheme: &str,
        webview: Option<WebViewId>,
    ) -> Option<(String, Arc<UriSchemeHandler>)> {
        let handlers = self.0.read().unwrap();
        let by_label = handlers.by_scheme.get(scheme)?;
        let label = webview.and_then(|id| handlers.labels.get(&id));
        match label {
            Some(label) => by_label
                .get(label)
                .map(|handler| (label.clone(), handler.clone())),
            None => {
                let (label, handler) = by_label.iter().next()?;
                log::debug!("{scheme}: request without a known webview, sent to {label}");
                Some((label.clone(), handler.clone()))
            }
        }
    }
}

/// Servo's handler for one scheme: hands the request to Tauri's handler
/// for the webview it came from and turns the answer into Servo's response.
struct Forwarder {
    scheme: String,
    handlers: SchemeHandlers,
    post: MainThreadPoster,
}

impl ProtocolHandler for Forwarder {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> FutureResponse<'a> {
        let url = request.current_url();
        let timing = ResourceFetchTiming::new(request.timing_type());
        let mut headers = request.headers.clone();
        // Servo adds `Origin` in its HTTP fetch only, which a custom scheme
        // never reaches; Tauri's IPC requires it (it decides from it whether
        // the page is local or remote). An app page's is Tauri's own.
        if !headers.contains_key(ORIGIN)
            && let Origin::Origin(origin) = &request.origin
        {
            let origin = origin.ascii_serialization();
            let origin = if origin == format!("http://{APP_HOST}") {
                "tauri://localhost".into()
            } else {
                origin
            };
            if let Ok(value) = HeaderValue::from_str(&origin) {
                headers.insert(ORIGIN, value);
            }
        }
        let method = request.method.clone();
        let body = request
            .body
            .as_ref()
            .filter(|body| !body.source_is_null())
            .map(|body| body.clone_stream());
        let webview = request.target_webview_id;

        Box::pin(async move {
            let body = match body {
                Some(stream) => match read_body(stream).await {
                    Ok(body) => body,
                    Err(error) => {
                        log::warn!("{}: request body not read: {error}", self.scheme);
                        return Response::network_error(NetworkError::ResourceLoadError(error));
                    }
                },
                None => Vec::new(),
            };
            let mut tauri_request = http::Request::new(body);
            *tauri_request.method_mut() = method;
            *tauri_request.headers_mut() = headers;
            match url.as_str().parse() {
                Ok(uri) => *tauri_request.uri_mut() = uri,
                Err(error) => {
                    return Response::network_error(NetworkError::ResourceLoadError(format!(
                        "{url} is not a valid request URI: {error}"
                    )));
                }
            }

            let Some((label, handler)) = self.handlers.find(&self.scheme, webview) else {
                log::warn!("{}: no handler for {url}", self.scheme);
                return Response::network_error(NetworkError::ResourceLoadError(format!(
                    "no handler for the {} scheme",
                    self.scheme
                )));
            };
            let (tx, rx) = tokio::sync::oneshot::channel();
            (self.post)(Box::new(move || {
                handler(
                    &label,
                    tauri_request,
                    Box::new(move |response| {
                        // The fetch may have been cancelled meanwhile.
                        let _ = tx.send(response);
                    }),
                );
            }));
            match rx.await {
                Ok(response) => to_servo_response(url, timing, response),
                Err(_) => Response::network_error(NetworkError::ResourceLoadError(format!(
                    "the {} handler dropped the request without an answer",
                    self.scheme
                ))),
            }
        })
    }

    // The page (on an http origin, or another scheme) reads the answers with
    // `fetch()`, without CORS; Tauri also sends `Access-Control-Allow-Origin: *`.
    fn is_fetchable(&self) -> bool {
        true
    }

    fn is_secure(&self) -> bool {
        true
    }
}

thread_local! {
    /// App page loads waiting for Tauri's answer (main thread).
    static APP_LOADS: RefCell<HashMap<u64, WebResourceLoad>> = Default::default();
    static NEXT_APP_LOAD: Cell<u64> = const { Cell::new(0) };
}

fn finish_app_load(id: u64, response: http::Response<Cow<'static, [u8]>>) {
    let Some(load) = APP_LOADS.with(|loads| loads.borrow_mut().remove(&id)) else {
        return;
    };
    let (parts, body) = response.into_parts();
    let servo_response = WebResourceResponse::new(load.request().url.clone())
        .status_code(parts.status)
        .status_message(
            parts
                .status
                .canonical_reason()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
        )
        .headers(parts.headers);
    let mut intercepted = load.intercept(servo_response);
    if !body.is_empty() {
        intercepted.send_body_data(body.into_owned());
    }
    intercepted.finish();
}

fn to_servo_response(
    url: servo::ServoUrl,
    timing: ResourceFetchTiming,
    response: http::Response<Cow<'static, [u8]>>,
) -> Response {
    let (parts, body) = response.into_parts();
    let mut servo_response = Response::new(url, timing);
    servo_response.status = HttpStatus::new_raw(
        parts.status.as_u16(),
        parts
            .status
            .canonical_reason()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    );
    servo_response.headers = parts.headers;
    *servo_response.body.lock() = ResponseBody::Done(body.into_owned());
    servo_response
}

/// Reads a request body from the page, chunk by chunk, the way Servo's HTTP
/// loader transmits one (`obtain_response_setup_router_callback` in
/// components/net/http_loader.rs): connect, then ask for each chunk.
async fn read_body(
    stream: Arc<parking_lot::Mutex<Option<ipc::IpcSender<BodyChunkRequest>>>>,
) -> Result<Vec<u8>, String> {
    let request_chunk = |message: BodyChunkRequest| -> Result<(), String> {
        let lock = stream.lock();
        let sender = lock.as_ref().ok_or("the body stream is closed")?;
        sender.send(message).map_err(|e| e.to_string())
    };

    let (chunk_sender, chunk_receiver) = ipc::channel().map_err(|e| e.to_string())?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ROUTER.add_typed_route(
        chunk_receiver,
        Box::new(move |message| {
            let _ = tx.send(message);
        }),
    );
    request_chunk(BodyChunkRequest::Connect(chunk_sender))?;
    request_chunk(BodyChunkRequest::Chunk)?;

    let mut body = Vec::new();
    while let Some(message) = rx.recv().await {
        match message.map_err(|e| e.to_string())? {
            BodyChunkResponse::Chunk(bytes) => {
                body.extend_from_slice(&bytes);
                request_chunk(BodyChunkRequest::Chunk)?;
            }
            BodyChunkResponse::Done => return Ok(body),
            BodyChunkResponse::Error => return Err("the page's body stream failed".into()),
        }
    }
    Err("the body stream ended without its last chunk".into())
}
