//! The runtime: the tao event loop, the messages dispatchers send to it, the
//! main-thread state (windows, webviews, the one `Servo`) and the mapping of
//! tao events to Tauri's `RunEvent`s and to Servo. Structure adapted from
//! tauri-runtime-wry 2.12.1 (Apache-2.0 OR MIT).
//!
//! Servo is single-threaded: the `Servo` handle, its `WebView`s and their
//! rendering contexts live on the main thread, in [`MainThreadContext`].
//! Dispatchers are `Send + Sync` and reach them only through messages:
//! handled at once on the main thread, posted through the tao proxy from
//! anywhere else. Servo wakes the loop through the same proxy
//! ([`Message::ServoWake`]), and the loop spins Servo after every event.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{ThreadId, current as current_thread};

use euclid::Scale;
use raw_window_handle::{DisplayHandle, HasDisplayHandle};
use servo::{EventLoopWaker, Opts, PrefValue, Preferences, Servo, ServoBuilder};
use tao::event::{Event, StartCause, WindowEvent as TaoWindowEvent};
use tao::event_loop::{
    ControlFlow, DeviceEventFilter as TaoDeviceEventFilter, EventLoop, EventLoopBuilder,
    EventLoopProxy as TaoEventLoopProxy, EventLoopWindowTarget,
};
use tao::window::{Window, WindowId as TaoWindowId};
use tauri_runtime::dpi::PhysicalPosition;
use tauri_runtime::monitor::Monitor;
use tauri_runtime::webview::{DetachedWebview, PendingWebview};
use tauri_runtime::window::{
    DetachedWindow, DetachedWindowWebview, PendingWindow, RawWindow, WindowEvent, WindowId,
};
use tauri_runtime::{
    DeviceEventFilter, Error, EventLoopProxy, ExitRequestedEventAction, Result, RunEvent, Runtime,
    RuntimeHandle, RuntimeInitArgs, UserEvent,
};
use tauri_utils::Theme;

use crate::input;
use crate::monitor::to_monitor;
use crate::protocol::{AppPageGuard, MainThreadPoster, SchemeHandlers};
use crate::webdriver::WebDriver;
use crate::webview::{
    Bounds, ServoWebviewDispatcher, WebviewKind, WebviewMessage, WebviewWrapper, create_webview,
    handle_webview_message, to_servo_theme,
};
use crate::window::{
    ServoWindowDispatcher, WindowMessage, WindowWrapper, create_window, handle_window_message,
    map_theme, to_tao_theme,
};

pub type WebviewId = u32;

#[derive(Debug, Clone, Default)]
pub struct WindowIdStore(Arc<Mutex<HashMap<TaoWindowId, WindowId>>>);

impl WindowIdStore {
    pub fn insert(&self, w: TaoWindowId, id: WindowId) {
        self.0.lock().unwrap().insert(w, id);
    }

    pub fn get(&self, w: &TaoWindowId) -> Option<WindowId> {
        self.0.lock().unwrap().get(w).copied()
    }
}

pub struct WindowsStore(pub RefCell<BTreeMap<WindowId, WindowWrapper>>);

impl fmt::Debug for WindowsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not borrowed: formatting can happen while the store is borrowed.
        f.debug_struct("WindowsStore").finish_non_exhaustive()
    }
}

/// What only the main thread touches.
pub struct MainThreadContext<T: UserEvent> {
    pub window_target: EventLoopWindowTarget<Message<T>>,
    pub windows: WindowsStore,
    /// Built at the first webview (Servo needs its protocol registry at
    /// build time, and Tauri hands the schemes over with the webview).
    pub(crate) servo: RefCell<Option<Servo>>,
    /// The WebDriver server's commands and pending replies (`webdriver`).
    pub(crate) webdriver: Rc<WebDriver>,
}

/// The runtime's shared handle: what every dispatcher holds.
#[derive(Clone)]
pub struct Context<T: UserEvent> {
    pub window_id_map: WindowIdStore,
    main_thread_id: ThreadId,
    pub proxy: TaoEventLoopProxy<Message<T>>,
    /// Behind an `Arc` so a clone or drop on another thread only touches an
    /// atomic count; the contents are used on the main thread only.
    pub(crate) main_thread: Arc<MainThreadContext<T>>,
    /// Tauri's URI scheme handlers, which Servo's protocol handlers reach.
    pub(crate) schemes: SchemeHandlers,
    /// Where Servo keeps its data when the first webview names no data
    /// directory: the app's data folder, from its id.
    default_data_dir: Option<PathBuf>,
    next_window_id: Arc<AtomicU32>,
    next_webview_id: Arc<AtomicU32>,
    next_window_event_id: Arc<AtomicU32>,
    next_webview_event_id: Arc<AtomicU32>,
}

// SAFETY: `main_thread` is only dereferenced on the main thread (checked in
// `send_user_message`, or in code that only runs inside the event loop);
// everything else is `Send + Sync`.
unsafe impl<T: UserEvent> Send for Context<T> {}
unsafe impl<T: UserEvent> Sync for Context<T> {}

impl<T: UserEvent> fmt::Debug for Context<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("main_thread_id", &self.main_thread_id)
            .finish_non_exhaustive()
    }
}

impl<T: UserEvent> Context<T> {
    /// Runs the message now when on the main thread, else posts it to the loop.
    pub(crate) fn send_user_message(&self, message: Message<T>) -> Result<()> {
        if current_thread().id() == self.main_thread_id {
            handle_user_message(&self.main_thread.window_target, message, self);
            Ok(())
        } else {
            self.proxy
                .send_event(message)
                .map_err(|_| Error::FailedToSendMessage)
        }
    }

    fn next_window_id(&self) -> WindowId {
        self.next_window_id.fetch_add(1, Ordering::Relaxed).into()
    }

    fn next_webview_id(&self) -> WebviewId {
        self.next_webview_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn next_window_event_id(&self) -> u32 {
        self.next_window_event_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn next_webview_event_id(&self) -> u32 {
        self.next_webview_event_id.fetch_add(1, Ordering::Relaxed)
    }

    /// The one `Servo`, built on first use. Main thread only.
    ///
    /// Servo keeps its client storage (localStorage, IndexedDB), cookies and
    /// HSTS list in one directory for the whole process, so only the first
    /// webview's `data_directory` counts; without one, the app's data folder.
    /// With none at all Servo falls back to a temporary directory, which
    /// its storage then fails to open.
    pub(crate) fn servo(&self, data_directory: Option<&Path>) -> Servo {
        if let Some(servo) = &*self.main_thread.servo.borrow() {
            if data_directory.is_some() {
                log::debug!("Servo is built already: a later webview's data_directory is not used");
            }
            return servo.clone();
        }
        let mut preferences = Preferences {
            // Off by default in Servo (experimental); SvelteKit's link
            // preloader throws without it.
            dom_intersection_observer_enabled: true,
            ..Default::default()
        };
        // Media Source Extensions exist in the fork's libservo only: set by
        // name, so the crate also builds against libservo 0.7.0 as
        // published, which has no such preference.
        const MEDIA_SOURCE: &str = "dom_media_source_enabled";
        if Preferences::exists(MEDIA_SOURCE) {
            preferences.set_value(MEDIA_SOURCE, PrefValue::Bool(true));
        }
        let config_dir = data_directory
            .map(Path::to_path_buf)
            .or_else(|| self.default_data_dir.clone());
        if let Some(dir) = &config_dir
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            log::error!(
                "Servo's data directory {} can't be made: {e}",
                dir.display()
            );
        }
        log::debug!("Servo's data directory: {config_dir:?}");
        let opts = Opts {
            config_dir,
            ..Default::default()
        };
        // Through the proxy even from the main thread: a forwarder runs on
        // Servo's network threads, and the handler must not run inside them.
        let proxy = self.proxy.clone();
        let post: MainThreadPoster = Arc::new(move |task| {
            if proxy.send_event(Message::Task(task)).is_err() {
                log::debug!("a URI scheme request arrived after the event loop ended");
            }
        });
        let webdriver_preferences = preferences.clone();
        let servo = ServoBuilder::default()
            .opts(opts)
            .preferences(preferences)
            .protocol_registry(self.schemes.registry(post))
            .event_loop_waker(Box::new(Waker(self.proxy.clone())))
            .build();
        // Loads for no webview come to the Servo delegate: app pages among
        // them are refused there rather than fetched.
        servo.set_delegate(Rc::new(AppPageGuard(self.schemes.clone())));
        // After the build: the server reads Servo's options, which the
        // build sets once for the process (read first, they'd be defaults
        // and the build would panic).
        if let Some(port) = crate::webdriver::port_from_env() {
            self.main_thread.webdriver.start(
                port,
                Box::new(Waker(self.proxy.clone())),
                webdriver_preferences,
            );
        }
        *self.main_thread.servo.borrow_mut() = Some(servo.clone());
        servo
    }

    /// Lets Servo handle what it has pending. Main thread only.
    fn spin_servo(&self) {
        // Cloned out first: Servo calls the delegates, which may reach the
        // runtime again.
        self.handle_webdriver_messages();
        let servo = self.main_thread.servo.borrow().clone();
        if let Some(servo) = servo {
            servo.spin_event_loop();
        }
    }

    /// Drops the webviews, then Servo (whose drop waits for its shutdown).
    fn shutdown(&self) {
        let webviews: Vec<_> = self
            .main_thread
            .windows
            .0
            .borrow_mut()
            .values_mut()
            .flat_map(|w| std::mem::take(&mut w.webviews))
            .collect();
        drop(webviews);
        let servo = self.main_thread.servo.borrow_mut().take();
        drop(servo);
    }

    pub(crate) fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, ServoRuntime<T>>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, ServoRuntime<T>>> {
        let label = pending.label.clone();
        let context = self.clone();
        let window_id = self.next_window_id();
        let (webview_id, use_https_scheme) = pending
            .webview
            .as_ref()
            .map(|w| {
                (
                    Some(context.next_webview_id()),
                    w.webview_attributes.use_https_scheme,
                )
            })
            .unwrap_or((None, false));

        let (tx, rx) = channel();
        self.send_user_message(Message::CreateWindow(
            window_id,
            Box::new(move |event_loop| {
                create_window(
                    window_id,
                    webview_id.unwrap_or_default(),
                    event_loop,
                    &context,
                    pending,
                    after_window_creation,
                )
            }),
            tx,
        ))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)??;

        let dispatcher = ServoWindowDispatcher {
            window_id,
            context: self.clone(),
        };
        let webview = webview_id.map(|id| DetachedWindowWebview {
            webview: DetachedWebview {
                label: label.clone(),
                dispatcher: ServoWebviewDispatcher {
                    window_id,
                    webview_id: id,
                    context: self.clone(),
                },
            },
            use_https_scheme,
        });
        Ok(DetachedWindow {
            id: window_id,
            label,
            dispatcher,
            webview,
        })
    }

    pub(crate) fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, ServoRuntime<T>>,
    ) -> Result<DetachedWebview<T, ServoRuntime<T>>> {
        let label = pending.label.clone();
        let context = self.clone();
        let webview_id = self.next_webview_id();

        let (tx, rx) = channel();
        self.send_user_message(Message::CreateWebview(
            window_id,
            Box::new(move |window| {
                create_webview(
                    WebviewKind::WindowChild,
                    window,
                    webview_id,
                    &context,
                    pending,
                )
            }),
            tx,
        ))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)??;

        let dispatcher = ServoWebviewDispatcher {
            window_id,
            webview_id,
            context: self.clone(),
        };
        Ok(DetachedWebview { label, dispatcher })
    }
}

/// Wakes the event loop for Servo, from any thread.
struct Waker<T: UserEvent>(TaoEventLoopProxy<Message<T>>);

impl<T: UserEvent> EventLoopWaker for Waker<T> {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(Waker(self.0.clone()))
    }

    fn wake(&self) {
        // Fails only once the loop is gone.
        let _ = self.0.send_event(Message::ServoWake);
    }
}

// --- Messages --------------------------------------------------------------

pub enum EventLoopWindowTargetMessage {
    CursorPosition(Sender<Result<PhysicalPosition<f64>>>),
    PrimaryMonitor(Sender<Option<Monitor>>),
    MonitorFromPoint(Sender<Option<Monitor>>, (f64, f64)),
    AvailableMonitors(Sender<Vec<Monitor>>),
    SetTheme(Option<Theme>),
    SetDeviceEventFilter(DeviceEventFilter),
}

pub type CreateWindowClosure<T> =
    Box<dyn FnOnce(&EventLoopWindowTarget<Message<T>>) -> Result<WindowWrapper> + Send>;

pub type CreateWebviewClosure = Box<dyn FnOnce(&Rc<Window>) -> Result<WebviewWrapper> + Send>;

pub enum Message<T: 'static> {
    Task(Box<dyn FnOnce() + Send>),
    RequestExit(i32),
    Window(WindowId, WindowMessage),
    Webview(WindowId, WebviewId, WebviewMessage),
    EventLoopWindowTarget(EventLoopWindowTargetMessage),
    CreateWebview(WindowId, CreateWebviewClosure, Sender<Result<()>>),
    CreateWindow(WindowId, CreateWindowClosure<T>, Sender<Result<()>>),
    /// Servo has work: the loop spins it (it does after every event).
    ServoWake,
    UserEvent(T),
}

// tao's event loop target is `Clone` only when its user event is (a derive
// bound), and the main thread keeps a clone of it. Nothing clones messages:
// a clone of anything but a user event is a no-op wake-up, not a panic.
impl<T: UserEvent> Clone for Message<T> {
    fn clone(&self) -> Self {
        match self {
            Self::UserEvent(t) => Self::UserEvent(t.clone()),
            _ => {
                log::error!("a runtime message was cloned; the clone is a no-op");
                Self::ServoWake
            }
        }
    }
}

fn device_event_filter(filter: DeviceEventFilter) -> TaoDeviceEventFilter {
    match filter {
        DeviceEventFilter::Always => TaoDeviceEventFilter::Always,
        DeviceEventFilter::Never => TaoDeviceEventFilter::Never,
        DeviceEventFilter::Unfocused => TaoDeviceEventFilter::Unfocused,
    }
}

/// Runs a message on the main thread.
fn handle_user_message<T: UserEvent>(
    event_loop: &EventLoopWindowTarget<Message<T>>,
    message: Message<T>,
    context: &Context<T>,
) {
    let windows = &context.main_thread.windows;
    match message {
        Message::Task(task) => task(),
        // These need the app's callback, which only the loop has: the
        // dispatchers post them, so they get here only if posted by hand.
        message @ (Message::RequestExit(_)
        | Message::Window(_, WindowMessage::Close)
        | Message::Window(_, WindowMessage::Destroy)
        | Message::UserEvent(_)) => {
            let _ = context.proxy.send_event(message);
        }
        Message::Window(id, window_message) => {
            let window = windows
                .0
                .borrow()
                .get(&id)
                .map(|w| (w.inner.clone(), w.window_event_listeners.clone()));
            if let Some((Some(window), listeners)) = window {
                handle_window_message(&window, window_message, &listeners);
            }
        }
        Message::Webview(window_id, webview_id, webview_message) => {
            handle_webview_message(context, window_id, webview_id, webview_message)
        }
        Message::CreateWebview(window_id, handler, sender) => {
            let window = windows
                .0
                .borrow()
                .get(&window_id)
                .and_then(|w| w.inner.clone());
            let result = match window {
                Some(window) => handler(&window).map(|webview| {
                    if let Some(w) = windows.0.borrow_mut().get_mut(&window_id) {
                        w.webviews.push(webview);
                    }
                }),
                None => Err(Error::WindowNotFound),
            };
            // The caller waits on the receiver; if it gave up, nobody cares.
            let _ = sender.send(result);
        }
        Message::CreateWindow(window_id, handler, sender) => {
            let result = handler(event_loop).map(|window| {
                windows.0.borrow_mut().insert(window_id, window);
            });
            let _ = sender.send(result);
        }
        Message::ServoWake => {}
        Message::EventLoopWindowTarget(message) => match message {
            EventLoopWindowTargetMessage::CursorPosition(sender) => {
                let _ = sender.send(
                    event_loop
                        .cursor_position()
                        .map_err(|_| Error::FailedToGetCursorPosition),
                );
            }
            EventLoopWindowTargetMessage::PrimaryMonitor(sender) => {
                let _ = sender.send(event_loop.primary_monitor().map(to_monitor));
            }
            EventLoopWindowTargetMessage::MonitorFromPoint(sender, (x, y)) => {
                let _ = sender.send(event_loop.monitor_from_point(x, y).map(to_monitor));
            }
            EventLoopWindowTargetMessage::AvailableMonitors(sender) => {
                let _ = sender.send(event_loop.available_monitors().map(to_monitor).collect());
            }
            EventLoopWindowTargetMessage::SetTheme(theme) => {
                event_loop.set_theme(to_tao_theme(theme))
            }
            EventLoopWindowTargetMessage::SetDeviceEventFilter(filter) => {
                event_loop.set_device_event_filter(device_event_filter(filter))
            }
        },
    }
}

// --- The event loop --------------------------------------------------------

fn make_event_handler<T: UserEvent, F: FnMut(RunEvent<T>) + 'static>(
    context: Context<T>,
    mut callback: F,
) -> impl FnMut(Event<'_, Message<T>>, &EventLoopWindowTarget<Message<T>>, &mut ControlFlow) {
    move |event, event_loop, control_flow| {
        handle_event_loop(event, event_loop, control_flow, &mut callback, &context);
        context.spin_servo();
    }
}

fn handle_event_loop<T: UserEvent>(
    event: Event<'_, Message<T>>,
    event_loop: &EventLoopWindowTarget<Message<T>>,
    control_flow: &mut ControlFlow,
    callback: &mut dyn FnMut(RunEvent<T>),
    context: &Context<T>,
) {
    if *control_flow != ControlFlow::Exit {
        *control_flow = ControlFlow::Wait;
    }
    let windows = &context.main_thread.windows;
    match event {
        Event::NewEvents(StartCause::Init) => callback(RunEvent::Ready),
        Event::NewEvents(StartCause::Poll) => callback(RunEvent::Resumed),
        Event::MainEventsCleared => callback(RunEvent::MainEventsCleared),
        Event::LoopDestroyed => {
            log::debug!("event loop destroyed: exiting");
            callback(RunEvent::Exit);
            context.shutdown();
            log::debug!("Servo shut down");
        }
        Event::RedrawRequested(tao_id) => {
            let webviews = context
                .window_id_map
                .get(&tao_id)
                .and_then(|id| windows.0.borrow().get(&id).map(|w| w.webviews.clone()))
                .unwrap_or_default();
            for webview in webviews {
                webview.inner.paint();
            }
        }
        Event::WindowEvent {
            event,
            window_id: tao_id,
            ..
        } => {
            if let Some(window_id) = context.window_id_map.get(&tao_id) {
                handle_window_event(window_id, event, control_flow, callback, context);
            }
        }
        Event::UserEvent(message) => match message {
            Message::RequestExit(code) => {
                let (tx, rx) = channel();
                callback(RunEvent::ExitRequested {
                    code: Some(code),
                    tx,
                });
                if !matches!(rx.try_recv(), Ok(ExitRequestedEventAction::Prevent)) {
                    *control_flow = ControlFlow::ExitWithCode(code);
                }
            }
            Message::Window(id, WindowMessage::Close) => on_close_requested(callback, id, context),
            Message::Window(id, WindowMessage::Destroy) => on_window_close(id, context),
            Message::UserEvent(t) => callback(RunEvent::UserEvent(t)),
            message => handle_user_message(event_loop, message, context),
        },
        _ => {}
    }
}

/// tao's window events Tauri hears about.
fn map_window_event(event: &TaoWindowEvent<'_>) -> Option<WindowEvent> {
    Some(match event {
        TaoWindowEvent::Resized(size) => WindowEvent::Resized(*size),
        TaoWindowEvent::Moved(position) => WindowEvent::Moved(*position),
        TaoWindowEvent::Destroyed => WindowEvent::Destroyed,
        TaoWindowEvent::ScaleFactorChanged {
            scale_factor,
            new_inner_size,
        } => WindowEvent::ScaleFactorChanged {
            scale_factor: *scale_factor,
            new_inner_size: **new_inner_size,
        },
        TaoWindowEvent::Focused(focused) => WindowEvent::Focused(*focused),
        TaoWindowEvent::ThemeChanged(theme) => WindowEvent::ThemeChanged(map_theme(theme)),
        _ => return None,
    })
}

fn handle_window_event<T: UserEvent>(
    window_id: WindowId,
    event: TaoWindowEvent<'_>,
    control_flow: &mut ControlFlow,
    callback: &mut dyn FnMut(RunEvent<T>),
    context: &Context<T>,
) {
    let windows = &context.main_thread.windows;

    // Tauri first, as tauri-runtime-wry does.
    if let Some(tauri_event) = map_window_event(&event) {
        let window = windows
            .0
            .borrow()
            .get(&window_id)
            .map(|w| (w.label.clone(), w.window_event_listeners.clone()));
        if let Some((label, listeners)) = window {
            callback(RunEvent::WindowEvent {
                label,
                event: tauri_event.clone(),
            });
            for handler in listeners.lock().unwrap().values() {
                handler(&tauri_event);
            }
        }
    }

    // Then the webviews: their surfaces follow the window, and the input
    // the GTK window received goes to Servo.
    let state = windows
        .0
        .borrow()
        .get(&window_id)
        .map(|w| (w.inner.clone(), w.webviews.clone(), w.input.clone()));
    if let Some((Some(window), webviews, input_state)) = state {
        match &event {
            TaoWindowEvent::Resized(size) => {
                for webview in &webviews {
                    if webview.inner.kind == WebviewKind::WindowContent
                        || webview.inner.auto_resize.get()
                    {
                        webview.inner.set_bounds(Bounds {
                            x: 0,
                            y: 0,
                            width: size.width,
                            height: size.height,
                        });
                    }
                }
                window.request_redraw();
            }
            TaoWindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                for webview in &webviews {
                    webview
                        .inner
                        .webview
                        .set_hidpi_scale_factor(Scale::new(*scale_factor as f32));
                }
            }
            TaoWindowEvent::Focused(focused) => {
                if let Some(webview) = webviews.first() {
                    webview.inner.webview.set_focused(*focused);
                }
            }
            TaoWindowEvent::ThemeChanged(theme) => {
                if let Some(theme) = to_servo_theme(*theme) {
                    for webview in &webviews {
                        webview.inner.webview.notify_theme_change(theme);
                    }
                }
            }
            _ => {}
        }
        let previous = input_state.update(&event);
        let is_key = matches!(event, TaoWindowEvent::KeyboardInput { .. });
        // Pointer events go to every webview (each takes what falls in its
        // bounds); keys to the first, the window's content.
        for webview in webviews.iter().take(if is_key { 1 } else { usize::MAX }) {
            let bounds = webview.inner.bounds.get();
            input::forward(
                &webview.inner.webview,
                (bounds.x, bounds.y),
                (bounds.width, bounds.height),
                &input_state,
                previous,
                &event,
            );
        }
    }

    match event {
        TaoWindowEvent::CloseRequested => on_close_requested(callback, window_id, context),
        TaoWindowEvent::Destroyed => {
            let removed = windows.0.borrow_mut().remove(&window_id);
            if removed.is_some() {
                log::debug!("window destroyed");
                drop(removed);
                if windows.0.borrow().is_empty() {
                    let (tx, rx) = channel();
                    callback(RunEvent::ExitRequested { code: None, tx });
                    if !matches!(rx.try_recv(), Ok(ExitRequestedEventAction::Prevent)) {
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
        }
        _ => {}
    }
}

fn on_close_requested<T: UserEvent>(
    callback: &mut dyn FnMut(RunEvent<T>),
    window_id: WindowId,
    context: &Context<T>,
) {
    let window = context
        .main_thread
        .windows
        .0
        .borrow()
        .get(&window_id)
        .map(|w| (w.label.clone(), w.window_event_listeners.clone()));
    let Some((label, listeners)) = window else {
        return;
    };
    log::debug!("close requested for window {label}");
    let (tx, rx) = channel();
    for handler in listeners.lock().unwrap().values() {
        handler(&WindowEvent::CloseRequested {
            signal_tx: tx.clone(),
        });
    }
    callback(RunEvent::WindowEvent {
        label,
        event: WindowEvent::CloseRequested { signal_tx: tx },
    });
    // `true` on the channel means the app prevented the close.
    if !matches!(rx.try_recv(), Ok(true)) {
        on_window_close(window_id, context);
    }
}

/// Drops the window's webviews, then the window; tao then reports it
/// destroyed, which removes it from the store.
fn on_window_close<T: UserEvent>(window_id: WindowId, context: &Context<T>) {
    let parts = context
        .main_thread
        .windows
        .0
        .borrow_mut()
        .get_mut(&window_id)
        .map(|w| (std::mem::take(&mut w.webviews), w.inner.take()));
    // Dropped outside the borrow: Servo and GTK may call back.
    drop(parts);
}

// --- The runtime and its handle --------------------------------------------

/// The proxy Tauri sends its user events through.
#[derive(Debug, Clone)]
pub struct EventProxy<T: UserEvent>(TaoEventLoopProxy<Message<T>>);

impl<T: UserEvent> EventLoopProxy<T> for EventProxy<T> {
    fn send_event(&self, event: T) -> Result<()> {
        self.0
            .send_event(Message::UserEvent(event))
            .map_err(|_| Error::EventLoopClosed)
    }
}

/// The app's data folder as Tauri's path resolver names it on Linux
/// (`$XDG_DATA_HOME/<identifier>`), for when no webview gives one.
fn app_data_dir(app_id: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(base.join(app_id))
}

/// A Tauri [`Runtime`] over libservo.
pub struct ServoRuntime<T: UserEvent> {
    context: Context<T>,
    event_loop: EventLoop<Message<T>>,
}

impl<T: UserEvent> fmt::Debug for ServoRuntime<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServoRuntime")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl<T: UserEvent> ServoRuntime<T> {
    fn init_with_builder(
        mut event_loop_builder: EventLoopBuilder<Message<T>>,
        args: RuntimeInitArgs,
    ) -> Result<Self> {
        // The process-wide rustls provider, before Servo or any plugin's
        // reqwest picks one: Servo's net and tauri-plugin-updater then both
        // use aws-lc-rs. An error means one is installed already.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let default_data_dir = args.app_id.as_deref().and_then(app_data_dir);
        if let Some(app_id) = args.app_id {
            use tao::platform::unix::EventLoopBuilderExtUnix;
            event_loop_builder.with_app_id(app_id);
        }
        let event_loop = event_loop_builder.build();

        #[allow(clippy::arc_with_non_send_sync)] // see `Context::main_thread`
        let main_thread = Arc::new(MainThreadContext {
            window_target: (*event_loop).clone(),
            windows: WindowsStore(RefCell::new(BTreeMap::new())),
            servo: RefCell::new(None),
            webdriver: Default::default(),
        });
        let context = Context {
            window_id_map: WindowIdStore::default(),
            main_thread_id: current_thread().id(),
            proxy: event_loop.create_proxy(),
            main_thread,
            schemes: SchemeHandlers::default(),
            default_data_dir,
            next_window_id: Default::default(),
            next_webview_id: Default::default(),
            next_window_event_id: Default::default(),
            next_webview_event_id: Default::default(),
        };
        Ok(Self {
            context,
            event_loop,
        })
    }
}

impl<T: UserEvent> Runtime<T> for ServoRuntime<T> {
    type WindowDispatcher = ServoWindowDispatcher<T>;
    type WebviewDispatcher = ServoWebviewDispatcher<T>;
    type Handle = ServoRuntimeHandle<T>;
    type EventLoopProxy = EventProxy<T>;

    fn new(args: RuntimeInitArgs) -> Result<Self> {
        Self::init_with_builder(EventLoopBuilder::<Message<T>>::with_user_event(), args)
    }

    fn new_any_thread(args: RuntimeInitArgs) -> Result<Self> {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        let mut event_loop_builder = EventLoopBuilder::<Message<T>>::with_user_event();
        event_loop_builder.with_any_thread(true);
        Self::init_with_builder(event_loop_builder, args)
    }

    fn create_proxy(&self) -> EventProxy<T> {
        EventProxy(self.event_loop.create_proxy())
    }

    fn handle(&self) -> Self::Handle {
        ServoRuntimeHandle {
            context: self.context.clone(),
        }
    }

    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, Self>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self>> {
        self.context.create_window(pending, after_window_creation)
    }

    fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, Self>,
    ) -> Result<DetachedWebview<T, Self>> {
        self.context.create_webview(window_id, pending)
    }

    fn primary_monitor(&self) -> Option<Monitor> {
        self.event_loop.primary_monitor().map(to_monitor)
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Option<Monitor> {
        self.event_loop.monitor_from_point(x, y).map(to_monitor)
    }

    fn available_monitors(&self) -> Vec<Monitor> {
        self.event_loop
            .available_monitors()
            .map(to_monitor)
            .collect()
    }

    fn cursor_position(&self) -> Result<PhysicalPosition<f64>> {
        self.event_loop
            .cursor_position()
            .map_err(|_| Error::FailedToGetCursorPosition)
    }

    fn set_theme(&self, theme: Option<Theme>) {
        self.event_loop.set_theme(to_tao_theme(theme));
    }

    fn set_device_event_filter(&mut self, filter: DeviceEventFilter) {
        self.event_loop
            .set_device_event_filter(device_event_filter(filter));
    }

    fn run_iteration<F: FnMut(RunEvent<T>) + 'static>(&mut self, mut callback: F) {
        use tao::platform::run_return::EventLoopExtRunReturn;
        let context = self.context.clone();
        self.event_loop
            .run_return(|event, event_loop, control_flow| {
                *control_flow = ControlFlow::Wait;
                if let Event::MainEventsCleared = &event {
                    *control_flow = ControlFlow::Exit;
                }
                handle_event_loop(event, event_loop, control_flow, &mut callback, &context);
                context.spin_servo();
            });
    }

    fn run_return<F: FnMut(RunEvent<T>) + 'static>(mut self, callback: F) -> i32 {
        use tao::platform::run_return::EventLoopExtRunReturn;
        let handler = make_event_handler(self.context.clone(), callback);
        self.event_loop.run_return(handler)
    }

    fn run<F: FnMut(RunEvent<T>) + 'static>(self, callback: F) {
        let handler = make_event_handler(self.context, callback);
        self.event_loop.run(handler)
    }
}

/// A handle to the runtime, usable from any thread.
#[derive(Debug, Clone)]
pub struct ServoRuntimeHandle<T: UserEvent> {
    context: Context<T>,
}

macro_rules! event_loop_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        $self
            .context
            .send_user_message(Message::EventLoopWindowTarget($message(tx)))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)
    }};
}

impl<T: UserEvent> RuntimeHandle<T> for ServoRuntimeHandle<T> {
    type Runtime = ServoRuntime<T>;

    fn create_proxy(&self) -> EventProxy<T> {
        EventProxy(self.context.proxy.clone())
    }

    fn request_exit(&self, code: i32) -> Result<()> {
        // Through the loop: exiting runs the app's callback.
        self.context
            .proxy
            .send_event(Message::RequestExit(code))
            .map_err(|_| Error::FailedToSendMessage)
    }

    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, Self::Runtime>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self::Runtime>> {
        self.context.create_window(pending, after_window_creation)
    }

    fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, Self::Runtime>,
    ) -> Result<DetachedWebview<T, Self::Runtime>> {
        self.context.create_webview(window_id, pending)
    }

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        self.context.send_user_message(Message::Task(Box::new(f)))
    }

    fn display_handle(
        &self,
    ) -> std::result::Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        // As in tauri-runtime-wry: the display handle is plain data, read
        // from the main thread's event loop target.
        self.context.main_thread.window_target.display_handle()
    }

    fn primary_monitor(&self) -> Result<Option<Monitor>> {
        event_loop_getter!(self, EventLoopWindowTargetMessage::PrimaryMonitor)
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Result<Option<Monitor>> {
        let (tx, rx) = channel();
        self.context
            .send_user_message(Message::EventLoopWindowTarget(
                EventLoopWindowTargetMessage::MonitorFromPoint(tx, (x, y)),
            ))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)
    }

    fn available_monitors(&self) -> Result<Vec<Monitor>> {
        event_loop_getter!(self, EventLoopWindowTargetMessage::AvailableMonitors)
    }

    fn cursor_position(&self) -> Result<PhysicalPosition<f64>> {
        event_loop_getter!(self, EventLoopWindowTargetMessage::CursorPosition)?
    }

    fn set_theme(&self, theme: Option<Theme>) {
        let _ = self
            .context
            .send_user_message(Message::EventLoopWindowTarget(
                EventLoopWindowTargetMessage::SetTheme(theme),
            ));
    }

    fn set_device_event_filter(&self, filter: DeviceEventFilter) {
        let _ = self
            .context
            .send_user_message(Message::EventLoopWindowTarget(
                EventLoopWindowTargetMessage::SetDeviceEventFilter(filter),
            ));
    }
}
