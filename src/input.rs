//! tao window events to Servo input events (after servoshell's
//! `headed_window.rs` and `keyutils.rs`, ported from winit to tao).
//!
//! The webview's child window takes no input, so every pointer, wheel and
//! key event arrives on the GTK toplevel and comes here through tao; the
//! runtime is the webview's whole input stack.

use std::cell::Cell;
use std::str::FromStr;

use gtk::gdk::ScrollDirection;
use gtk::glib;
use gtk::prelude::*;
use servo::{
    Code, DevicePoint, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers,
    MouseButton as ServoMouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent,
    MouseMoveEvent, NamedKey, WebView, WheelDelta, WheelEvent, WheelMode,
};
use tao::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use tao::keyboard::{Key as TaoKey, KeyCode, KeyLocation, ModifiersState};

/// Pixels per wheel "line", servoshell's value.
const LINE_HEIGHT: f32 = 76.0;
const LINE_WIDTH: f32 = 76.0;

/// What the conversion remembers between events, per window.
#[derive(Debug, Default)]
pub struct InputState {
    /// The pointer, in the window's physical pixels.
    cursor: Cell<Option<(f64, f64)>>,
    modifiers: Cell<ModifiersState>,
}

impl InputState {
    /// Notes what a window event changes; call once per event, before
    /// forwarding it to the window's webviews.
    pub fn update(&self, event: &WindowEvent<'_>) -> Option<(f64, f64)> {
        let previous = self.cursor.get();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor.set(Some((position.x, position.y)))
            }
            WindowEvent::CursorLeft { .. } => self.cursor.set(None),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers.set(*modifiers),
            _ => {}
        }
        previous
    }
}

/// Forwards one tao window event to the webview whose top-left corner is at
/// `origin` (window physical pixels) and whose size is `size`; `previous` is
/// the pointer before this event ([`InputState::update`]'s result). Pointer
/// and wheel events outside the webview are not its.
pub fn forward(
    webview: &WebView,
    origin: (i32, i32),
    size: (u32, u32),
    state: &InputState,
    previous: Option<(f64, f64)>,
    event: &WindowEvent<'_>,
) {
    let to_webview = |(x, y): (f64, f64)| {
        DevicePoint::new((x - origin.0 as f64) as f32, (y - origin.1 as f64) as f32)
    };
    let inside = |point: DevicePoint| {
        point.x >= 0.0 && point.y >= 0.0 && point.x < size.0 as f32 && point.y < size.1 as f32
    };
    let cursor = state
        .cursor
        .get()
        .map(to_webview)
        .filter(|point| inside(*point));
    let was_inside = previous.map(to_webview).is_some_and(inside);
    match event {
        WindowEvent::CursorMoved { .. } => match cursor {
            Some(point) => {
                webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
            }
            None if was_inside => {
                webview.notify_input_event(InputEvent::MouseLeftViewport(
                    MouseLeftViewportEvent::default(),
                ));
            }
            None => {}
        },
        WindowEvent::CursorLeft { .. } if was_inside => {
            webview.notify_input_event(InputEvent::MouseLeftViewport(
                MouseLeftViewportEvent::default(),
            ));
        }
        WindowEvent::MouseInput {
            state: button_state,
            button,
            ..
        } => {
            let Some(point) = cursor else { return };
            let action = match button_state {
                ElementState::Pressed => MouseButtonAction::Down,
                ElementState::Released => MouseButtonAction::Up,
                _ => return,
            };
            webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action,
                mouse_button(*button),
                point.into(),
            )));
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let Some(point) = cursor else { return };
            let (x, y) = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    ((x * LINE_WIDTH) as f64, (y * LINE_HEIGHT) as f64)
                }
                MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
                _ => return,
            };
            // tao 0.37 reports GDK's discrete scrolls (wheel clicks without
            // smooth scrolling) as a zero delta; `connect_discrete_scroll`
            // delivers those.
            if x != 0.0 || y != 0.0 {
                wheel(webview, point, x, y);
            }
        }
        WindowEvent::KeyboardInput { event, .. } => {
            webview.notify_input_event(InputEvent::Keyboard(keyboard_event(
                event,
                state.modifiers.get(),
            )));
        }
        _ => {}
    }
}

fn wheel(webview: &WebView, point: DevicePoint, x: f64, y: f64) {
    webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
        WheelDelta {
            x,
            y,
            z: 0.0,
            mode: WheelMode::DeltaPixel,
        },
        point.into(),
    )));
}

/// Delivers the GTK window's discrete scroll events (one per wheel click,
/// as X11 sends them without smooth scrolling) to the webview, which tao
/// 0.37 reports with a zero delta. Smooth scrolls keep coming through tao.
/// `target` gives the webview and its place in the window (physical
/// pixels), or `None` once it is gone.
pub fn connect_discrete_scroll(
    gtk_window: &gtk::ApplicationWindow,
    target: impl Fn() -> Option<(WebView, (i32, i32, u32, u32))> + 'static,
) {
    gtk_window.connect_scroll_event(move |gtk_window, event| {
        // Same sign as tao's smooth deltas (and winit's): up is positive.
        let (x, y) = match event.direction() {
            ScrollDirection::Up => (0.0, 1.0),
            ScrollDirection::Down => (0.0, -1.0),
            ScrollDirection::Left => (1.0, 0.0),
            ScrollDirection::Right => (-1.0, 0.0),
            _ => return glib::Propagation::Proceed,
        };
        let Some((webview, (left, top, width, height))) = target() else {
            return glib::Propagation::Proceed;
        };
        let scale = gtk_window.scale_factor() as f64;
        let (px, py) = event.position();
        let point = DevicePoint::new(
            (px * scale) as f32 - left as f32,
            (py * scale) as f32 - top as f32,
        );
        if point.x >= 0.0 && point.y >= 0.0 && point.x < width as f32 && point.y < height as f32 {
            wheel(
                &webview,
                point,
                x * LINE_WIDTH as f64,
                y * LINE_HEIGHT as f64,
            );
        }
        glib::Propagation::Proceed
    });
}

fn mouse_button(button: MouseButton) -> ServoMouseButton {
    match button {
        MouseButton::Left => ServoMouseButton::Primary,
        MouseButton::Right => ServoMouseButton::Secondary,
        MouseButton::Middle => ServoMouseButton::Auxiliary,
        // GDK numbers the back and forward buttons 8 and 9.
        MouseButton::Other(8) => ServoMouseButton::Back,
        MouseButton::Other(9) => ServoMouseButton::Forward,
        MouseButton::Other(value) => ServoMouseButton::Other(value),
        _ => ServoMouseButton::Other(0),
    }
}

fn keyboard_event(event: &KeyEvent, modifiers: ModifiersState) -> KeyboardEvent {
    let state = match event.state {
        ElementState::Released => KeyState::Up,
        _ => KeyState::Down,
    };
    KeyboardEvent::new_without_event(
        state,
        key(&event.logical_key),
        code(event.physical_key),
        location(event.location),
        keyboard_modifiers(modifiers),
        event.repeat,
        false,
    )
}

/// tao's logical keys carry the UI Events names, except `Super` (`Meta`
/// there) and `Space` (a character there), so named keys go by name.
fn key(key: &TaoKey<'_>) -> Key {
    match key {
        TaoKey::Character(text) => Key::Character(text.to_string()),
        TaoKey::Space => Key::Character(" ".into()),
        TaoKey::Super => Key::Named(NamedKey::Meta),
        TaoKey::Unidentified(_) | TaoKey::Dead(_) => Key::Named(NamedKey::Unidentified),
        named => {
            Key::Named(NamedKey::from_str(&format!("{named:?}")).unwrap_or(NamedKey::Unidentified))
        }
    }
}

/// tao's key codes are the UI Events `code` names, except the Super keys.
fn code(code: KeyCode) -> Code {
    match code {
        KeyCode::SuperLeft => Code::MetaLeft,
        KeyCode::SuperRight => Code::MetaRight,
        KeyCode::Unidentified(_) => Code::Unidentified,
        code => Code::from_str(&format!("{code:?}")).unwrap_or(Code::Unidentified),
    }
}

fn location(location: KeyLocation) -> Location {
    match location {
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
        _ => Location::Standard,
    }
}

fn keyboard_modifiers(modifiers: ModifiersState) -> Modifiers {
    let mut result = Modifiers::empty();
    result.set(Modifiers::CONTROL, modifiers.control_key());
    result.set(Modifiers::SHIFT, modifiers.shift_key());
    result.set(Modifiers::ALT, modifiers.alt_key());
    result.set(Modifiers::META, modifiers.super_key());
    result
}
