//! Webviews: a Servo [`WebView`] on an X11 child window of the tao window,
//! its delegate, the webview messages and the [`WebviewDispatch`]
//! implementation.

use std::any::Any;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use euclid::Scale;
use gtk::prelude::*;
use servo::{
    EmbedderControl, EmbedderControlId, LoadStatus, NavigationRequest, RenderingContext,
    Theme as ServoTheme, WebView, WebViewBuilder, WebViewDelegate, WindowRenderingContext,
};
use tao::platform::unix::WindowExtUnix;
use tao::window::Window;
use tauri_runtime::dpi::{PhysicalPosition, PhysicalSize, Position, Rect, Size};
use tauri_runtime::webview::{PageLoadEvent, PendingWebview};
use tauri_runtime::window::{WebviewEvent, WindowId};
use tauri_runtime::{Cookie, Error, Result, UserEvent, WebviewDispatch, WebviewEventId};
use tauri_utils::config::Color;
use url::Url;

use crate::dialog::OpenDialogs;
use crate::input;
use crate::runtime::{Context, Message, ServoRuntime, WebviewId};
use crate::x11::ChildWindow;

pub type WebviewEventHandler = Box<dyn Fn(&WebviewEvent) + Send>;
pub type WebviewEventListeners = Arc<Mutex<HashMap<WebviewEventId, WebviewEventHandler>>>;

/// tauri-runtime has no "not supported" error; this one says what is
/// missing (the variant's text reads "failed to create webview: ...").
pub(crate) fn unsupported(what: &str) -> Error {
    Error::CreateWebview(format!("{what} is not supported by tauri-runtime-servo yet").into())
}

/// Whether the webview is the window's whole content (made with the window)
/// or a child placed by bounds (made with `create_webview`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebviewKind {
    WindowContent,
    WindowChild,
}

/// The webview's place in its window, in physical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A webview as the main thread holds it.
pub struct ServoWebview {
    // Field order is drop order: the WebView, then its rendering context,
    // then the child window the context draws on.
    pub webview: WebView,
    pub rendering_context: Rc<WindowRenderingContext>,
    pub surface: ChildWindow,
    pub kind: WebviewKind,
    pub bounds: Cell<Bounds>,
    pub auto_resize: Cell<bool>,
    pub listeners: WebviewEventListeners,
}

impl ServoWebview {
    /// Moves and sizes the child window and the page to `bounds`.
    pub fn set_bounds(&self, bounds: Bounds) {
        self.bounds.set(bounds);
        self.surface
            .set_geometry(bounds.x, bounds.y, bounds.width, bounds.height);
        self.webview
            .resize(PhysicalSize::new(bounds.width.max(1), bounds.height.max(1)));
    }

    /// Paints the page's latest frame and shows it.
    pub fn paint(&self) {
        self.webview.paint();
        self.rendering_context.present();
    }
}

#[derive(Clone)]
pub struct WebviewWrapper {
    pub label: String,
    pub id: WebviewId,
    pub inner: Rc<ServoWebview>,
}

impl fmt::Debug for WebviewWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebviewWrapper")
            .field("label", &self.label)
            .field("id", &self.id)
            .finish()
    }
}

// --- The delegate ----------------------------------------------------------

/// Servo's callbacks for one webview: repaint requests, page loads, title,
/// navigation policy and the controls Servo asks the embedder to draw.
struct Delegate {
    window: Weak<Window>,
    dialogs: Rc<OpenDialogs>,
    on_page_load: Option<Box<OnPageLoadHandler>>,
    navigation_handler: Option<Box<NavigationHandler>>,
    document_title_changed: Option<Box<DocumentTitleChangedHandler>>,
}

// tauri-runtime's names for these; its own aliases are private.
type OnPageLoadHandler = dyn Fn(Url, PageLoadEvent) + Send;
type NavigationHandler = dyn Fn(&Url) -> bool + Send;
type DocumentTitleChangedHandler = dyn Fn(String) + Send;

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        if let Some(window) = self.window.upgrade() {
            window.request_redraw();
        }
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        let event = match status {
            LoadStatus::Started => PageLoadEvent::Started,
            LoadStatus::Complete => PageLoadEvent::Finished,
            _ => return,
        };
        if let (Some(handler), Some(url)) = (&self.on_page_load, webview.url()) {
            handler(url, event);
        }
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        if let Some(handler) = &self.document_title_changed {
            handler(title.unwrap_or_default());
        }
    }

    fn request_navigation(&self, _webview: WebView, request: NavigationRequest) {
        match &self.navigation_handler {
            Some(handler) if !handler(&request.url) => request.deny(),
            _ => request.allow(),
        }
    }

    fn show_embedder_control(&self, _webview: WebView, control: EmbedderControl) {
        let id = control.id();
        match control {
            EmbedderControl::SimpleDialog(dialog) => {
                let parent = self
                    .window
                    .upgrade()
                    .map(|window| window.gtk_window().clone());
                self.dialogs.show(parent.as_ref(), id, dialog);
            }
            // Dropping a control answers it with its default (nothing chosen).
            EmbedderControl::SelectElement(_) => {
                log::warn!("embedder control not drawn yet: <select> picker")
            }
            EmbedderControl::ColorPicker(_) => {
                log::warn!("embedder control not drawn yet: colour picker")
            }
            EmbedderControl::FilePicker(_) => {
                log::warn!("embedder control not drawn yet: file picker")
            }
            EmbedderControl::InputMethod(_) => {
                log::warn!("embedder control not drawn yet: input method")
            }
            EmbedderControl::ContextMenu(_) => {
                log::warn!("embedder control not drawn yet: context menu")
            }
        }
    }

    fn hide_embedder_control(&self, _webview: WebView, id: EmbedderControlId) {
        if !self.dialogs.hide(id) {
            log::debug!("hide_embedder_control for a control not shown: {id:?}");
        }
    }

    fn request_create_new(&self, _parent: WebView, _request: servo::CreateNewWebViewRequest) {
        log::warn!("embedder request not handled yet: open a new webview (window.open)");
    }

    fn request_permission(&self, _webview: WebView, request: servo::PermissionRequest) {
        log::warn!(
            "embedder request not handled yet: permission {:?} (denied)",
            request.feature()
        );
    }

    fn request_authentication(&self, _webview: WebView, _request: servo::AuthenticationRequest) {
        log::warn!("embedder request not handled yet: HTTP authentication (cancelled)");
    }

    fn request_unload(&self, _webview: WebView, request: servo::AllowOrDenyRequest) {
        request.allow();
    }

    fn request_move_to(&self, _webview: WebView, _point: servo::DeviceIntPoint) {
        log::warn!("embedder request not handled yet: move the window");
    }

    fn request_resize_to(&self, _webview: WebView, _size: servo::DeviceIntSize) {
        log::warn!("embedder request not handled yet: resize the window");
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        log::error!("the page crashed: {reason}");
    }

    fn show_console_message(
        &self,
        _webview: WebView,
        level: servo::ConsoleLogLevel,
        message: String,
    ) {
        log::info!(target: "console", "{level:?}: {message}");
    }
}

// --- Creation (on the main thread) -----------------------------------------

pub fn create_webview<T: UserEvent>(
    kind: WebviewKind,
    window: &Rc<Window>,
    id: WebviewId,
    context: &Context<T>,
    pending: PendingWebview<T, ServoRuntime<T>>,
) -> Result<WebviewWrapper> {
    let PendingWebview {
        label,
        webview_attributes: attributes,
        uri_scheme_protocols,
        ipc_handler,
        navigation_handler,
        document_title_changed_handler,
        url,
        on_page_load_handler,
        ..
    } = pending;

    // What T3 wires; said once per webview so a page that needs it explains itself.
    if !uri_scheme_protocols.is_empty() {
        let mut schemes: Vec<_> = uri_scheme_protocols.keys().cloned().collect();
        schemes.sort();
        log::warn!(
            "webview {label}: custom URI schemes not registered yet: {}",
            schemes.join(", ")
        );
    }
    if !attributes.initialization_scripts.is_empty() {
        log::warn!(
            "webview {label}: {} initialization scripts not injected yet",
            attributes.initialization_scripts.len()
        );
    }
    if ipc_handler.is_some() {
        log::warn!("webview {label}: the IPC (postMessage) handler is not wired yet");
    }

    let url = Url::parse(&url).map_err(|e| Error::CreateWebview(Box::new(e)))?;
    let scale_factor = window.scale_factor();
    let inner_size = window.inner_size();
    let bounds = match (kind, attributes.bounds) {
        (WebviewKind::WindowChild, Some(Rect { position, size })) => {
            let position = position.to_physical::<i32>(scale_factor);
            let size = size.to_physical::<u32>(scale_factor);
            Bounds {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
            }
        }
        _ => Bounds {
            x: 0,
            y: 0,
            width: inner_size.width,
            height: inner_size.height,
        },
    };

    // A hidden window has no GdkWindow until it is realized.
    let gtk_window = window.gtk_window();
    if !gtk_window.is_realized() {
        gtk_window.realize();
    }
    let gdk_window = gtk_window
        .window()
        .ok_or_else(|| Error::CreateWebview("the window has no GdkWindow".into()))?;
    let surface = ChildWindow::new(&gdk_window, bounds.x, bounds.y, bounds.width, bounds.height)
        .map_err(|e| Error::CreateWebview(e.into()))?;
    let rendering_context = Rc::new(
        WindowRenderingContext::new(
            surface.display_handle(),
            surface.window_handle(),
            PhysicalSize::new(bounds.width.max(1), bounds.height.max(1)),
        )
        .map_err(|e| Error::CreateWebview(format!("no rendering context: {e:?}").into()))?,
    );
    rendering_context.make_current().map_err(|e| {
        Error::CreateWebview(format!("rendering context not current: {e:?}").into())
    })?;

    let delegate = Rc::new(Delegate {
        window: Rc::downgrade(window),
        dialogs: Default::default(),
        on_page_load: on_page_load_handler,
        navigation_handler,
        document_title_changed: document_title_changed_handler,
    });
    let servo = context.servo();
    let webview = WebViewBuilder::new(&servo, rendering_context.clone())
        .url(url)
        .hidpi_scale_factor(Scale::new(scale_factor as f32))
        .delegate(delegate)
        .build();
    if attributes.focus {
        webview.set_focused(true);
    }
    if let Some(theme) = to_servo_theme(window.theme()) {
        webview.notify_theme_change(theme);
    }

    let inner = Rc::new(ServoWebview {
        webview,
        rendering_context,
        surface,
        kind,
        bounds: Cell::new(bounds),
        auto_resize: Cell::new(attributes.auto_resize),
        listeners: Default::default(),
    });
    let weak = Rc::downgrade(&inner);
    input::connect_discrete_scroll(gtk_window, move || {
        let inner = weak.upgrade()?;
        let b = inner.bounds.get();
        Some((inner.webview.clone(), (b.x, b.y, b.width, b.height)))
    });
    Ok(WebviewWrapper { label, id, inner })
}

pub fn to_servo_theme(theme: tao::window::Theme) -> Option<ServoTheme> {
    match theme {
        tao::window::Theme::Light => Some(ServoTheme::Light),
        tao::window::Theme::Dark => Some(ServoTheme::Dark),
        _ => None,
    }
}

// --- Messages --------------------------------------------------------------

pub enum WebviewMessage {
    AddEventListener(WebviewEventId, WebviewEventHandler),
    EvaluateScript(String),
    Navigate(Url),
    Reload,
    Close,
    Show,
    Hide,
    SetPosition(Position),
    SetSize(Size),
    SetBounds(Rect),
    SetFocus,
    SetAutoResize(bool),
    SetZoom(f64),
    // Getters
    Url(Sender<Result<String>>),
    Bounds(Sender<Result<Rect>>),
    Position(Sender<Result<PhysicalPosition<i32>>>),
    Size(Sender<Result<PhysicalSize<u32>>>),
    WithWebview(Box<dyn FnOnce(Box<dyn Any>) + Send>),
}

/// Runs a webview message on the main thread. A webview that is gone drops
/// the message, and with it any reply channel, so a getter's caller gets
/// `FailedToReceiveMessage`.
pub fn handle_webview_message<T: UserEvent>(
    context: &Context<T>,
    window_id: WindowId,
    webview_id: WebviewId,
    message: WebviewMessage,
) {
    let windows = &context.main_thread.windows;
    if let WebviewMessage::Close = message {
        let removed = windows.0.borrow_mut().get_mut(&window_id).and_then(|w| {
            let index = w.webviews.iter().position(|v| v.id == webview_id)?;
            Some(w.webviews.remove(index))
        });
        // Dropped outside the borrow: Servo may call back while closing.
        drop(removed);
        return;
    }
    let found = windows.0.borrow().get(&window_id).and_then(|w| {
        let webview = w
            .webviews
            .iter()
            .find(|v| v.id == webview_id)?
            .inner
            .clone();
        Some((w.inner.clone()?, webview))
    });
    let Some((window, webview)) = found else {
        log::debug!("message for a webview that is gone");
        return;
    };
    let scale_factor = window.scale_factor();
    let bounds = webview.bounds.get();
    match message {
        WebviewMessage::AddEventListener(id, listener) => {
            webview.listeners.lock().unwrap().insert(id, listener);
        }
        WebviewMessage::EvaluateScript(script) => {
            webview.webview.evaluate_javascript(script, |result| {
                if let Err(error) = result {
                    log::warn!("eval_script failed: {error:?}");
                }
            });
        }
        WebviewMessage::Navigate(url) => webview.webview.load(url),
        WebviewMessage::Reload => webview.webview.reload(),
        WebviewMessage::Close => {} // handled above
        WebviewMessage::Show => {
            webview.surface.set_visible(true);
            webview.webview.show();
        }
        WebviewMessage::Hide => {
            webview.webview.hide();
            webview.surface.set_visible(false);
        }
        WebviewMessage::SetPosition(position) => {
            let position = position.to_physical::<i32>(scale_factor);
            webview.set_bounds(Bounds {
                x: position.x,
                y: position.y,
                ..bounds
            });
        }
        WebviewMessage::SetSize(size) => {
            let size = size.to_physical::<u32>(scale_factor);
            webview.set_bounds(Bounds {
                width: size.width,
                height: size.height,
                ..bounds
            });
        }
        WebviewMessage::SetBounds(Rect { position, size }) => {
            let position = position.to_physical::<i32>(scale_factor);
            let size = size.to_physical::<u32>(scale_factor);
            webview.set_bounds(Bounds {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
            });
        }
        WebviewMessage::SetFocus => webview.webview.set_focused(true),
        WebviewMessage::SetAutoResize(auto_resize) => webview.auto_resize.set(auto_resize),
        WebviewMessage::SetZoom(scale) => webview.webview.set_page_zoom(scale as f32),
        WebviewMessage::Url(tx) => {
            let _ = tx.send(Ok(webview
                .webview
                .url()
                .map(|url| url.to_string())
                .unwrap_or_default()));
        }
        WebviewMessage::Bounds(tx) => {
            let _ = tx.send(Ok(Rect {
                position: PhysicalPosition::new(bounds.x, bounds.y).into(),
                size: PhysicalSize::new(bounds.width, bounds.height).into(),
            }));
        }
        WebviewMessage::Position(tx) => {
            let _ = tx.send(Ok(PhysicalPosition::new(bounds.x, bounds.y)));
        }
        WebviewMessage::Size(tx) => {
            let _ = tx.send(Ok(PhysicalSize::new(bounds.width, bounds.height)));
        }
        WebviewMessage::WithWebview(f) => f(Box::new(webview.webview.clone())),
    }
    window.request_redraw();
}

// --- The dispatcher --------------------------------------------------------

/// The Tauri [`WebviewDispatch`] for [`ServoRuntime`].
#[derive(Debug, Clone)]
pub struct ServoWebviewDispatcher<T: UserEvent> {
    pub(crate) window_id: Arc<Mutex<WindowId>>,
    pub(crate) webview_id: WebviewId,
    pub(crate) context: Context<T>,
}

macro_rules! webview_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        $self.send($message(tx))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)?
    }};
}

impl<T: UserEvent> ServoWebviewDispatcher<T> {
    fn send(&self, message: WebviewMessage) -> Result<()> {
        let window_id = *self.window_id.lock().unwrap();
        self.context
            .send_user_message(Message::Webview(window_id, self.webview_id, message))
    }
}

impl<T: UserEvent> WebviewDispatch<T> for ServoWebviewDispatcher<T> {
    type Runtime = ServoRuntime<T>;

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        self.context.send_user_message(Message::Task(Box::new(f)))
    }

    fn on_webview_event<F: Fn(&WebviewEvent) + Send + 'static>(&self, f: F) -> WebviewEventId {
        let id = self.context.next_webview_event_id();
        let _ = self.send(WebviewMessage::AddEventListener(id, Box::new(f)));
        id
    }

    /// Hands out the Servo [`WebView`] (a clone of the handle), on the main
    /// thread: `webview.downcast_ref::<servo::WebView>()`.
    fn with_webview<F: FnOnce(Box<dyn Any>) + Send + 'static>(&self, f: F) -> Result<()> {
        self.send(WebviewMessage::WithWebview(Box::new(f)))
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn open_devtools(&self) {
        // Servo has no inspector window; its devtools server (the
        // `devtools_server_enabled` preference) serves Firefox's remote
        // devtools instead. Not started yet.
        log::warn!(
            "open_devtools: Servo has no built-in inspector (Firefox remote devtools not wired yet)"
        );
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn close_devtools(&self) {}

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn is_devtools_open(&self) -> Result<bool> {
        Ok(false)
    }

    fn url(&self) -> Result<String> {
        webview_getter!(self, WebviewMessage::Url)
    }

    fn bounds(&self) -> Result<Rect> {
        webview_getter!(self, WebviewMessage::Bounds)
    }

    fn position(&self) -> Result<PhysicalPosition<i32>> {
        webview_getter!(self, WebviewMessage::Position)
    }

    fn size(&self) -> Result<PhysicalSize<u32>> {
        webview_getter!(self, WebviewMessage::Size)
    }

    fn navigate(&self, url: Url) -> Result<()> {
        self.send(WebviewMessage::Navigate(url))
    }

    fn reload(&self) -> Result<()> {
        self.send(WebviewMessage::Reload)
    }

    fn print(&self) -> Result<()> {
        Err(unsupported("print"))
    }

    fn close(&self) -> Result<()> {
        self.send(WebviewMessage::Close)
    }

    fn set_bounds(&self, bounds: Rect) -> Result<()> {
        self.send(WebviewMessage::SetBounds(bounds))
    }

    fn set_size(&self, size: Size) -> Result<()> {
        self.send(WebviewMessage::SetSize(size))
    }

    fn set_position(&self, position: Position) -> Result<()> {
        self.send(WebviewMessage::SetPosition(position))
    }

    fn set_focus(&self) -> Result<()> {
        self.send(WebviewMessage::SetFocus)
    }

    fn hide(&self) -> Result<()> {
        self.send(WebviewMessage::Hide)
    }

    fn show(&self) -> Result<()> {
        self.send(WebviewMessage::Show)
    }

    fn eval_script<S: Into<String>>(&self, script: S) -> Result<()> {
        self.send(WebviewMessage::EvaluateScript(script.into()))
    }

    fn eval_script_with_callback<S: Into<String>>(
        &self,
        _script: S,
        _callback: impl Fn(String) + Send + 'static,
    ) -> Result<()> {
        // Needs Servo's JSValue serialized as wry does (JSON); T3.
        Err(unsupported("eval_script_with_callback"))
    }

    fn reparent(&self, _window_id: WindowId) -> Result<()> {
        Err(unsupported("reparent"))
    }

    fn cookies_for_url(&self, _url: Url) -> Result<Vec<Cookie<'static>>> {
        Err(unsupported("cookies_for_url"))
    }

    fn cookies(&self) -> Result<Vec<Cookie<'static>>> {
        Err(unsupported("cookies"))
    }

    fn set_cookie(&self, _cookie: Cookie<'_>) -> Result<()> {
        Err(unsupported("set_cookie"))
    }

    fn delete_cookie(&self, _cookie: Cookie<'_>) -> Result<()> {
        Err(unsupported("delete_cookie"))
    }

    fn set_auto_resize(&self, auto_resize: bool) -> Result<()> {
        self.send(WebviewMessage::SetAutoResize(auto_resize))
    }

    fn set_zoom(&self, scale_factor: f64) -> Result<()> {
        self.send(WebviewMessage::SetZoom(scale_factor))
    }

    fn set_background_color(&self, _color: Option<Color>) -> Result<()> {
        // Servo's clear colour is a process-wide preference and its surface
        // has no alpha yet (a fork change).
        Err(unsupported("set_background_color"))
    }

    fn clear_all_browsing_data(&self) -> Result<()> {
        Err(unsupported("clear_all_browsing_data"))
    }
}
