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
    let scale_factor = monitor.scale_factor();
    PhysicalRect {
        size: LogicalSize::new(rect.width() as u32, rect.height() as u32).to_physical(scale_factor),
        position: LogicalPosition::new(rect.x(), rect.y()).to_physical(scale_factor),
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
        let monitor_pos = m.position();
        let monitor_size = m.size();
        let window_position = window_position.to_physical::<i32>(m.scale_factor());
        monitor_pos.x <= window_position.x
            && window_position.x < monitor_pos.x + monitor_size.width as i32
            && monitor_pos.y <= window_position.y
            && window_position.y < monitor_pos.y + monitor_size.height as i32
    })
}

/// Where a window of this size sits centred in the monitor's work area.
pub fn center_position(
    window_size: PhysicalSize<u32>,
    monitor: &MonitorHandle,
) -> PhysicalPosition<i32> {
    let work_area = work_area(monitor);
    PhysicalPosition::new(
        (work_area.size.width as i32 - window_size.width as i32) / 2 + work_area.position.x,
        (work_area.size.height as i32 - window_size.height as i32) / 2 + work_area.position.y,
    )
}
