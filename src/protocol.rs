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
//!
//! An app page the runtime cannot route (no `tauri` handler for its
//! webview yet, or a load Servo makes for no webview, which reaches the
//! runtime's `ServoDelegate`) gets an error response: left to Servo, it
//! would be fetched from whatever local server answers on
//! `tauri.localhost`, port 80, as the app's own origin.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use http::header::{HeaderMap, HeaderValue, ORIGIN};
use http::{Method, StatusCode};
use ipc_channel::ipc;
use ipc_channel::router::ROUTER;
use net_traits::request::{BodyChunkRequest, BodyChunkResponse, Origin};
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, NetworkError, ProtocolHandler, ProtocolRegistry,
    Request, ResourceFetchTiming, Response, ResponseBody,
};
use servo::{ServoDelegate, WebResourceLoad, WebResourceResponse, WebViewId};
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
        let schemes = handlers.schemes_to_register();
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
    /// Tauri's `tauri` handler; any other load is left to Servo. `webview`
    /// is the one the load is for, `None` for a load Servo makes for no
    /// webview (its `ServoDelegate`). Main thread.
    ///
    /// An app page the runtime cannot route is refused with an error
    /// response, never left to Servo: Servo would fetch
    /// `http://tauri.localhost/...` from the network, where `.localhost`
    /// resolves to this machine, and any local server on port 80 would be
    /// served as the app's own origin.
    pub fn intercept(&self, webview: Option<WebViewId>, load: WebResourceLoad) {
        let (label, handler, post, uri) = match self.route(webview, load.request().url.as_url()) {
            AppRoute::NotAnAppPage => return, // dropping the load lets Servo fetch it
            AppRoute::Refused(reason) => {
                log::warn!("app page {} refused: {reason}", load.request().url);
                refuse_app_load(load);
                return;
            }
            AppRoute::Handler {
                label,
                handler,
                post,
                uri,
            } => (label, handler, post, uri),
        };
        let request = load.request();
        let tauri_request = tauri_request(
            request.method.clone(),
            request.headers.clone(),
            uri,
            Vec::new(),
        );

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

    /// Who answers a load of `url` made for `webview`.
    fn route(&self, webview: Option<WebViewId>, url: &Url) -> AppRoute {
        if !is_app_url(url) {
            return AppRoute::NotAnAppPage;
        }
        // Tauri checks a request's permissions by the webview label it is
        // handed: a load for no webview has none to give it.
        let Some(webview) = webview else {
            return AppRoute::Refused("the load is for no webview".into());
        };
        let Some(post) = self.0.read().unwrap().post.clone() else {
            return AppRoute::Refused("Servo's URI schemes are not set up yet".into());
        };
        let Some((label, handler)) = self.find("tauri", Some(webview)) else {
            return AppRoute::Refused("no `tauri` handler for its webview".into());
        };
        match to_tauri_url(url.clone()).as_str().parse() {
            Ok(uri) => AppRoute::Handler {
                label,
                handler,
                post,
                uri,
            },
            Err(error) => AppRoute::Refused(format!("not a valid request URI: {error}")),
        }
    }

    /// The label and handler for a request: the target webview's. A request
    /// that names no webview Tauri knows of (one that arrives before the
    /// webview's id is recorded, or one Servo made for no webview) goes to
    /// the only webview with this scheme, and to none when there are
    /// several: Tauri checks a request's permissions by the label it is
    /// handed, so a guess could grant one webview's page another's.
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
            None if by_label.len() == 1 => {
                let (label, handler) = by_label.iter().next()?;
                log::debug!("{scheme}: request without a known webview, sent to {label}");
                Some((label.clone(), handler.clone()))
            }
            None => {
                // The caller logs the refusal.
                log::debug!(
                    "{scheme}: request without a known webview, and {} webviews have this \
                     scheme: refused",
                    by_label.len()
                );
                None
            }
        }
    }
}

impl Handlers {
    /// Tauri's schemes and every scheme a webview has brought so far.
    fn schemes_to_register(&self) -> BTreeSet<String> {
        TAURI_SCHEMES
            .iter()
            .map(|s| s.to_string())
            .chain(self.by_scheme.keys().cloned())
            .collect()
    }
}

/// The runtime's `ServoDelegate`: Servo offers it the loads made for no
/// webview, which would otherwise go to the network unasked. App pages
/// among them are refused ([`SchemeHandlers::intercept`]).
pub struct AppPageGuard(pub SchemeHandlers);

impl ServoDelegate for AppPageGuard {
    fn load_web_resource(&self, load: WebResourceLoad) {
        self.0.intercept(None, load);
    }
}

/// Who answers a load Servo offers the runtime ([`SchemeHandlers::route`]).
enum AppRoute {
    /// Not an app page: Servo fetches it.
    NotAnAppPage,
    /// An app page, for Tauri's `tauri` handler of webview `label`.
    Handler {
        label: String,
        handler: Arc<UriSchemeHandler>,
        post: MainThreadPoster,
        uri: http::Uri,
    },
    /// An app page nothing here can answer, and why.
    Refused(String),
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
        let page_origin = match &request.origin {
            Origin::Origin(origin) => Some(origin.ascii_serialization()),
            _ => None,
        };
        add_origin(&mut headers, page_origin.as_deref());
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
            let uri = match url.as_str().parse() {
                Ok(uri) => uri,
                Err(error) => {
                    return Response::network_error(NetworkError::ResourceLoadError(format!(
                        "{url} is not a valid request URI: {error}"
                    )));
                }
            };
            let tauri_request = tauri_request(method, headers, uri, body);

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
    let servo_response = WebResourceResponse::new(load.request().url.as_url().clone())
        .status_code(parts.status)
        .status_message(reason_phrase(parts.status))
        .headers(parts.headers);
    let mut intercepted = load.intercept(servo_response);
    if !body.is_empty() {
        intercepted.send_body_data(body.into_owned());
    }
    intercepted.finish();
}

/// Servo adds `Origin` in its HTTP fetch only, which a custom scheme never
/// reaches; Tauri's IPC requires it (it decides from it whether the page is
/// local or remote). `page_origin` is the page's serialized origin, `None`
/// for an opaque one; an app page's is Tauri's own, `tauri://localhost`. An
/// `Origin` the request has already is kept.
fn add_origin(headers: &mut HeaderMap, page_origin: Option<&str>) {
    if headers.contains_key(ORIGIN) {
        return;
    }
    let Some(origin) = page_origin else { return };
    let origin = if origin == format!("http://{APP_HOST}") {
        "tauri://localhost"
    } else {
        origin
    };
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(ORIGIN, value);
    }
}

/// The request Tauri's handler gets, for an app page or a scheme request.
fn tauri_request(
    method: Method,
    headers: HeaderMap,
    uri: http::Uri,
    body: Vec<u8>,
) -> http::Request<Vec<u8>> {
    let mut request = http::Request::new(body);
    *request.method_mut() = method;
    *request.headers_mut() = headers;
    *request.uri_mut() = uri;
    request
}

/// The status line's text for a status code, empty for one without a
/// standard reason (Tauri's responses carry none of their own).
fn reason_phrase(status: StatusCode) -> Vec<u8> {
    status
        .canonical_reason()
        .unwrap_or_default()
        .as_bytes()
        .to_vec()
}

/// The answer to an app page the runtime cannot route.
fn refusal(url: Url) -> (WebResourceResponse, Vec<u8>) {
    let status = StatusCode::INTERNAL_SERVER_ERROR;
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    let response = WebResourceResponse::new(url)
        .status_code(status)
        .status_message(reason_phrase(status))
        .headers(headers);
    (
        response,
        b"This page of the app could not be loaded.".to_vec(),
    )
}

fn refuse_app_load(load: WebResourceLoad) {
    let (response, body) = refusal(load.request().url.as_url().clone());
    let mut intercepted = load.intercept(response);
    intercepted.send_body_data(body);
    intercepted.finish();
}

fn to_servo_response(
    url: servo::ServoUrl,
    timing: ResourceFetchTiming,
    response: http::Response<Cow<'static, [u8]>>,
) -> Response {
    let (parts, body) = response.into_parts();
    let mut servo_response = Response::new(url, timing);
    servo_response.status = HttpStatus::new_raw(parts.status.as_u16(), reason_phrase(parts.status));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> Box<UriSchemeHandler> {
        Box::new(|_, _, _| {})
    }

    fn app_url() -> Url {
        Url::parse("http://tauri.localhost/index.html").unwrap()
    }

    fn webview_id() -> WebViewId {
        servo_base::id::TEST_WEBVIEW_ID
    }

    fn assert_refused(route: AppRoute) {
        match route {
            AppRoute::Refused(_) => {}
            AppRoute::NotAnAppPage => panic!("an app page was left to Servo's network fetch"),
            AppRoute::Handler { label, .. } => panic!("an app page was sent to {label}"),
        }
    }

    #[test]
    fn an_app_page_before_servo_is_built_is_refused() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("tauri".to_owned(), handler())]));
        assert_refused(schemes.route(Some(webview_id()), &app_url()));
    }

    #[test]
    fn an_app_page_without_a_tauri_handler_is_refused() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("ipc".to_owned(), handler())]));
        drop(schemes.registry(Arc::new(|_| {})));
        assert_refused(schemes.route(Some(webview_id()), &app_url()));
    }

    #[test]
    fn an_app_page_for_no_webview_is_refused() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("tauri".to_owned(), handler())]));
        drop(schemes.registry(Arc::new(|_| {})));
        assert_refused(schemes.route(None, &app_url()));
    }

    #[test]
    fn an_app_page_with_a_handler_goes_to_it() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("tauri".to_owned(), handler())]));
        drop(schemes.registry(Arc::new(|_| {})));
        match schemes.route(Some(webview_id()), &app_url()) {
            AppRoute::Handler { label, uri, .. } => {
                assert_eq!(label, "main");
                assert_eq!(uri, "tauri://localhost/index.html");
            }
            _ => panic!("the app page was not sent to its webview's handler"),
        }
    }

    #[test]
    fn other_pages_are_left_to_servo() {
        let schemes = SchemeHandlers::default();
        let url = Url::parse("http://tauri.localhost:8080/").unwrap();
        assert!(matches!(schemes.route(None, &url), AppRoute::NotAnAppPage));
    }

    #[test]
    fn a_refused_app_page_gets_an_error_response() {
        let (response, body) = refusal(app_url());
        assert_eq!(
            response.status_code,
            http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(response.url, app_url());
        assert!(!body.is_empty());
    }

    #[test]
    fn a_request_without_a_webview_goes_to_the_only_one() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("ipc".to_owned(), handler())]));
        let (label, _) = schemes
            .find("ipc", None)
            .expect("the only webview's handler");
        assert_eq!(label, "main");
    }

    #[test]
    fn a_request_without_a_webview_is_refused_when_several_could_take_it() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("trusted", HashMap::from([("ipc".to_owned(), handler())]));
        schemes.add_webview("remote", HashMap::from([("ipc".to_owned(), handler())]));
        let found = schemes.find("ipc", None).map(|(label, _)| label);
        assert_eq!(found, None);
    }

    /// The label `find` gives a request from the webview Servo knows as
    /// `webview_id()`, recorded as `label`.
    fn found_for(schemes: &SchemeHandlers, scheme: &str, label: &str) -> Option<String> {
        schemes.set_webview_id(webview_id(), label);
        schemes
            .find(scheme, Some(webview_id()))
            .map(|(label, _)| label)
    }

    #[test]
    fn a_request_goes_to_the_webview_it_names() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("ipc".to_owned(), handler())]));
        schemes.add_webview("panel", HashMap::from([("ipc".to_owned(), handler())]));
        assert_eq!(
            found_for(&schemes, "ipc", "panel").as_deref(),
            Some("panel")
        );
        assert_eq!(found_for(&schemes, "ipc", "main").as_deref(), Some("main"));
    }

    #[test]
    fn a_request_for_an_unknown_scheme_is_refused() {
        let schemes = SchemeHandlers::default();
        schemes.add_webview("main", HashMap::from([("ipc".to_owned(), handler())]));
        assert_eq!(found_for(&schemes, "plugin-x", "main"), None);
        assert!(schemes.find("plugin-x", None).is_none());
    }

    #[test]
    fn a_webview_without_the_scheme_is_not_given_another_s_handler() {
        // "remote" has no `ipc` handler (Tauri gave it none); its requests
        // must not reach the only webview that has one.
        let schemes = SchemeHandlers::default();
        schemes.add_webview("trusted", HashMap::from([("ipc".to_owned(), handler())]));
        schemes.add_webview("remote", HashMap::from([("asset".to_owned(), handler())]));
        assert_eq!(found_for(&schemes, "ipc", "remote"), None);
    }

    fn registered(schemes: &SchemeHandlers) -> Vec<String> {
        let handlers = schemes.0.read().unwrap();
        handlers.schemes_to_register().into_iter().collect()
    }

    #[test]
    fn tauri_s_schemes_are_registered_before_any_webview_brings_them() {
        let schemes = SchemeHandlers::default();
        assert_eq!(registered(&schemes), ["asset", "ipc", "tauri"]);
        schemes.add_webview(
            "main",
            HashMap::from([
                ("ipc".to_owned(), handler()),
                ("plugin-x".to_owned(), handler()),
            ]),
        );
        assert_eq!(registered(&schemes), ["asset", "ipc", "plugin-x", "tauri"]);
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn app_pages_load_from_tauri_localhost() {
        let cases = [
            (
                "tauri://localhost/index.html",
                "http://tauri.localhost/index.html",
            ),
            ("tauri://localhost", "http://tauri.localhost/"),
            (
                "tauri://localhost/a/b.js?x=1&y=%2F#top",
                "http://tauri.localhost/a/b.js?x=1&y=%2F#top",
            ),
            (
                "tauri://localhost/caf%C3%A9%20menu.html",
                "http://tauri.localhost/caf%C3%A9%20menu.html",
            ),
            // The port and user info have no place in an app page's URL.
            ("tauri://localhost:8080/x", "http://tauri.localhost/x"),
            (
                "tauri://user:secret@localhost/x",
                "http://tauri.localhost/x",
            ),
        ];
        for (tauri, servo) in cases {
            assert_eq!(to_servo_url(url(tauri)).as_str(), servo, "{tauri}");
        }
    }

    #[test]
    fn other_urls_load_as_they_are() {
        for other in [
            "tauri://other/index.html",
            "asset://localhost/file.png",
            "ipc://localhost/cmd",
            "http://127.0.0.1:47123/",
            "https://tauri.localhost/",
            "http://localhost/",
        ] {
            assert_eq!(to_servo_url(url(other)).as_str(), other);
        }
    }

    #[test]
    fn tauri_sees_app_pages_as_tauri_localhost() {
        let cases = [
            ("http://tauri.localhost/", "tauri://localhost/"),
            (
                "http://tauri.localhost/a/b.js?x=1#top",
                "tauri://localhost/a/b.js?x=1#top",
            ),
            (
                "http://tauri.localhost/caf%C3%A9.html",
                "tauri://localhost/caf%C3%A9.html",
            ),
            ("http://user@tauri.localhost/x", "tauri://localhost/x"),
        ];
        for (servo, tauri) in cases {
            assert_eq!(to_tauri_url(url(servo)).as_str(), tauri, "{servo}");
        }
    }

    #[test]
    fn only_the_app_host_on_port_80_is_an_app_page() {
        for other in [
            "http://tauri.localhost:8080/",
            "https://tauri.localhost/",
            "http://tauri.localhost.example/",
            "http://127.0.0.1:47123/",
            "ipc://localhost/cmd",
        ] {
            assert_eq!(to_tauri_url(url(other)).as_str(), other);
        }
        // An explicit :80 is the default port, which the URL parser drops.
        assert_eq!(
            to_tauri_url(url("http://tauri.localhost:80/x")).as_str(),
            "tauri://localhost/x"
        );
    }

    #[test]
    fn app_page_urls_survive_the_round_trip() {
        for tauri in [
            "tauri://localhost/",
            "tauri://localhost/index.html?q=a%20b#h",
            "tauri://localhost/assets/%E2%9C%93.svg",
        ] {
            assert_eq!(to_tauri_url(to_servo_url(url(tauri))).as_str(), tauri);
        }
    }

    fn origin_of(headers: &HeaderMap) -> Option<&str> {
        headers.get(ORIGIN).map(|v| v.to_str().unwrap())
    }

    #[test]
    fn a_page_s_origin_is_added_and_an_app_page_s_is_tauri_s() {
        let mut headers = HeaderMap::new();
        add_origin(&mut headers, Some("http://127.0.0.1:47123"));
        assert_eq!(origin_of(&headers), Some("http://127.0.0.1:47123"));

        let mut headers = HeaderMap::new();
        add_origin(&mut headers, Some("http://tauri.localhost"));
        assert_eq!(origin_of(&headers), Some("tauri://localhost"));

        // Another port on the app host is someone else's server.
        let mut headers = HeaderMap::new();
        add_origin(&mut headers, Some("http://tauri.localhost:8080"));
        assert_eq!(origin_of(&headers), Some("http://tauri.localhost:8080"));
    }

    #[test]
    fn an_origin_header_already_there_is_kept() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("https://example.com"));
        add_origin(&mut headers, Some("http://tauri.localhost"));
        assert_eq!(origin_of(&headers), Some("https://example.com"));
    }

    #[test]
    fn an_opaque_origin_adds_no_header() {
        let mut headers = HeaderMap::new();
        add_origin(&mut headers, None);
        assert_eq!(origin_of(&headers), None);
    }

    #[test]
    fn tauri_s_request_carries_method_headers_uri_and_body() {
        let mut headers = HeaderMap::new();
        headers.insert("x-probe", HeaderValue::from_static("hdr"));
        let uri = "ipc://localhost/reverse_bytes".parse().unwrap();
        let request = tauri_request(Method::POST, headers, uri, vec![1, 2, 3]);
        assert_eq!(*request.method(), Method::POST);
        assert_eq!(request.uri(), "ipc://localhost/reverse_bytes");
        assert_eq!(request.headers()["x-probe"], "hdr");
        assert_eq!(request.body(), &[1, 2, 3]);
    }

    #[test]
    fn statuses_without_a_standard_reason_have_an_empty_one() {
        assert_eq!(reason_phrase(StatusCode::OK), b"OK");
        assert_eq!(reason_phrase(StatusCode::NOT_FOUND), b"Not Found");
        assert_eq!(reason_phrase(StatusCode::from_u16(599).unwrap()), b"");
    }
}
