//! Monitors: tao's handles converted to Tauri's [`Monitor`] (adapted from
//! tauri-runtime-wry's monitor module).

use gtk::prelude::MonitorExt as _;
use tao::monitor::MonitorHandle;
use tao::platform::unix::MonitorHandleExtUnix;
use tauri_runtime::dpi::{
    LogicalPosition, LogicalSize, PhysicalPosition, PhysicalRect, PhysicalSize, Position,
};
use tauri_runtime::monitor::Monitor;

/// The monitor's work area (the screen minus panels), from GDK.
pub fn work_area(monitor: &MonitorHandle) -> PhysicalRect<i32, u32> {
    let rect = monitor.gdk_monitor().workarea();
    logical_rect_to_physical(
        (rect.x(), rect.y()),
        (rect.width(), rect.height()),
        monitor.scale_factor(),
    )
}

/// GDK's rectangles are in logical pixels; Tauri's monitors in physical ones.
fn logical_rect_to_physical(
    (x, y): (i32, i32),
    (width, height): (i32, i32),
    scale_factor: f64,
) -> PhysicalRect<i32, u32> {
    PhysicalRect {
        size: LogicalSize::new(width as u32, height as u32).to_physical(scale_factor),
        position: LogicalPosition::new(x, y).to_physical(scale_factor),
    }
}

pub fn to_monitor(monitor: MonitorHandle) -> Monitor {
    Monitor {
        name: monitor.name(),
        position: monitor.position(),
        size: monitor.size(),
        work_area: work_area(&monitor),
        scale_factor: monitor.scale_factor(),
    }
}

/// The monitor containing a (physical or logical) position.
pub fn find_monitor_for_position(
    monitors: impl Iterator<Item = MonitorHandle>,
    window_position: Position,
) -> Option<MonitorHandle> {
    monitors.into_iter().find(|m| {
        contains(
            m.position(),
            m.size(),
            window_position.to_physical::<i32>(m.scale_factor()),
        )
    })
}

/// Whether a monitor at `position` of `size` holds `point` (all physical):
/// the left and top edges are in, the right and bottom ones are the next
/// monitor's.
fn contains(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    point: PhysicalPosition<i32>,
) -> bool {
    position.x <= point.x
        && point.x < position.x + size.width as i32
        && position.y <= point.y
        && point.y < position.y + size.height as i32
}

/// Where a window of this size sits centred in the monitor's work area.
pub fn center_position(
    window_size: PhysicalSize<u32>,
    monitor: &MonitorHandle,
) -> PhysicalPosition<i32> {
    centered_in(window_size, work_area(monitor))
}

/// Where a window of this size sits centred in a work area. A window larger
/// than the area overhangs it equally on both sides (a negative offset).
fn centered_in(
    window_size: PhysicalSize<u32>,
    work_area: PhysicalRect<i32, u32>,
) -> PhysicalPosition<i32> {
    PhysicalPosition::new(
        (work_area.size.width as i32 - window_size.width as i32) / 2 + work_area.position.x,
        (work_area.size.height as i32 - window_size.height as i32) / 2 + work_area.position.y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
        }
    }

    /// `PhysicalRect` has no `PartialEq`.
    fn parts(rect: PhysicalRect<i32, u32>) -> (i32, i32, u32, u32) {
        (
            rect.position.x,
            rect.position.y,
            rect.size.width,
            rect.size.height,
        )
    }

    #[test]
    fn a_work_area_scales_with_the_monitor() {
        // A 1920x1080 screen with a 32 px panel on top, at 1, 1.25 and 2.
        let at = |scale| parts(logical_rect_to_physical((0, 32), (1920, 1048), scale));
        assert_eq!(at(1.0), (0, 32, 1920, 1048));
        assert_eq!(at(1.25), (0, 40, 2400, 1310));
        assert_eq!(at(2.0), (0, 64, 3840, 2096));
    }

    #[test]
    fn a_work_area_left_of_the_primary_monitor_keeps_its_sign() {
        assert_eq!(
            parts(logical_rect_to_physical((-1280, -100), (1280, 1024), 1.25)),
            (-1600, -125, 1600, 1280)
        );
    }

    #[test]
    fn an_empty_work_area_stays_empty() {
        assert_eq!(
            parts(logical_rect_to_physical((0, 0), (0, 0), 2.0)),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn a_monitor_holds_its_top_left_edge_but_not_its_bottom_right() {
        let position = PhysicalPosition::new(0, 0);
        let size = PhysicalSize::new(1920, 1080);
        let point = PhysicalPosition::new;
        assert!(contains(position, size, point(0, 0)));
        assert!(contains(position, size, point(1919, 1079)));
        assert!(!contains(position, size, point(1920, 500)));
        assert!(!contains(position, size, point(500, 1080)));
        assert!(!contains(position, size, point(-1, 0)));
    }

    #[test]
    fn a_monitor_at_negative_coordinates_holds_negative_points() {
        // A second monitor to the left of the primary one.
        let position = PhysicalPosition::new(-2560, -200);
        let size = PhysicalSize::new(2560, 1440);
        assert!(contains(position, size, PhysicalPosition::new(-2560, -200)));
        assert!(contains(position, size, PhysicalPosition::new(-1, 0)));
        assert!(!contains(position, size, PhysicalPosition::new(0, 0)));
    }

    #[test]
    fn a_zero_sized_monitor_holds_nothing() {
        let origin = PhysicalPosition::new(0, 0);
        assert!(!contains(origin, PhysicalSize::new(0, 0), origin));
    }

    #[test]
    fn a_window_is_centred_in_the_work_area_not_the_screen() {
        let window = PhysicalSize::new(800, 600);
        assert_eq!(
            centered_in(window, rect(0, 0, 1920, 1080)),
            PhysicalPosition::new(560, 240)
        );
        // A panel on top moves the centre down by its height.
        assert_eq!(
            centered_in(window, rect(0, 40, 1920, 1040)),
            PhysicalPosition::new(560, 260)
        );
        // A work area on a monitor to the left.
        assert_eq!(
            centered_in(window, rect(-1920, 0, 1920, 1080)),
            PhysicalPosition::new(-1360, 240)
        );
    }

    #[test]
    fn a_window_larger_than_the_work_area_overhangs_both_sides() {
        assert_eq!(
            centered_in(PhysicalSize::new(2000, 1200), rect(0, 0, 1920, 1080)),
            PhysicalPosition::new(-40, -60)
        );
    }

    #[test]
    fn a_zero_sized_window_sits_at_the_centre_point() {
        assert_eq!(
            centered_in(PhysicalSize::new(0, 0), rect(0, 0, 1920, 1080)),
            PhysicalPosition::new(960, 540)
        );
    }
}
