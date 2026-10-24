//! Windows: the Tauri window builder over tao's, the window messages, the
//! [`WindowDispatch`] implementation and window creation. Adapted from
//! tauri-runtime-wry 2.12.1 (Apache-2.0 OR MIT), Linux paths only.

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use tao::platform::unix::{WindowBuilderExtUnix, WindowExtUnix};
use tao::window::{
    CursorIcon as TaoCursorIcon, Fullscreen, Icon as TaoWindowIcon,
    ProgressBarState as TaoProgressBarState, ProgressState as TaoProgressState, Theme as TaoTheme,
    UserAttentionType as TaoUserAttentionType, Window, WindowBuilder as TaoWindowBuilder,
};
use tauri_runtime::dpi::{
    LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size,
};
use tauri_runtime::monitor::Monitor;
use tauri_runtime::webview::{DetachedWebview, PendingWebview};
use tauri_runtime::window::{
    CursorIcon, DetachedWindow, PendingWindow, RawWindow, WindowBuilder, WindowBuilderBase,
    WindowEvent, WindowId, WindowSizeConstraints,
};
use tauri_runtime::{
    Error, Icon, ProgressBarState, ProgressBarStatus, Result, UserAttentionType, UserEvent,
    WindowDispatch, WindowEventId,
};
use tauri_utils::config::{Color, PreventOverflowConfig, WindowConfig};
use tauri_utils::{Theme, TitleBarStyle};

use crate::input::InputState;
use crate::monitor::{center_position, find_monitor_for_position, to_monitor};
use crate::runtime::{Context, Message, ServoRuntime};
use crate::webview::{WebviewKind, WebviewWrapper, create_webview};

pub type WindowEventHandler = Box<dyn Fn(&WindowEvent) + Send>;
pub type WindowEventListeners = Arc<Mutex<HashMap<WindowEventId, WindowEventHandler>>>;

/// A window as the main thread holds it.
pub struct WindowWrapper {
    pub label: String,
    /// `None` once the window is closed and until tao reports it destroyed.
    pub inner: Option<Rc<Window>>,
    pub webviews: Vec<WebviewWrapper>,
    pub window_event_listeners: WindowEventListeners,
    pub input: Rc<InputState>,
}

impl fmt::Debug for WindowWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowWrapper")
            .field("label", &self.label)
            .field("open", &self.inner.is_some())
            .field("webviews", &self.webviews.len())
            .finish()
    }
}

// --- Conversions -----------------------------------------------------------

/// A [`TaoWindowIcon`] made from Tauri's [`Icon`].
pub struct TaoIcon(pub TaoWindowIcon);

impl TryFrom<Icon<'_>> for TaoIcon {
    type Error = Error;
    fn try_from(icon: Icon<'_>) -> std::result::Result<Self, Self::Error> {
        TaoWindowIcon::from_rgba(icon.rgba.to_vec(), icon.width, icon.height)
            .map(Self)
            .map_err(|e| Error::InvalidIcon(Box::new(e)))
    }
}

pub fn map_theme(theme: &TaoTheme) -> Theme {
    match theme {
        TaoTheme::Dark => Theme::Dark,
        _ => Theme::Light,
    }
}

pub fn to_tao_theme(theme: Option<Theme>) -> Option<TaoTheme> {
    match theme {
        Some(Theme::Light) => Some(TaoTheme::Light),
        Some(Theme::Dark) => Some(TaoTheme::Dark),
        _ => None,
    }
}

fn user_attention(request_type: UserAttentionType) -> TaoUserAttentionType {
    match request_type {
        UserAttentionType::Critical => TaoUserAttentionType::Critical,
        UserAttentionType::Informational => TaoUserAttentionType::Informational,
    }
}

fn cursor_icon(icon: CursorIcon) -> TaoCursorIcon {
    use CursorIcon::*;
    match icon {
        Default => TaoCursorIcon::Default,
        Crosshair => TaoCursorIcon::Crosshair,
        Hand => TaoCursorIcon::Hand,
        Arrow => TaoCursorIcon::Arrow,
        Move => TaoCursorIcon::Move,
        Text => TaoCursorIcon::Text,
        Wait => TaoCursorIcon::Wait,
        Help => TaoCursorIcon::Help,
        Progress => TaoCursorIcon::Progress,
        NotAllowed => TaoCursorIcon::NotAllowed,
        ContextMenu => TaoCursorIcon::ContextMenu,
        Cell => TaoCursorIcon::Cell,
        VerticalText => TaoCursorIcon::VerticalText,
        Alias => TaoCursorIcon::Alias,
        Copy => TaoCursorIcon::Copy,
        NoDrop => TaoCursorIcon::NoDrop,
        Grab => TaoCursorIcon::Grab,
        Grabbing => TaoCursorIcon::Grabbing,
        AllScroll => TaoCursorIcon::AllScroll,
        ZoomIn => TaoCursorIcon::ZoomIn,
        ZoomOut => TaoCursorIcon::ZoomOut,
        EResize => TaoCursorIcon::EResize,
        NResize => TaoCursorIcon::NResize,
        NeResize => TaoCursorIcon::NeResize,
        NwResize => TaoCursorIcon::NwResize,
        SResize => TaoCursorIcon::SResize,
        SeResize => TaoCursorIcon::SeResize,
        SwResize => TaoCursorIcon::SwResize,
        WResize => TaoCursorIcon::WResize,
        EwResize => TaoCursorIcon::EwResize,
        NsResize => TaoCursorIcon::NsResize,
        NeswResize => TaoCursorIcon::NeswResize,
        NwseResize => TaoCursorIcon::NwseResize,
        ColResize => TaoCursorIcon::ColResize,
        RowResize => TaoCursorIcon::RowResize,
        _ => TaoCursorIcon::Default,
    }
}

fn progress_bar(progress_state: ProgressBarState) -> TaoProgressBarState {
    TaoProgressBarState {
        progress: progress_state.progress,
        state: progress_state.status.map(|status| match status {
            ProgressBarStatus::None => TaoProgressState::None,
            ProgressBarStatus::Normal => TaoProgressState::Normal,
            ProgressBarStatus::Indeterminate => TaoProgressState::Indeterminate,
            ProgressBarStatus::Paused => TaoProgressState::Paused,
            ProgressBarStatus::Error => TaoProgressState::Error,
        }),
        desktop_filename: progress_state.desktop_filename,
    }
}

fn resize_direction(direction: tauri_runtime::ResizeDirection) -> tao::window::ResizeDirection {
    use tao::window::ResizeDirection as Tao;
    use tauri_runtime::ResizeDirection::*;
    match direction {
        East => Tao::East,
        North => Tao::North,
        NorthEast => Tao::NorthEast,
        NorthWest => Tao::NorthWest,
        South => Tao::South,
        SouthEast => Tao::SouthEast,
        SouthWest => Tao::SouthWest,
        West => Tao::West,
    }
}

// --- The window builder ----------------------------------------------------

#[derive(Clone, Default)]
pub struct WindowBuilderWrapper {
    pub(crate) inner: TaoWindowBuilder,
    center: bool,
    prevent_overflow: Option<Size>,
}

impl fmt::Debug for WindowBuilderWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowBuilderWrapper")
            .field("inner", &self.inner)
            .field("center", &self.center)
            .field("prevent_overflow", &self.prevent_overflow)
            .finish()
    }
}

// SAFETY: the builder holds GTK objects only through `transient_for`, which
// Tauri sets and reads on the main thread (as with tauri-runtime-wry).
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for WindowBuilderWrapper {}

impl WindowBuilderBase for WindowBuilderWrapper {}

impl WindowBuilder for WindowBuilderWrapper {
    fn new() -> Self {
        Self::default().focused(true).title("Tauri App")
    }

    fn with_config(config: &WindowConfig) -> Self {
        // tauri-runtime-wry turns tao's cursor-moved events off on Linux (its
        // webview tracks the pointer itself); Servo's input comes from those
        // events, so they stay on here.
        let mut window = WindowBuilderWrapper::new()
            .inner_size(config.width, config.height)
            .title(config.title.to_string())
            .focused(config.focus)
            .focusable(config.focusable)
            .visible(config.visible)
            .resizable(config.resizable)
            .fullscreen(config.fullscreen)
            .decorations(config.decorations)
            .maximized(config.maximized)
            .always_on_bottom(config.always_on_bottom)
            .always_on_top(config.always_on_top)
            .visible_on_all_workspaces(config.visible_on_all_workspaces)
            .content_protected(config.content_protected)
            .skip_taskbar(config.skip_taskbar)
            .theme(config.theme)
            .closable(config.closable)
            .maximizable(config.maximizable)
            .minimizable(config.minimizable)
            .shadow(config.shadow)
            .transparent(config.transparent);

        let mut constraints = WindowSizeConstraints::default();
        if let Some(min_width) = config.min_width {
            constraints.min_width = Some(tao::dpi::LogicalUnit::new(min_width).into());
        }
        if let Some(min_height) = config.min_height {
            constraints.min_height = Some(tao::dpi::LogicalUnit::new(min_height).into());
        }
        if let Some(max_width) = config.max_width {
            constraints.max_width = Some(tao::dpi::LogicalUnit::new(max_width).into());
        }
        if let Some(max_height) = config.max_height {
            constraints.max_height = Some(tao::dpi::LogicalUnit::new(max_height).into());
        }
        if let Some(color) = config.background_color {
            window = window.background_color(color);
        }
        window = window.inner_size_constraints(constraints);

        if let (Some(x), Some(y)) = (config.x, config.y) {
            window = window.position(x, y);
        }
        if config.center {
            window = window.center();
        }
        if let Some(prevent_overflow) = &config.prevent_overflow {
            window = match prevent_overflow {
                PreventOverflowConfig::Enable(true) => window.prevent_overflow(),
                PreventOverflowConfig::Margin(margin) => window.prevent_overflow_with_margin(
                    PhysicalSize::new(margin.width, margin.height).into(),
                ),
                _ => window,
            };
        }
        window
    }

    fn center(mut self) -> Self {
        self.center = true;
        self
    }

    fn position(mut self, x: f64, y: f64) -> Self {
        self.inner = self.inner.with_position(LogicalPosition::new(x, y));
        self
    }

    fn inner_size(mut self, width: f64, height: f64) -> Self {
        self.inner = self.inner.with_inner_size(LogicalSize::new(width, height));
        self
    }

    fn min_inner_size(mut self, min_width: f64, min_height: f64) -> Self {
        self.inner = self
            .inner
            .with_min_inner_size(LogicalSize::new(min_width, min_height));
        self
    }

    fn max_inner_size(mut self, max_width: f64, max_height: f64) -> Self {
        self.inner = self
            .inner
            .with_max_inner_size(LogicalSize::new(max_width, max_height));
        self
    }

    fn inner_size_constraints(mut self, constraints: WindowSizeConstraints) -> Self {
        self.inner.window.inner_size_constraints = tao::window::WindowSizeConstraints {
            min_width: constraints.min_width,
            min_height: constraints.min_height,
            max_width: constraints.max_width,
            max_height: constraints.max_height,
        };
        self
    }

    fn prevent_overflow(mut self) -> Self {
        self.prevent_overflow
            .replace(PhysicalSize::new(0, 0).into());
        self
    }

    fn prevent_overflow_with_margin(mut self, margin: Size) -> Self {
        self.prevent_overflow.replace(margin);
        self
    }

    fn resizable(mut self, resizable: bool) -> Self {
        self.inner = self.inner.with_resizable(resizable);
        self
    }

    fn maximizable(mut self, maximizable: bool) -> Self {
        self.inner = self.inner.with_maximizable(maximizable);
        self
    }

    fn minimizable(mut self, minimizable: bool) -> Self {
        self.inner = self.inner.with_minimizable(minimizable);
        self
    }

    fn closable(mut self, closable: bool) -> Self {
        self.inner = self.inner.with_closable(closable);
        self
    }

    fn title<S: Into<String>>(mut self, title: S) -> Self {
        self.inner = self.inner.with_title(title.into());
        self
    }

    fn fullscreen(mut self, fullscreen: bool) -> Self {
        self.inner = if fullscreen {
            self.inner
                .with_fullscreen(Some(Fullscreen::Borderless(None)))
        } else {
            self.inner.with_fullscreen(None)
        };
        self
    }

    fn focused(mut self, focused: bool) -> Self {
        self.inner = self.inner.with_focused(focused);
        self
    }

    fn focusable(mut self, focusable: bool) -> Self {
        self.inner = self.inner.with_focusable(focusable);
        self
    }

    fn maximized(mut self, maximized: bool) -> Self {
        self.inner = self.inner.with_maximized(maximized);
        self
    }

    fn visible(mut self, visible: bool) -> Self {
        self.inner = self.inner.with_visible(visible);
        self
    }

    fn transparent(mut self, transparent: bool) -> Self {
        self.inner = self.inner.with_transparent(transparent);
        self
    }

    fn decorations(mut self, decorations: bool) -> Self {
        self.inner = self.inner.with_decorations(decorations);
        self
    }

    fn always_on_bottom(mut self, always_on_bottom: bool) -> Self {
        self.inner = self.inner.with_always_on_bottom(always_on_bottom);
        self
    }

    fn always_on_top(mut self, always_on_top: bool) -> Self {
        self.inner = self.inner.with_always_on_top(always_on_top);
        self
    }

    fn visible_on_all_workspaces(mut self, visible_on_all_workspaces: bool) -> Self {
        self.inner = self
            .inner
            .with_visible_on_all_workspaces(visible_on_all_workspaces);
        self
    }

    fn content_protected(mut self, protected: bool) -> Self {
        self.inner = self.inner.with_content_protection(protected);
        self
    }

    fn shadow(self, _enable: bool) -> Self {
        // Windows and macOS only.
        self
    }

    fn transient_for(mut self, parent: &impl gtk::glib::IsA<gtk::Window>) -> Self {
        self.inner = self.inner.with_transient_for(parent);
        self
    }

    fn icon(mut self, icon: Icon) -> Result<Self> {
        self.inner = self
            .inner
            .with_window_icon(Some(TaoIcon::try_from(icon)?.0));
        Ok(self)
    }

    fn background_color(mut self, color: Color) -> Self {
        self.inner = self.inner.with_background_color(color.into());
        self
    }

    fn skip_taskbar(mut self, skip: bool) -> Self {
        self.inner = self.inner.with_skip_taskbar(skip);
        self
    }

    fn theme(mut self, theme: Option<Theme>) -> Self {
        self.inner = self.inner.with_theme(theme.map(|theme| match theme {
            Theme::Dark => TaoTheme::Dark,
            _ => TaoTheme::Light,
        }));
        self
    }

    fn has_icon(&self) -> bool {
        self.inner.window.window_icon.is_some()
    }

    fn get_theme(&self) -> Option<Theme> {
        self.inner
            .window
            .preferred_theme
            .map(|theme| map_theme(&theme))
    }

    fn window_classname<S: Into<String>>(self, _window_classname: S) -> Self {
        // Windows only.
        self
    }

    fn no_redirection_bitmap(self, _enable: bool) -> Self {
        // Windows only.
        self
    }
}

// --- Messages --------------------------------------------------------------

/// The GTK window, sent from the main thread to the caller of
/// [`WindowDispatch::gtk_window`].
pub struct GtkWindow(pub gtk::ApplicationWindow);
// SAFETY: as in tauri-runtime-wry: Tauri and plugins use it on the main thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for GtkWindow {}

pub struct GtkBox(pub gtk::Box);
// SAFETY: as for `GtkWindow`.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for GtkBox {}

pub struct SendRawWindowHandle(pub raw_window_handle::RawWindowHandle);
// SAFETY: a handle is plain data; using it is the receiver's business.
unsafe impl Send for SendRawWindowHandle {}

pub enum WindowMessage {
    AddEventListener(WindowEventId, WindowEventHandler),
    // Getters
    ScaleFactor(Sender<f64>),
    InnerPosition(Sender<Result<PhysicalPosition<i32>>>),
    OuterPosition(Sender<Result<PhysicalPosition<i32>>>),
    InnerSize(Sender<PhysicalSize<u32>>),
    OuterSize(Sender<PhysicalSize<u32>>),
    IsFullscreen(Sender<bool>),
    IsMinimized(Sender<bool>),
    IsMaximized(Sender<bool>),
    IsFocused(Sender<bool>),
    IsDecorated(Sender<bool>),
    IsResizable(Sender<bool>),
    IsMaximizable(Sender<bool>),
    IsMinimizable(Sender<bool>),
    IsClosable(Sender<bool>),
    IsVisible(Sender<bool>),
    Title(Sender<String>),
    CurrentMonitor(Sender<Option<Monitor>>),
    PrimaryMonitor(Sender<Option<Monitor>>),
    MonitorFromPoint(Sender<Option<Monitor>>, (f64, f64)),
    AvailableMonitors(Sender<Vec<Monitor>>),
    GtkWindow(Sender<GtkWindow>),
    GtkBox(Sender<Option<GtkBox>>),
    RawWindowHandle(
        Sender<std::result::Result<SendRawWindowHandle, raw_window_handle::HandleError>>,
    ),
    Theme(Sender<Theme>),
    IsEnabled(Sender<bool>),
    IsAlwaysOnTop(Sender<bool>),
    // Setters
    Center,
    RequestUserAttention(Option<UserAttentionType>),
    SetEnabled(bool),
    SetResizable(bool),
    SetMaximizable(bool),
    SetMinimizable(bool),
    SetClosable(bool),
    SetTitle(String),
    Maximize,
    Unmaximize,
    Minimize,
    Unminimize,
    Show,
    Hide,
    /// Handled by the event loop (it runs the close-requested callbacks).
    Close,
    /// Handled by the event loop.
    Destroy,
    SetDecorations(bool),
    SetAlwaysOnBottom(bool),
    SetAlwaysOnTop(bool),
    SetVisibleOnAllWorkspaces(bool),
    SetContentProtected(bool),
    SetSize(Size),
    SetMinSize(Option<Size>),
    SetMaxSize(Option<Size>),
    SetSizeConstraints(WindowSizeConstraints),
    SetPosition(Position),
    SetFullscreen(bool),
    SetFullscreenOnMonitor(PhysicalPosition<f64>),
    SetFocus,
    SetFocusable(bool),
    SetIcon(TaoWindowIcon),
    SetSkipTaskbar(bool),
    SetCursorGrab(bool),
    SetCursorVisible(bool),
    SetCursorIcon(CursorIcon),
    SetCursorPosition(Position),
    SetIgnoreCursorEvents(bool),
    SetBadgeCount(Option<i64>, Option<String>),
    SetProgressBar(ProgressBarState),
    SetTheme(Option<Theme>),
    SetBackgroundColor(Option<Color>),
    DragWindow,
    ResizeDragWindow(tauri_runtime::ResizeDirection),
    RequestRedraw,
}

/// Runs a window message on the main thread. `Close` and `Destroy` never
/// get here (the event loop takes them).
pub fn handle_window_message(
    window: &Window,
    message: WindowMessage,
    listeners: &WindowEventListeners,
) {
    // A dropped receiver means the caller gave up waiting: nothing to do.
    match message {
        WindowMessage::AddEventListener(id, listener) => {
            listeners.lock().unwrap().insert(id, listener);
        }
        // Getters
        WindowMessage::ScaleFactor(tx) => reply(&tx, window.scale_factor()),
        WindowMessage::InnerPosition(tx) => reply(
            &tx,
            window
                .inner_position()
                .map_err(|_| Error::FailedToSendMessage),
        ),
        WindowMessage::OuterPosition(tx) => reply(
            &tx,
            window
                .outer_position()
                .map_err(|_| Error::FailedToSendMessage),
        ),
        WindowMessage::InnerSize(tx) => reply(&tx, window.inner_size()),
        WindowMessage::OuterSize(tx) => reply(&tx, window.outer_size()),
        WindowMessage::IsFullscreen(tx) => reply(&tx, window.fullscreen().is_some()),
        WindowMessage::IsMinimized(tx) => reply(&tx, window.is_minimized()),
        WindowMessage::IsMaximized(tx) => reply(&tx, window.is_maximized()),
        WindowMessage::IsFocused(tx) => reply(&tx, window.is_focused()),
        WindowMessage::IsDecorated(tx) => reply(&tx, window.is_decorated()),
        WindowMessage::IsResizable(tx) => reply(&tx, window.is_resizable()),
        WindowMessage::IsMaximizable(tx) => reply(&tx, window.is_maximizable()),
        WindowMessage::IsMinimizable(tx) => reply(&tx, window.is_minimizable()),
        WindowMessage::IsClosable(tx) => reply(&tx, window.is_closable()),
        WindowMessage::IsVisible(tx) => reply(&tx, window.is_visible()),
        WindowMessage::Title(tx) => reply(&tx, window.title()),
        WindowMessage::CurrentMonitor(tx) => reply(&tx, window.current_monitor().map(to_monitor)),
        WindowMessage::PrimaryMonitor(tx) => reply(&tx, window.primary_monitor().map(to_monitor)),
        WindowMessage::MonitorFromPoint(tx, (x, y)) => {
            reply(&tx, window.monitor_from_point(x, y).map(to_monitor))
        }
        WindowMessage::AvailableMonitors(tx) => {
            reply(&tx, window.available_monitors().map(to_monitor).collect())
        }
        WindowMessage::GtkWindow(tx) => reply(&tx, GtkWindow(window.gtk_window().clone())),
        WindowMessage::GtkBox(tx) => reply(&tx, window.default_vbox().map(|b| GtkBox(b.clone()))),
        WindowMessage::RawWindowHandle(tx) => {
            use raw_window_handle::HasWindowHandle;
            reply(
                &tx,
                window
                    .window_handle()
                    .map(|h| SendRawWindowHandle(h.as_raw())),
            )
        }
        WindowMessage::Theme(tx) => reply(&tx, map_theme(&window.theme())),
        WindowMessage::IsEnabled(tx) => reply(&tx, window.gtk_window().is_sensitive()),
        WindowMessage::IsAlwaysOnTop(tx) => reply(&tx, window.is_always_on_top()),
        // Setters
        WindowMessage::Center => {
            if let Some(monitor) = window.current_monitor() {
                window.set_outer_position(center_position(window.outer_size(), &monitor));
            }
        }
        WindowMessage::RequestUserAttention(request_type) => {
            window.request_user_attention(request_type.map(user_attention))
        }
        WindowMessage::SetEnabled(enabled) => window.gtk_window().set_sensitive(enabled),
        WindowMessage::SetResizable(resizable) => window.set_resizable(resizable),
        WindowMessage::SetMaximizable(maximizable) => window.set_maximizable(maximizable),
        WindowMessage::SetMinimizable(minimizable) => window.set_minimizable(minimizable),
        WindowMessage::SetClosable(closable) => window.set_closable(closable),
        WindowMessage::SetTitle(title) => window.set_title(&title),
        WindowMessage::Maximize => window.set_maximized(true),
        WindowMessage::Unmaximize => window.set_maximized(false),
        WindowMessage::Minimize => window.set_minimized(true),
        WindowMessage::Unminimize => window.set_minimized(false),
        WindowMessage::Show => window.set_visible(true),
        WindowMessage::Hide => window.set_visible(false),
        WindowMessage::Close | WindowMessage::Destroy => {
            log::error!("a window close reached the message handler instead of the event loop")
        }
        WindowMessage::SetDecorations(decorations) => window.set_decorations(decorations),
        WindowMessage::SetAlwaysOnBottom(always_on_bottom) => {
            window.set_always_on_bottom(always_on_bottom)
        }
        WindowMessage::SetAlwaysOnTop(always_on_top) => window.set_always_on_top(always_on_top),
        WindowMessage::SetVisibleOnAllWorkspaces(visible) => {
            window.set_visible_on_all_workspaces(visible)
        }
        WindowMessage::SetContentProtected(protected) => window.set_content_protection(protected),
        WindowMessage::SetSize(size) => window.set_inner_size(size),
        WindowMessage::SetMinSize(size) => window.set_min_inner_size(size),
        WindowMessage::SetMaxSize(size) => window.set_max_inner_size(size),
        WindowMessage::SetSizeConstraints(constraints) => {
            window.set_inner_size_constraints(tao::window::WindowSizeConstraints {
                min_width: constraints.min_width,
                min_height: constraints.min_height,
                max_width: constraints.max_width,
                max_height: constraints.max_height,
            })
        }
        WindowMessage::SetPosition(position) => window.set_outer_position(position),
        WindowMessage::SetFullscreen(fullscreen) => {
            window.set_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)))
        }
        WindowMessage::SetFullscreenOnMonitor(position) => {
            // Not `Window::monitor_from_point`: on Linux (GTK) it takes logical
            // coordinates, while callers pass physical ones.
            if let Some(monitor) =
                find_monitor_for_position(window.available_monitors(), position.into())
            {
                window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor))))
            }
        }
        WindowMessage::SetFocus => window.set_focus(),
        WindowMessage::SetFocusable(focusable) => window.set_focusable(focusable),
        WindowMessage::SetIcon(icon) => window.set_window_icon(Some(icon)),
        WindowMessage::SetSkipTaskbar(skip) => {
            let _ = window.set_skip_taskbar(skip);
        }
        WindowMessage::SetCursorGrab(grab) => {
            let _ = window.set_cursor_grab(grab);
        }
        WindowMessage::SetCursorVisible(visible) => window.set_cursor_visible(visible),
        WindowMessage::SetCursorIcon(icon) => window.set_cursor_icon(cursor_icon(icon)),
        WindowMessage::SetCursorPosition(position) => {
            let _ = window.set_cursor_position(position);
        }
        WindowMessage::SetIgnoreCursorEvents(ignore) => {
            let _ = window.set_ignore_cursor_events(ignore);
        }
        WindowMessage::DragWindow => {
            let _ = window.drag_window();
        }
        WindowMessage::ResizeDragWindow(direction) => {
            let _ = window.drag_resize_window(resize_direction(direction));
        }
        WindowMessage::RequestRedraw => window.request_redraw(),
        WindowMessage::SetBadgeCount(count, desktop_filename) => {
            window.set_badge_count(count, desktop_filename)
        }
        WindowMessage::SetProgressBar(progress_state) => {
            window.set_progress_bar(progress_bar(progress_state))
        }
        WindowMessage::SetTheme(theme) => window.set_theme(to_tao_theme(theme)),
        WindowMessage::SetBackgroundColor(color) => {
            window.set_background_color(color.map(Into::into))
        }
    }
}

// --- The dispatcher --------------------------------------------------------

/// The Tauri [`WindowDispatch`] for [`ServoRuntime`].
#[derive(Debug, Clone)]
pub struct ServoWindowDispatcher<T: UserEvent> {
    pub(crate) window_id: WindowId,
    pub(crate) context: Context<T>,
}

macro_rules! window_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        $self
            .context
            .send_user_message(Message::Window($self.window_id, $message(tx)))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)
    }};
}

#[allow(clippy::type_complexity)]
fn raw_window_handle<T: UserEvent>(
    dispatcher: &ServoWindowDispatcher<T>,
) -> Result<std::result::Result<SendRawWindowHandle, raw_window_handle::HandleError>> {
    let (tx, rx) = channel();
    dispatcher.send(WindowMessage::RawWindowHandle(tx))?;
    rx.recv().map_err(|_| Error::FailedToReceiveMessage)
}

impl<T: UserEvent> ServoWindowDispatcher<T> {
    fn send(&self, message: WindowMessage) -> Result<()> {
        self.context
            .send_user_message(Message::Window(self.window_id, message))
    }
}

impl<T: UserEvent> WindowDispatch<T> for ServoWindowDispatcher<T> {
    type Runtime = ServoRuntime<T>;
    type WindowBuilder = WindowBuilderWrapper;

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        self.context.send_user_message(Message::Task(Box::new(f)))
    }

    fn on_window_event<F: Fn(&WindowEvent) + Send + 'static>(&self, f: F) -> WindowEventId {
        let id = self.context.next_window_event_id();
        let _ = self.context.proxy.send_event(Message::Window(
            self.window_id,
            WindowMessage::AddEventListener(id, Box::new(f)),
        ));
        id
    }

    fn scale_factor(&self) -> Result<f64> {
        window_getter!(self, WindowMessage::ScaleFactor)
    }

    fn inner_position(&self) -> Result<PhysicalPosition<i32>> {
        window_getter!(self, WindowMessage::InnerPosition)?
    }

    fn outer_position(&self) -> Result<PhysicalPosition<i32>> {
        window_getter!(self, WindowMessage::OuterPosition)?
    }

    fn inner_size(&self) -> Result<PhysicalSize<u32>> {
        window_getter!(self, WindowMessage::InnerSize)
    }

    fn outer_size(&self) -> Result<PhysicalSize<u32>> {
        window_getter!(self, WindowMessage::OuterSize)
    }

    fn is_fullscreen(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsFullscreen)
    }

    fn is_minimized(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMinimized)
    }

    fn is_maximized(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMaximized)
    }

    fn is_focused(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsFocused)
    }

    fn is_decorated(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsDecorated)
    }

    fn is_resizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsResizable)
    }

    fn is_maximizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMaximizable)
    }

    fn is_minimizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMinimizable)
    }

    fn is_closable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsClosable)
    }

    fn is_visible(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsVisible)
    }

    fn is_enabled(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsEnabled)
    }

    fn is_always_on_top(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsAlwaysOnTop)
    }

    fn title(&self) -> Result<String> {
        window_getter!(self, WindowMessage::Title)
    }

    fn current_monitor(&self) -> Result<Option<Monitor>> {
        window_getter!(self, WindowMessage::CurrentMonitor)
    }

    fn primary_monitor(&self) -> Result<Option<Monitor>> {
        window_getter!(self, WindowMessage::PrimaryMonitor)
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Result<Option<Monitor>> {
        let (tx, rx) = channel();
        self.send(WindowMessage::MonitorFromPoint(tx, (x, y)))?;
        rx.recv().map_err(|_| Error::FailedToReceiveMessage)
    }

    fn available_monitors(&self) -> Result<Vec<Monitor>> {
        window_getter!(self, WindowMessage::AvailableMonitors)
    }

    fn gtk_window(&self) -> Result<gtk::ApplicationWindow> {
        window_getter!(self, WindowMessage::GtkWindow).map(|w| w.0)
    }

    fn default_vbox(&self) -> Result<gtk::Box> {
        window_getter!(self, WindowMessage::GtkBox)?
            .map(|b| b.0)
            .ok_or(Error::WindowNotFound)
    }

    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        raw_window_handle(self)
            .map_err(|_| raw_window_handle::HandleError::Unavailable)
            // SAFETY: as in tauri-runtime-wry: the handle names the window for
            // as long as the window is open, which the caller holds.
            .and_then(|r| r.map(|h| unsafe { raw_window_handle::WindowHandle::borrow_raw(h.0) }))
    }

    fn theme(&self) -> Result<Theme> {
        window_getter!(self, WindowMessage::Theme)
    }

    fn center(&self) -> Result<()> {
        self.send(WindowMessage::Center)
    }

    fn request_user_attention(&self, request_type: Option<UserAttentionType>) -> Result<()> {
        self.send(WindowMessage::RequestUserAttention(request_type))
    }

    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &mut self,
        pending: PendingWindow<T, Self::Runtime>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self::Runtime>> {
        self.context.create_window(pending, after_window_creation)
    }

    fn create_webview(
        &mut self,
        pending: PendingWebview<T, Self::Runtime>,
    ) -> Result<DetachedWebview<T, Self::Runtime>> {
        self.context.create_webview(self.window_id, pending)
    }

    fn set_resizable(&self, resizable: bool) -> Result<()> {
        self.send(WindowMessage::SetResizable(resizable))
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.send(WindowMessage::SetEnabled(enabled))
    }

    fn set_maximizable(&self, maximizable: bool) -> Result<()> {
        self.send(WindowMessage::SetMaximizable(maximizable))
    }

    fn set_minimizable(&self, minimizable: bool) -> Result<()> {
        self.send(WindowMessage::SetMinimizable(minimizable))
    }

    fn set_closable(&self, closable: bool) -> Result<()> {
        self.send(WindowMessage::SetClosable(closable))
    }

    fn set_title<S: Into<String>>(&self, title: S) -> Result<()> {
        self.send(WindowMessage::SetTitle(title.into()))
    }

    fn maximize(&self) -> Result<()> {
        self.send(WindowMessage::Maximize)
    }

    fn unmaximize(&self) -> Result<()> {
        self.send(WindowMessage::Unmaximize)
    }

    fn minimize(&self) -> Result<()> {
        self.send(WindowMessage::Minimize)
    }

    fn unminimize(&self) -> Result<()> {
        self.send(WindowMessage::Unminimize)
    }

    fn show(&self) -> Result<()> {
        self.send(WindowMessage::Show)
    }

    fn hide(&self) -> Result<()> {
        self.send(WindowMessage::Hide)
    }

    fn close(&self) -> Result<()> {
        // Always through the loop: closing runs the app's callback, which the
        // loop owns.
        self.context
            .proxy
            .send_event(Message::Window(self.window_id, WindowMessage::Close))
            .map_err(|_| Error::FailedToSendMessage)
    }

    fn destroy(&self) -> Result<()> {
        self.context
            .proxy
            .send_event(Message::Window(self.window_id, WindowMessage::Destroy))
            .map_err(|_| Error::FailedToSendMessage)
    }

    fn set_decorations(&self, decorations: bool) -> Result<()> {
        self.send(WindowMessage::SetDecorations(decorations))
    }

    fn set_shadow(&self, _enable: bool) -> Result<()> {
        // Windows and macOS only, as with tauri-runtime-wry.
        Ok(())
    }

    fn set_always_on_bottom(&self, always_on_bottom: bool) -> Result<()> {
        self.send(WindowMessage::SetAlwaysOnBottom(always_on_bottom))
    }

    fn set_always_on_top(&self, always_on_top: bool) -> Result<()> {
        self.send(WindowMessage::SetAlwaysOnTop(always_on_top))
    }

    fn set_visible_on_all_workspaces(&self, visible_on_all_workspaces: bool) -> Result<()> {
        self.send(WindowMessage::SetVisibleOnAllWorkspaces(
            visible_on_all_workspaces,
        ))
    }

    fn set_background_color(&self, color: Option<Color>) -> Result<()> {
        self.send(WindowMessage::SetBackgroundColor(color))
    }

    fn set_content_protected(&self, protected: bool) -> Result<()> {
        self.send(WindowMessage::SetContentProtected(protected))
    }

    fn set_size(&self, size: Size) -> Result<()> {
        self.send(WindowMessage::SetSize(size))
    }

    fn set_min_size(&self, size: Option<Size>) -> Result<()> {
        self.send(WindowMessage::SetMinSize(size))
    }

    fn set_max_size(&self, size: Option<Size>) -> Result<()> {
        self.send(WindowMessage::SetMaxSize(size))
    }

    fn set_size_constraints(&self, constraints: WindowSizeConstraints) -> Result<()> {
        self.send(WindowMessage::SetSizeConstraints(constraints))
    }

    fn set_position(&self, position: Position) -> Result<()> {
        self.send(WindowMessage::SetPosition(position))
    }

    fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        self.send(WindowMessage::SetFullscreen(fullscreen))
    }

    fn set_fullscreen_on_monitor(&self, position: PhysicalPosition<f64>) -> Result<()> {
        self.send(WindowMessage::SetFullscreenOnMonitor(position))
    }

    fn set_focus(&self) -> Result<()> {
        self.send(WindowMessage::SetFocus)
    }

    fn set_focusable(&self, focusable: bool) -> Result<()> {
        self.send(WindowMessage::SetFocusable(focusable))
    }

    fn set_icon(&self, icon: Icon) -> Result<()> {
        self.send(WindowMessage::SetIcon(TaoIcon::try_from(icon)?.0))
    }

    fn set_skip_taskbar(&self, skip: bool) -> Result<()> {
        self.send(WindowMessage::SetSkipTaskbar(skip))
    }

    fn set_cursor_grab(&self, grab: bool) -> Result<()> {
        self.send(WindowMessage::SetCursorGrab(grab))
    }

    fn set_cursor_visible(&self, visible: bool) -> Result<()> {
        self.send(WindowMessage::SetCursorVisible(visible))
    }

    fn set_cursor_icon(&self, icon: CursorIcon) -> Result<()> {
        self.send(WindowMessage::SetCursorIcon(icon))
    }

    fn set_cursor_position<Pos: Into<Position>>(&self, position: Pos) -> Result<()> {
        self.send(WindowMessage::SetCursorPosition(position.into()))
    }

    fn set_ignore_cursor_events(&self, ignore: bool) -> Result<()> {
        self.send(WindowMessage::SetIgnoreCursorEvents(ignore))
    }

    fn start_dragging(&self) -> Result<()> {
        self.send(WindowMessage::DragWindow)
    }

    fn start_resize_dragging(&self, direction: tauri_runtime::ResizeDirection) -> Result<()> {
        self.send(WindowMessage::ResizeDragWindow(direction))
    }

    fn set_badge_count(&self, count: Option<i64>, desktop_filename: Option<String>) -> Result<()> {
        self.send(WindowMessage::SetBadgeCount(count, desktop_filename))
    }

    fn set_badge_label(&self, _label: Option<String>) -> Result<()> {
        // macOS only, as with tauri-runtime-wry.
        Ok(())
    }

    fn set_overlay_icon(&self, _icon: Option<Icon>) -> Result<()> {
        // Windows only, as with tauri-runtime-wry.
        Ok(())
    }

    fn set_progress_bar(&self, progress_state: ProgressBarState) -> Result<()> {
        self.send(WindowMessage::SetProgressBar(progress_state))
    }

    fn set_title_bar_style(&self, _style: TitleBarStyle) -> Result<()> {
        // macOS only, as with tauri-runtime-wry.
        Ok(())
    }

    fn set_traffic_light_position(&self, _position: Position) -> Result<()> {
        // macOS only, as with tauri-runtime-wry.
        Ok(())
    }

    fn set_theme(&self, theme: Option<Theme>) -> Result<()> {
        self.send(WindowMessage::SetTheme(theme))
    }
}

// --- Window creation (on the main thread) ----------------------------------

pub fn create_window<T: UserEvent, F: Fn(RawWindow) + Send + 'static>(
    window_id: WindowId,
    webview_id: u32,
    event_loop: &tao::event_loop::EventLoopWindowTarget<Message<T>>,
    context: &Context<T>,
    pending: PendingWindow<T, ServoRuntime<T>>,
    after_window_creation: Option<F>,
) -> Result<WindowWrapper> {
    let PendingWindow {
        mut window_builder,
        label,
        webview,
    } = pending;

    if window_builder.prevent_overflow.is_some() || window_builder.center {
        let monitor = if let Some(window_position) = &window_builder.inner.window.position {
            find_monitor_for_position(event_loop.available_monitors(), *window_position)
        } else {
            event_loop.primary_monitor()
        };
        if let Some(monitor) = monitor {
            let scale_factor = monitor.scale_factor();
            let desired_size = window_builder
                .inner
                .window
                .inner_size
                .unwrap_or_else(|| PhysicalSize::new(800, 600).into());
            let mut inner_size = window_builder
                .inner
                .window
                .inner_size_constraints
                .clamp(desired_size, scale_factor)
                .to_physical::<u32>(scale_factor);
            let mut window_size = inner_size;
            if let Some(margin) = window_builder.prevent_overflow {
                let work_area = crate::monitor::work_area(&monitor);
                let margin = margin.to_physical::<u32>(scale_factor);
                let constraint = PhysicalSize::new(
                    work_area.size.width - margin.width,
                    work_area.size.height - margin.height,
                );
                if window_size.width > constraint.width || window_size.height > constraint.height {
                    if window_size.width > constraint.width {
                        inner_size.width = inner_size
                            .width
                            .saturating_sub(window_size.width - constraint.width);
                        window_size.width = constraint.width;
                    }
                    if window_size.height > constraint.height {
                        inner_size.height = inner_size
                            .height
                            .saturating_sub(window_size.height - constraint.height);
                        window_size.height = constraint.height;
                    }
                    window_builder.inner.window.inner_size = Some(inner_size.into());
                }
            }
            if window_builder.center {
                let position =
                    center_position(window_size, &monitor).to_logical::<f64>(scale_factor);
                window_builder = window_builder.position(position.x, position.y);
            }
        }
    }

    // Fullscreen with an explicit position: fullscreen on that monitor.
    if window_builder.inner.window.fullscreen.is_some()
        && let Some(position) = window_builder.inner.window.position
        && let Some(target) = find_monitor_for_position(event_loop.available_monitors(), position)
    {
        window_builder.inner.window.fullscreen = Some(Fullscreen::Borderless(Some(target)));
    }

    let window = window_builder
        .inner
        .build(event_loop)
        .inspect_err(|e| log::error!("Error creating window: {e:?}"))
        .map_err(|_| Error::CreateWindow)?;

    context.window_id_map.insert(window.id(), window_id);

    if let Some(handler) = after_window_creation {
        handler(RawWindow {
            gtk_window: window.gtk_window(),
            default_vbox: window.default_vbox(),
            _marker: &std::marker::PhantomData,
        });
    }

    let window = Rc::new(window);
    let input = Rc::new(InputState::default());
    let mut webviews = Vec::new();
    if let Some(webview) = webview {
        webviews.push(create_webview(
            WebviewKind::WindowContent,
            &window,
            webview_id,
            context,
            webview,
        )?);
    }

    Ok(WindowWrapper {
        label,
        inner: Some(window),
        webviews,
        window_event_listeners: Default::default(),
        input,
    })
}

/// Answers a getter. A dropped receiver means the caller gave up waiting.
fn reply<V>(tx: &Sender<V>, value: V) {
    let _ = tx.send(value);
}
