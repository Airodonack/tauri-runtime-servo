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
use tao::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
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
    let cursor = state
        .cursor
        .get()
        .map(|point| to_webview(point, origin))
        .filter(|point| inside(*point, size));
    let was_inside = previous
        .map(|point| to_webview(point, origin))
        .is_some_and(|point| inside(point, size));
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
            let Some((x, y)) = wheel_delta(delta) else {
                return;
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
                event.state,
                &event.logical_key,
                event.physical_key,
                event.location,
                event.repeat,
                state.modifiers.get(),
            )));
        }
        _ => {}
    }
}

/// A point in the window's physical pixels, relative to the webview whose
/// top-left corner is at `origin`.
fn to_webview((x, y): (f64, f64), origin: (i32, i32)) -> DevicePoint {
    DevicePoint::new((x - origin.0 as f64) as f32, (y - origin.1 as f64) as f32)
}

/// Whether a webview-relative point falls in a webview of `size`: its left
/// and top edges are in, its right and bottom ones out.
fn inside(point: DevicePoint, size: (u32, u32)) -> bool {
    point.x >= 0.0 && point.y >= 0.0 && point.x < size.0 as f32 && point.y < size.1 as f32
}

/// A tao wheel delta in pixels; lines count as servoshell's line size.
fn wheel_delta(delta: &MouseScrollDelta) -> Option<(f64, f64)> {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            Some(((x * LINE_WIDTH) as f64, (y * LINE_HEIGHT) as f64))
        }
        MouseScrollDelta::PixelDelta(position) => Some((position.x, position.y)),
        _ => None,
    }
}

/// One GDK wheel click in pixels, with the same sign as tao's smooth
/// deltas (and winit's): up is positive. `None` for smooth scrolls.
fn discrete_scroll_delta(direction: ScrollDirection) -> Option<(f64, f64)> {
    let (x, y) = match direction {
        ScrollDirection::Up => (0.0, 1.0),
        ScrollDirection::Down => (0.0, -1.0),
        ScrollDirection::Left => (1.0, 0.0),
        ScrollDirection::Right => (-1.0, 0.0),
        _ => return None,
    };
    Some((x * LINE_WIDTH as f64, y * LINE_HEIGHT as f64))
}

/// A GDK event position (logical pixels of the GTK window) relative to the
/// webview whose top-left corner is at `origin` (physical pixels).
fn gtk_to_webview((x, y): (f64, f64), scale: f64, origin: (i32, i32)) -> DevicePoint {
    DevicePoint::new(
        (x * scale) as f32 - origin.0 as f32,
        (y * scale) as f32 - origin.1 as f32,
    )
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
    window: &tao::window::Window,
    target: impl Fn() -> Option<(WebView, (i32, i32, u32, u32))> + 'static,
) {
    use tao::platform::unix::WindowExtUnix;
    let gtk_window = window.gtk_window();
    gtk_window.connect_scroll_event(move |gtk_window, event| {
        let Some((x, y)) = discrete_scroll_delta(event.direction()) else {
            return glib::Propagation::Proceed;
        };
        let Some((webview, (left, top, width, height))) = target() else {
            return glib::Propagation::Proceed;
        };
        let scale = gtk_window.scale_factor() as f64;
        let point = gtk_to_webview(event.position(), scale, (left, top));
        if inside(point, (width, height)) {
            wheel(&webview, point, x, y);
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

/// A tao key event's parts (tao's `KeyEvent` cannot be built outside tao)
/// as Servo's keyboard event.
fn keyboard_event(
    state: ElementState,
    logical_key: &TaoKey<'_>,
    physical_key: KeyCode,
    key_location: KeyLocation,
    repeat: bool,
    modifiers: ModifiersState,
) -> KeyboardEvent {
    let state = match state {
        ElementState::Released => KeyState::Up,
        _ => KeyState::Down,
    };
    KeyboardEvent::new_without_event(
        state,
        key(logical_key),
        code(physical_key),
        location(key_location),
        keyboard_modifiers(modifiers),
        repeat,
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

#[cfg(test)]
mod tests {
    use tao::keyboard::NativeKeyCode;

    use super::*;

    #[test]
    fn characters_keep_their_text_and_case() {
        assert_eq!(key(&TaoKey::Character("a")), Key::Character("a".into()));
        assert_eq!(key(&TaoKey::Character("A")), Key::Character("A".into()));
        assert_eq!(key(&TaoKey::Character("é")), Key::Character("é".into()));
    }

    #[test]
    fn space_is_a_character_and_super_is_meta() {
        assert_eq!(key(&TaoKey::Space), Key::Character(" ".into()));
        assert_eq!(key(&TaoKey::Super), Key::Named(NamedKey::Meta));
    }

    #[test]
    fn named_keys_go_by_their_ui_events_name() {
        let cases = [
            (TaoKey::Enter, NamedKey::Enter),
            (TaoKey::Escape, NamedKey::Escape),
            (TaoKey::Backspace, NamedKey::Backspace),
            (TaoKey::Tab, NamedKey::Tab),
            (TaoKey::ArrowLeft, NamedKey::ArrowLeft),
            (TaoKey::PageDown, NamedKey::PageDown),
            (TaoKey::F1, NamedKey::F1),
            (TaoKey::F12, NamedKey::F12),
            (TaoKey::Shift, NamedKey::Shift),
            (TaoKey::Control, NamedKey::Control),
            (TaoKey::Alt, NamedKey::Alt),
            (TaoKey::AltGraph, NamedKey::AltGraph),
            (TaoKey::CapsLock, NamedKey::CapsLock),
        ];
        for (tao, servo) in cases {
            assert_eq!(key(&tao), Key::Named(servo), "{tao:?}");
        }
    }

    #[test]
    fn dead_and_unidentified_keys_are_unidentified() {
        let unidentified = Key::Named(NamedKey::Unidentified);
        // A dead acute accent, the first half of "é" on many layouts.
        assert_eq!(key(&TaoKey::Dead(Some('\u{b4}'))), unidentified);
        assert_eq!(key(&TaoKey::Dead(None)), unidentified);
        assert_eq!(
            key(&TaoKey::Unidentified(NativeKeyCode::Unidentified)),
            unidentified
        );
        assert_eq!(
            key(&TaoKey::Unidentified(NativeKeyCode::Gtk(0xfe51))),
            unidentified
        );
    }

    #[test]
    fn key_codes_go_by_name_and_super_is_meta() {
        let cases = [
            (KeyCode::KeyA, Code::KeyA),
            (KeyCode::Digit1, Code::Digit1),
            (KeyCode::Enter, Code::Enter),
            (KeyCode::Space, Code::Space),
            (KeyCode::ShiftLeft, Code::ShiftLeft),
            (KeyCode::ControlRight, Code::ControlRight),
            (KeyCode::Numpad5, Code::Numpad5),
            (KeyCode::F12, Code::F12),
            (KeyCode::SuperLeft, Code::MetaLeft),
            (KeyCode::SuperRight, Code::MetaRight),
        ];
        for (tao, servo) in cases {
            assert_eq!(code(tao), servo, "{tao:?}");
        }
        assert_eq!(
            code(KeyCode::Unidentified(NativeKeyCode::Gtk(0))),
            Code::Unidentified
        );
    }

    #[test]
    fn key_locations_map_one_to_one() {
        assert_eq!(location(KeyLocation::Standard), Location::Standard);
        assert_eq!(location(KeyLocation::Left), Location::Left);
        assert_eq!(location(KeyLocation::Right), Location::Right);
        assert_eq!(location(KeyLocation::Numpad), Location::Numpad);
    }

    #[test]
    fn modifier_combinations_carry_over() {
        assert_eq!(
            keyboard_modifiers(ModifiersState::empty()),
            Modifiers::empty()
        );
        assert_eq!(
            keyboard_modifiers(ModifiersState::CONTROL | ModifiersState::SHIFT),
            Modifiers::CONTROL | Modifiers::SHIFT
        );
        assert_eq!(
            keyboard_modifiers(ModifiersState::ALT | ModifiersState::SUPER),
            Modifiers::ALT | Modifiers::META
        );
        assert_eq!(
            keyboard_modifiers(ModifiersState::all()),
            Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT | Modifiers::META
        );
    }

    #[test]
    fn a_key_event_carries_state_repeat_and_modifiers() {
        let event = keyboard_event(
            ElementState::Pressed,
            &TaoKey::Character("c"),
            KeyCode::KeyC,
            KeyLocation::Standard,
            false,
            ModifiersState::CONTROL,
        )
        .event;
        assert_eq!(event.state, KeyState::Down);
        assert_eq!(event.key, Key::Character("c".into()));
        assert_eq!(event.code, Code::KeyC);
        assert_eq!(event.modifiers, Modifiers::CONTROL);
        assert!(!event.repeat);
        assert!(!event.is_composing);

        let event = keyboard_event(
            ElementState::Released,
            &TaoKey::Shift,
            KeyCode::ShiftRight,
            KeyLocation::Right,
            true,
            ModifiersState::SHIFT,
        )
        .event;
        assert_eq!(event.state, KeyState::Up);
        assert_eq!(event.location, Location::Right);
        assert!(event.repeat);
    }

    #[test]
    fn mouse_buttons_include_back_and_forward() {
        assert_eq!(mouse_button(MouseButton::Left), ServoMouseButton::Primary);
        assert_eq!(
            mouse_button(MouseButton::Right),
            ServoMouseButton::Secondary
        );
        assert_eq!(
            mouse_button(MouseButton::Middle),
            ServoMouseButton::Auxiliary
        );
        assert_eq!(mouse_button(MouseButton::Other(8)), ServoMouseButton::Back);
        assert_eq!(
            mouse_button(MouseButton::Other(9)),
            ServoMouseButton::Forward
        );
        assert_eq!(
            mouse_button(MouseButton::Other(12)),
            ServoMouseButton::Other(12)
        );
    }

    #[test]
    fn points_are_relative_to_the_webview() {
        let point = to_webview((150.5, 80.0), (100, 50));
        assert_eq!((point.x, point.y), (50.5, 30.0));
        // Left of and above the webview: negative, so outside.
        let point = to_webview((10.0, 10.0), (100, 50));
        assert_eq!((point.x, point.y), (-90.0, -40.0));
        assert!(!inside(point, (800, 600)));
    }

    #[test]
    fn a_webview_holds_its_top_left_edge_but_not_its_bottom_right() {
        let size = (800, 600);
        assert!(inside(DevicePoint::new(0.0, 0.0), size));
        assert!(inside(DevicePoint::new(799.5, 599.5), size));
        assert!(!inside(DevicePoint::new(800.0, 10.0), size));
        assert!(!inside(DevicePoint::new(10.0, 600.0), size));
        assert!(!inside(DevicePoint::new(-0.5, 10.0), size));
    }

    #[test]
    fn a_zero_sized_webview_holds_nothing() {
        assert!(!inside(DevicePoint::new(0.0, 0.0), (0, 0)));
    }

    #[test]
    fn gtk_positions_scale_to_physical_pixels() {
        for (scale, expected) in [
            (1.0, (90.0, 50.0)),
            (1.25, (115.0, 65.0)),
            (2.0, (190.0, 110.0)),
        ] {
            let point = gtk_to_webview((100.0, 60.0), scale, (10, 10));
            assert_eq!((point.x, point.y), expected, "scale {scale}");
        }
    }

    #[test]
    fn wheel_lines_become_pixels() {
        assert_eq!(
            wheel_delta(&MouseScrollDelta::LineDelta(0.0, -1.0)),
            Some((0.0, -76.0))
        );
        assert_eq!(
            wheel_delta(&MouseScrollDelta::LineDelta(0.5, 2.0)),
            Some((38.0, 152.0))
        );
        assert_eq!(
            wheel_delta(&MouseScrollDelta::PixelDelta(
                tao::dpi::PhysicalPosition::new(3.0, -12.5)
            )),
            Some((3.0, -12.5))
        );
        // A zero delta is reported as one; `forward` drops it.
        assert_eq!(
            wheel_delta(&MouseScrollDelta::LineDelta(0.0, 0.0)),
            Some((0.0, 0.0))
        );
    }

    #[test]
    fn a_wheel_click_scrolls_one_line_with_tao_s_sign() {
        assert_eq!(
            discrete_scroll_delta(ScrollDirection::Up),
            Some((0.0, 76.0))
        );
        assert_eq!(
            discrete_scroll_delta(ScrollDirection::Down),
            Some((0.0, -76.0))
        );
        assert_eq!(
            discrete_scroll_delta(ScrollDirection::Left),
            Some((76.0, 0.0))
        );
        assert_eq!(
            discrete_scroll_delta(ScrollDirection::Right),
            Some((-76.0, 0.0))
        );
        assert_eq!(discrete_scroll_delta(ScrollDirection::Smooth), None);
    }
}
