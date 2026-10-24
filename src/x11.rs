//! The webview's native surface under X11: a child window of the tao
//! window's toplevel, on an X connection of the runtime's own.
//!
//! The child takes no input (an empty input region), so the pointer and the
//! wheel reach the GTK window under it and come back as tao events, which the
//! runtime forwards to Servo. GTK knows nothing of the child: it keeps
//! drawing its toplevel, which X clips by its children, so the GL surface
//! shows on top. This is the technique VODscribe's player uses for its own
//! surface.

use std::cell::RefCell;
use std::ffi::{CString, c_ulong};
use std::ptr::NonNull;
use std::rc::Rc;

use gtk::prelude::*;
use raw_window_handle::{
    DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle, XlibDisplayHandle,
    XlibWindowHandle,
};
use x11_dl::{xfixes, xlib};

/// One X connection for every child window, opened on first use and never
/// closed: surfman's EGL display is made on it, and Mesa keeps using it until
/// the process ends, after the last rendering context is gone (dropping the
/// connection first could leave EGL on a closed display).
struct Connection {
    xlib: xlib::Xlib,
    xfixes: xfixes::Xlib,
    display: *mut xlib::Display,
    screen: i32,
}

thread_local! {
    static CONNECTION: RefCell<Option<Rc<Connection>>> = const { RefCell::new(None) };
}

impl Connection {
    fn get(display_name: &str) -> Result<Rc<Connection>, String> {
        CONNECTION.with(|slot| {
            if let Some(connection) = &*slot.borrow() {
                return Ok(connection.clone());
            }
            let xlib = xlib::Xlib::open().map_err(|e| format!("libX11: {e}"))?;
            let xfixes = xfixes::Xlib::open().map_err(|e| format!("libXfixes: {e}"))?;
            let name = CString::new(display_name).map_err(|e| e.to_string())?;
            // SAFETY: plain Xlib calls; the display is checked before use.
            let (display, screen) = unsafe {
                let display = (xlib.XOpenDisplay)(name.as_ptr());
                if display.is_null() {
                    return Err(format!("can't connect to X display {display_name:?}"));
                }
                (display, (xlib.XDefaultScreen)(display))
            };
            let connection = Rc::new(Connection {
                xlib,
                xfixes,
                display,
                screen,
            });
            *slot.borrow_mut() = Some(connection.clone());
            Ok(connection)
        })
    }
}

/// A child window of a GTK toplevel, in physical pixels.
pub struct ChildWindow {
    connection: Rc<Connection>,
    window: xlib::Window,
    colormap: xlib::Colormap,
}

impl ChildWindow {
    pub fn new(
        parent: &gtk::gdk::Window,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<ChildWindow, String> {
        let parent_xid = parent
            .downcast_ref::<gdkx11::X11Window>()
            .ok_or("the window is not an X11 window (Wayland is not supported yet)")?
            .xid();
        let gdk_display = parent.display();
        // GTK's own connection may not have sent the toplevel's creation yet;
        // ours must not name a parent the server has not seen.
        gdk_display.sync();
        let connection = Connection::get(gdk_display.name().as_str())?;
        // SAFETY: Xlib calls on our own connection, on a parent the server knows.
        unsafe {
            let display = connection.display;
            let screen = connection.screen;
            // The screen's own opaque visual (GTK's toplevel may have an RGBA one).
            let visual = (connection.xlib.XDefaultVisual)(display, screen);
            let depth = (connection.xlib.XDefaultDepth)(display, screen);
            let colormap =
                (connection.xlib.XCreateColormap)(display, parent_xid, visual, xlib::AllocNone);
            let mut attributes: xlib::XSetWindowAttributes = std::mem::zeroed();
            attributes.colormap = colormap;
            // No background: X never clears the window, so nothing flashes
            // between GL frames.
            attributes.background_pixmap = 0;
            attributes.border_pixel = 0;
            let window = (connection.xlib.XCreateWindow)(
                display,
                parent_xid,
                x,
                y,
                width.max(1),
                height.max(1),
                0,
                depth,
                xlib::InputOutput as u32,
                visual,
                xlib::CWColormap | xlib::CWBackPixmap | xlib::CWBorderPixel,
                &mut attributes,
            );
            if window == 0 {
                (connection.xlib.XFreeColormap)(display, colormap);
                return Err("XCreateWindow failed".into());
            }
            // No input: the pointer goes to the GTK window under it.
            const SHAPE_INPUT: i32 = 2;
            let nothing = (connection.xfixes.XFixesCreateRegion)(display, std::ptr::null_mut(), 0);
            (connection.xfixes.XFixesSetWindowShapeRegion)(
                display,
                window,
                SHAPE_INPUT,
                0,
                0,
                nothing,
            );
            (connection.xfixes.XFixesDestroyRegion)(display, nothing);
            (connection.xlib.XMapWindow)(display, window);
            (connection.xlib.XSync)(display, 0);
            Ok(ChildWindow {
                connection,
                window,
                colormap,
            })
        }
    }

    pub fn set_geometry(&self, x: i32, y: i32, width: u32, height: u32) {
        // SAFETY: our window, on our connection.
        unsafe {
            (self.connection.xlib.XMoveResizeWindow)(
                self.connection.display,
                self.window,
                x,
                y,
                width.max(1),
                height.max(1),
            );
            (self.connection.xlib.XFlush)(self.connection.display);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        // SAFETY: our window, on our connection.
        unsafe {
            if visible {
                (self.connection.xlib.XMapWindow)(self.connection.display, self.window);
            } else {
                (self.connection.xlib.XUnmapWindow)(self.connection.display, self.window);
            }
            (self.connection.xlib.XFlush)(self.connection.display);
        }
    }

    pub fn display_handle(&self) -> DisplayHandle<'_> {
        let raw = RawDisplayHandle::Xlib(XlibDisplayHandle::new(
            NonNull::new(self.connection.display.cast()),
            self.connection.screen,
        ));
        // SAFETY: the connection is never closed (see `Connection`).
        unsafe { DisplayHandle::borrow_raw(raw) }
    }

    pub fn window_handle(&self) -> WindowHandle<'_> {
        let raw = RawWindowHandle::Xlib(XlibWindowHandle::new(self.window as c_ulong));
        // SAFETY: the window lives as long as `self`.
        unsafe { WindowHandle::borrow_raw(raw) }
    }
}

impl Drop for ChildWindow {
    fn drop(&mut self) {
        // SAFETY: ours, made in `new`.
        unsafe {
            (self.connection.xlib.XDestroyWindow)(self.connection.display, self.window);
            (self.connection.xlib.XFreeColormap)(self.connection.display, self.colormap);
            (self.connection.xlib.XFlush)(self.connection.display);
        }
    }
}
