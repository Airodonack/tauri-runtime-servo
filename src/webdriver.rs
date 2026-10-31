//! Servo's WebDriver server, for end-to-end tests: with the `webdriver`
//! feature, started with Servo when [`PORT_VARIABLE`] names a port.
//!
//! The server (the fork's `webdriver_server` crate) speaks the W3C protocol
//! and sends what the page's engine can't do alone to the embedder as
//! [`WebDriverCommandMsg`]s; the runtime handles those the way servoshell
//! does (`ports/servoshell/webdriver.rs`): finding webviews, window
//! geometry, the page's dialogs as user prompts, loads and their status,
//! input events and screenshots. Everything else goes back to Servo with
//! `execute_webdriver_command`. What a Tauri app's runtime can't do (open a
//! window of its own) is answered with an error: the reply channel is
//! dropped, which the server reports to the client.
//!
//! The server listens on the loopback interface only (127.0.0.1), so only
//! programs on the same machine can reach it, but it runs any script it is
//! sent: only test builds should have the feature.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crossbeam_channel::{Receiver, Sender};
use euclid::{Point2D, Size2D};
use servo::{
    DeviceIndependentPixel, DeviceIntPoint, DeviceIntRect, DeviceIntSize, GenericSender,
    InputEventId, TraversalId, WebDriverCommandMsg, WebDriverJSResult, WebDriverLoadStatus,
    WebDriverScriptCommand, WebViewId, WebViewRect, convert_rect_to_css_pixel,
};
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::window::Window;
use tauri_runtime::UserEvent;
use tauri_runtime::window::WindowId;

use crate::runtime::{Context, Message};
use crate::webview::{ServoWebview, WebviewKind, WebviewMessage};
use crate::window::WindowMessage;

/// The environment variable naming the port Servo's WebDriver server
/// listens on (a test build only: the `webdriver` feature). Unset, empty
/// or not a port: no server.
pub const PORT_VARIABLE: &str = "TAURI_RUNTIME_SERVO_WEBDRIVER_PORT";

/// The port [`PORT_VARIABLE`] names, if any.
pub fn port_from_env() -> Option<u16> {
    let value = std::env::var(PORT_VARIABLE)
        .ok()
        .filter(|v| !v.is_empty())?;
    match value.parse() {
        Ok(port) => Some(port),
        Err(_) => {
            log::error!("{PORT_VARIABLE}={value} is not a port: no WebDriver server");
            None
        }
    }
}

/// What the runtime keeps for the WebDriver server: its commands, and the
/// replies it is owed once Servo tells the delegates (a load complete, a
/// traversal done, an input event handled, a dialog interrupting a script).
#[derive(Default)]
pub(crate) struct WebDriver {
    receiver: RefCell<Option<Receiver<WebDriverCommandMsg>>>,
    load_status_senders: RefCell<HashMap<WebViewId, GenericSender<WebDriverLoadStatus>>>,
    pending_traversals: RefCell<HashMap<TraversalId, GenericSender<WebDriverLoadStatus>>>,
    script_interrupt: RefCell<Option<GenericSender<WebDriverJSResult>>>,
    pending_input_events: RefCell<HashMap<InputEventId, (WebViewId, Sender<()>)>>,
    /// The webview WebDriver picked for interaction (its "current
    /// top-level browsing context").
    selected: RefCell<Option<WebViewId>>,
}

impl WebDriver {
    /// Starts the server on `port`; its commands are read in
    /// [`Context::handle_webdriver_messages`].
    #[cfg(feature = "webdriver")]
    pub fn start(
        &self,
        port: u16,
        waker: Box<dyn servo::EventLoopWaker>,
        preferences: servo::Preferences,
    ) {
        // Loopback only: a client that reaches the server can run script in
        // the app's pages and so call its commands, which no other machine
        // on the network should be able to do.
        let address = std::net::Ipv4Addr::LOCALHOST;
        // The crate panics on its own thread when it can't listen: try the
        // port first, so a port in use is an error in the log, not a panic.
        if let Err(e) = std::net::TcpListener::bind((address, port)) {
            log::error!("WebDriver can't listen on {address}:{port}: {e}");
            return;
        }
        let (sender, receiver) = crossbeam_channel::unbounded();
        webdriver_server::start_server(address.into(), port, sender, waker, preferences);
        log::info!("WebDriver server on {address}:{port}");
        *self.receiver.borrow_mut() = Some(receiver);
    }

    #[cfg(not(feature = "webdriver"))]
    pub fn start(
        &self,
        port: u16,
        _waker: Box<dyn servo::EventLoopWaker>,
        _preferences: servo::Preferences,
    ) {
        log::error!(
            "{PORT_VARIABLE}={port}, but tauri-runtime-servo was built without its `webdriver` feature"
        );
    }

    pub fn is_active(&self) -> bool {
        self.receiver.borrow().is_some()
    }

    /// A page load finished: a navigation WebDriver waits on is done.
    pub fn load_complete(&self, webview: WebViewId) {
        if let Some(sender) = self.load_status_senders.borrow_mut().remove(&webview) {
            let _ = sender.send(WebDriverLoadStatus::Complete);
        }
    }

    pub fn traversal_complete(&self, traversal: TraversalId) {
        if let Some(sender) = self.pending_traversals.borrow_mut().remove(&traversal) {
            let _ = sender.send(WebDriverLoadStatus::Complete);
        }
    }

    pub fn input_event_handled(&self, event: InputEventId) {
        if let Some((_, sender)) = self.pending_input_events.borrow_mut().remove(&event) {
            let _ = sender.send(());
        }
    }

    /// The page opened a dialog: per "execute a function body", a script
    /// being run returns null at once, and a load waiting on the page is
    /// blocked by the prompt.
    pub fn dialog_shown(&self, webview: WebViewId) {
        if !self.is_active() {
            return;
        }
        if let Some(sender) = &*self.script_interrupt.borrow() {
            let _ = sender.send(Ok(servo::JSValue::Null));
        }
        if let Some(sender) = self.load_status_senders.borrow().get(&webview) {
            let _ = sender.send(WebDriverLoadStatus::Blocked);
        }
    }

    /// Forgets what was owed to a webview that is gone.
    pub fn webview_closed(&self, webview: WebViewId) {
        self.load_status_senders.borrow_mut().remove(&webview);
        self.pending_input_events
            .borrow_mut()
            .retain(|_, (id, _)| *id != webview);
    }

    fn handle_script_command(&self, command: &WebDriverScriptCommand) {
        match command {
            // WebDriver runs one script at a time: the newest is the one a
            // dialog interrupts.
            WebDriverScriptCommand::ExecuteScriptWithCallback(_, sender) => {
                *self.script_interrupt.borrow_mut() = Some(sender.clone());
            }
            WebDriverScriptCommand::AddLoadStatusSender(webview, sender) => {
                self.load_status_senders
                    .borrow_mut()
                    .insert(*webview, sender.clone());
            }
            WebDriverScriptCommand::RemoveLoadStatusSender(webview) => {
                self.load_status_senders.borrow_mut().remove(webview);
            }
            _ => *self.script_interrupt.borrow_mut() = None,
        }
    }
}

/// A webview found by its Servo id, with its window.
struct Found {
    window_id: WindowId,
    window: Rc<Window>,
    webview: Rc<ServoWebview>,
    webview_id: crate::runtime::WebviewId,
}

impl<T: UserEvent> Context<T> {
    fn find_webview(&self, id: WebViewId) -> Option<Found> {
        let windows = self.main_thread.windows.0.borrow();
        windows.iter().find_map(|(window_id, w)| {
            let window = w.inner.clone()?;
            let webview = w.webviews.iter().find(|v| v.inner.webview.id() == id)?;
            Some(Found {
                window_id: *window_id,
                window,
                webview: webview.inner.clone(),
                webview_id: webview.id,
            })
        })
    }

    /// Every open webview, window by window, each window's content first.
    fn all_webviews(&self) -> Vec<(Rc<Window>, WebViewId)> {
        let windows = self.main_thread.windows.0.borrow();
        windows
            .values()
            .filter_map(|w| Some((w.inner.clone()?, &w.webviews)))
            .flat_map(|(window, webviews)| {
                webviews
                    .iter()
                    .map(move |v| (window.clone(), v.inner.webview.id()))
            })
            .collect()
    }

    /// Answers what the WebDriver server has sent. Main thread only, outside
    /// any borrow of the windows (a command may close one).
    pub(crate) fn handle_webdriver_messages(&self) {
        let webdriver = self.main_thread.webdriver.clone();
        let Some(receiver) = webdriver.receiver.borrow().clone() else {
            return;
        };
        while let Ok(message) = receiver.try_recv() {
            self.handle_webdriver_message(&webdriver, message);
        }
    }

    fn handle_webdriver_message(&self, webdriver: &WebDriver, message: WebDriverCommandMsg) {
        use WebDriverCommandMsg as M;
        // For a webview that is gone, a reply channel is dropped unanswered:
        // the server reports the error to the client.
        match message {
            M::GetAllWebViews(sender) => {
                let ids = self.all_webviews().into_iter().map(|(_, id)| id).collect();
                let _ = sender.send(ids);
            }
            M::GetWebViewSelectedForInteraction(sender) => {
                let webviews = self.all_webviews();
                let selected = *webdriver.selected.borrow();
                let chosen = selected
                    .filter(|id| webviews.iter().any(|(_, open)| open == id))
                    .or_else(|| {
                        webviews
                            .iter()
                            .find(|(window, _)| window.is_focused())
                            .or(webviews.first())
                            .map(|(_, id)| *id)
                    });
                let _ = sender.send(chosen);
            }
            M::SelectWebViewForInteraction(id) => {
                if let Some(found) = self.find_webview(id) {
                    *webdriver.selected.borrow_mut() = Some(id);
                    found.webview.webview.show();
                    found.webview.webview.set_focused(true);
                }
            }
            M::IsWebViewOpen(id, sender) => {
                let _ = sender.send(self.find_webview(id).is_some());
            }
            M::GetWindowRect(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let _ = sender.send(window_rect(&found.window));
                }
            }
            M::SetWindowRect(id, requested, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let window = &found.window;
                    let scale = window.scale_factor() as f32;
                    let scale =
                        euclid::Scale::<f32, DeviceIndependentPixel, servo::DevicePixel>::new(
                            scale,
                        );
                    let requested = (requested.to_f32() * scale).round().to_i32();
                    // WebDriver's size is the outer one; tao sets the inner.
                    let outer = window.outer_size();
                    let inner = window.inner_size();
                    let frame_width = outer.width.saturating_sub(inner.width);
                    let frame_height = outer.height.saturating_sub(inner.height);
                    let size = requested.size();
                    window.set_inner_size(PhysicalSize::new(
                        (size.width.max(1) as u32)
                            .saturating_sub(frame_width)
                            .max(1),
                        (size.height.max(1) as u32)
                            .saturating_sub(frame_height)
                            .max(1),
                    ));
                    window.set_outer_position(PhysicalPosition::new(
                        requested.min.x,
                        requested.min.y,
                    ));
                    let _ = sender.send(window_rect(window));
                }
            }
            M::MaximizeWebView(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    found.window.set_maximized(true);
                    let _ = sender.send(window_rect(&found.window));
                }
            }
            M::GetViewportSize(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let bounds = found.webview.bounds.get();
                    let scale = found.window.scale_factor() as f32;
                    let size: Size2D<f32, DeviceIndependentPixel> =
                        Size2D::new(bounds.width as f32 / scale, bounds.height as f32 / scale);
                    let _ = sender.send(size);
                }
            }
            M::NewWindow(type_hint, _sender, _load_status_sender) => {
                // Tauri's app makes its windows; the runtime has none to
                // offer of its own.
                log::warn!(
                    "WebDriver asked for a new {type_hint:?} window: not supported by tauri-runtime-servo"
                );
            }
            M::CloseWebView(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    // As the person would: a window's content closes with its
                    // window (through the app, which may keep it open), a
                    // child webview on its own. Through the loop: closing a
                    // window needs the app's callback.
                    let message = match found.webview.kind {
                        WebviewKind::WindowContent => {
                            Message::Window(found.window_id, WindowMessage::Close)
                        }
                        WebviewKind::WindowChild => Message::Webview(
                            found.window_id,
                            found.webview_id,
                            WebviewMessage::Close,
                        ),
                    };
                    webdriver.webview_closed(id);
                    if self.proxy.send_event(message).is_ok() {
                        let _ = sender.send(());
                    }
                }
            }
            M::ResetAllCookies(sender) => {
                let servo = self.main_thread.servo.borrow().clone();
                match servo {
                    Some(servo) => {
                        servo
                            .site_data_manager()
                            .clear_cookies(Some(Box::new(move || {
                                let _ = sender.send(());
                            })))
                    }
                    None => {
                        let _ = sender.send(());
                    }
                }
            }
            M::Shutdown => {
                // As `request_exit`: through the loop, which asks the app.
                let _ = self.proxy.send_event(Message::RequestExit(0));
            }
            M::LoadUrl(id, url, load_status_sender) => {
                if let Some(found) = self.find_webview(id) {
                    // A dialog open on the old page goes with it.
                    while found.webview.dialogs.current_prompt().is_some()
                        && found
                            .webview
                            .dialogs
                            .respond(servo::WebDriverUserPromptAction::Dismiss)
                            .is_ok()
                    {}
                    webdriver
                        .load_status_senders
                        .borrow_mut()
                        .insert(id, load_status_sender);
                    found.webview.webview.load(url);
                }
            }
            M::Refresh(id, load_status_sender) => {
                if let Some(found) = self.find_webview(id) {
                    webdriver
                        .load_status_senders
                        .borrow_mut()
                        .insert(id, load_status_sender);
                    found.webview.webview.reload();
                }
            }
            M::GoBack(id, load_status_sender) => {
                if let Some(found) = self.find_webview(id) {
                    let traversal = found.webview.webview.go_back(1);
                    webdriver
                        .pending_traversals
                        .borrow_mut()
                        .insert(traversal, load_status_sender);
                }
            }
            M::GoForward(id, load_status_sender) => {
                if let Some(found) = self.find_webview(id) {
                    let traversal = found.webview.webview.go_forward(1);
                    webdriver
                        .pending_traversals
                        .borrow_mut()
                        .insert(traversal, load_status_sender);
                }
            }
            M::InputEvent(id, event, sender) => match self.find_webview(id) {
                Some(found) => {
                    let event_id = found.webview.webview.notify_input_event(event);
                    if let Some(sender) = sender {
                        webdriver
                            .pending_input_events
                            .borrow_mut()
                            .insert(event_id, (id, sender));
                    }
                }
                None => log::warn!("WebDriver input event for a webview that is gone"),
            },
            M::TakeScreenshot(id, rect, sender) => match self.find_webview(id) {
                Some(found) => {
                    let rect = rect.map(|rect| WebViewRect::Page(rect.to_box2d()));
                    found.webview.webview.take_screenshot(rect, move |result| {
                        let _ = sender.send(result);
                    });
                }
                None => {
                    let _ = sender.send(Err(servo::ScreenshotCaptureError::WebViewDoesNotExist));
                }
            },
            M::CurrentUserPrompt(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let _ = sender.send(found.webview.dialogs.current_prompt());
                }
            }
            M::HandleUserPrompt(id, action, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let _ = sender.send(found.webview.dialogs.respond(action));
                }
            }
            M::GetAlertText(id, sender) => {
                if let Some(found) = self.find_webview(id) {
                    let _ = sender.send(found.webview.dialogs.current_message().ok_or(()));
                }
            }
            M::SendAlertText(id, text) => {
                if let Some(found) = self.find_webview(id) {
                    found.webview.dialogs.set_current_text(&text);
                }
            }
            M::ScriptCommand(_, ref command) => {
                webdriver.handle_script_command(command);
                self.forward_to_servo(message);
            }
            message @ (M::FocusBrowsingContext(..) | M::IsBrowsingContextOpen(..)) => {
                self.forward_to_servo(message)
            }
        }
    }

    fn forward_to_servo(&self, message: WebDriverCommandMsg) {
        let servo = self.main_thread.servo.borrow().clone();
        match servo {
            Some(servo) => servo.execute_webdriver_command(message),
            None => log::warn!("a WebDriver command before Servo was built"),
        }
    }
}

/// The window's outer rectangle in device-independent pixels, as WebDriver's
/// "window rect".
fn window_rect(window: &Window) -> servo::DeviceIndependentIntRect {
    let outer = window.outer_size();
    let origin = window
        .outer_position()
        .map(|p| DeviceIntPoint::new(p.x, p.y))
        .unwrap_or_else(|_| Point2D::zero());
    convert_rect_to_css_pixel(
        DeviceIntRect::from_origin_and_size(
            origin,
            DeviceIntSize::new(outer.width as i32, outer.height as i32),
        ),
        euclid::Scale::new(window.scale_factor() as f32),
    )
}
