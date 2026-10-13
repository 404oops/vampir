//! What a host that embeds a live native view needs from AppKit.
//!
//! A native view — a web view, a video player — is composited over the
//! GPUI window, so anything the toolkit draws inside that window is hidden
//! wherever the view is. The pieces here get round that: a
//! [`floating_menu`] in a transparent pop-up window of its own, and the
//! handles the rest of the toolkit uses to find the AppKit window behind a
//! GPUI one and tell its events from another window's.
//!
//! macOS only, behind the `native` feature.

use gpui::Window;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSView, NSWindow};

pub mod floating_menu;

/// The AppKit window behind a GPUI window, and GPUI's own view in it, as
/// addresses: the window to tell events apart by, the view to give the
/// keyboard to. Both are `0` when the window has no AppKit window yet.
pub fn ns_window_of(window: &Window) -> (usize, usize) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return (0, 0);
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return (0, 0);
    };
    // SAFETY: GPUI's view, alive while the window is.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let window = view.window().map_or(0, |w| Retained::as_ptr(&w) as usize);
    (window, handle.ns_view.as_ptr() as usize)
}

/// The window an event is for. AppKit can deliver keyboard and auxiliary
/// mouse events without one; those resolve through the key window, while
/// an event explicitly addressed to a window keeps that window.
pub fn input_window(event: &NSEvent) -> Option<Retained<NSWindow>> {
    let mtm = objc2::MainThreadMarker::new()?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    if let Some(window) = event.window(mtm) {
        return Some(window);
    }
    let number = event.windowNumber();
    if number > 0 {
        return app.windowWithWindowNumber(number);
    }
    app.keyWindow()
}

/// Whether `event` is for the window at `ns_window`. Every window's local
/// event monitors see every event in the app, so each has to ask.
pub fn event_in(event: &NSEvent, ns_window: usize) -> bool {
    ns_window != 0
        && input_window(event).is_some_and(|window| Retained::as_ptr(&window) as usize == ns_window)
}
