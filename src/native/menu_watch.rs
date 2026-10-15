//! What macOS adds to the floating menu beyond what GPUI does everywhere:
//! a window that never takes the keyboard, so the field or page being
//! typed in keeps its caret, and an event monitor that walks the menu
//! with the keys and closes it on a press anywhere else in the app — a
//! press that goes no further, as with a native menu.

use std::{
    cell::{Cell, RefCell},
    ptr::NonNull,
};

use block2::RcBlock;
use gpui::{App, Window, WindowHandle};
use objc2::{
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_foundation::{NSNotificationCenter, NSObjectProtocol};

use crate::floating_menu::{Menu, Signal, Typed, typed};
use crate::keyboard::Key;

use super::{event_in, ns_window_of};

/// The menu's window keeps the keyboard away from the host's.
pub const KEYLESS: bool = true;

/// What closes a menu from outside it: presses and keys anywhere else in
/// the app, and the app going to the background. Dropped with the menu.
pub struct Watch {
    monitor: Option<Retained<AnyObject>>,
    resign: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    ns_window: usize,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: the token AppKit returned for this monitor.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        // SAFETY: the observer the notification center returned.
        unsafe {
            NSNotificationCenter::defaultCenter()
                .removeObserver(ProtocolObject::as_ref(&*self.resign))
        };
        allow_key(self.ns_window);
    }
}

/// The key a macOS key code names, for the keys the menu walks with.
fn key_for_code(code: u16) -> Option<Key> {
    Some(match code {
        53 => Key::Dismiss,
        36 | 76 | 49 => Key::Activate,
        125 => Key::Down,
        126 => Key::Up,
        123 => Key::Left,
        124 => Key::Right,
        115 => Key::Home,
        119 => Key::End,
        116 => Key::PageUp,
        121 => Key::PageDown,
        _ => return None,
    })
}

/// Watches the app's events for the menu in `window`.
pub fn watch<I: Clone + 'static>(
    window: &Window,
    menu: WindowHandle<Menu<I>>,
    cx: &App,
) -> Option<Watch> {
    let (ns_window, _) = ns_window_of(window);
    if ns_window == 0 {
        return None;
    }
    let done = std::rc::Rc::new(Cell::new(false));
    let deliver: std::rc::Rc<dyn Fn(Signal)> = {
        let done = done.clone();
        let cx = RefCell::new(cx.to_async());
        std::rc::Rc::new(move |signal: Signal| {
            if done.get() {
                return;
            }
            if matches!(signal, Signal::Close) {
                done.set(true);
            }
            let mut cx = cx.borrow_mut();
            let _ = menu.update(&mut *cx, |menu, window, cx| menu.signal(signal, window, cx));
        })
    };
    let on_resign = deliver.clone();
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit owns the event for the callback's duration.
        let event_ref = unsafe { event.as_ref() };
        if done.get() {
            return event.as_ptr();
        }
        if event_ref.r#type() == NSEventType::KeyDown {
            let flags = event_ref.modifierFlags();
            let text = event_ref.characters().map(|text| text.to_string());
            return match typed(
                key_for_code(event_ref.keyCode()),
                text.as_deref(),
                flags.contains(NSEventModifierFlags::Command)
                    || flags.contains(NSEventModifierFlags::Control),
            ) {
                Typed::Key(key) => {
                    deliver(Signal::Key(key));
                    std::ptr::null_mut()
                }
                Typed::Text(text) => {
                    deliver(Signal::Text(text));
                    std::ptr::null_mut()
                }
                Typed::Other => {
                    deliver(Signal::Close);
                    event.as_ptr()
                }
            };
        }
        if event_in(event_ref, ns_window) {
            return event.as_ptr();
        }
        deliver(Signal::Close);
        std::ptr::null_mut()
    });
    // SAFETY: the block returns the live event or null, as AppKit requires.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown
                | NSEventMask::RightMouseDown
                | NSEventMask::OtherMouseDown
                | NSEventMask::KeyDown,
            &handler,
        )
    };
    let block = RcBlock::new(move |_| on_resign(Signal::Close));
    // SAFETY: AppKit's own notification name and a block of the documented
    // type; the observer is removed with the watch.
    let resign = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSApplicationDidResignActiveNotification),
            None,
            None,
            &block,
        )
    };
    Some(Watch {
        monitor,
        resign,
        ns_window,
    })
}

/// Shows the menu's window, opened hidden: its required hidden draw has
/// warmed text and icons, and the animation's first frame replaces that
/// scene before AppKit makes the popup visible.
pub fn shown(window: &mut Window, cx: &mut App) {
    let (ns_window, _) = ns_window_of(window);
    if ns_window == 0 {
        return;
    }
    // SAFETY: the native window belongs to this live GPUI window.
    let ns_window = unsafe { &*(ns_window as *const NSWindow) };
    // The popup reserves transparent space for wider/taller submenus and
    // the edge gutter. A native shadow would outline it.
    ns_window.setHasShadow(false);
    never_key(ns_window);
    window.draw(cx).clear(cx);
    ns_window.orderFront(None);
}

/// Takes the menu's window off the screen at once, before GPUI gets round
/// to closing it.
pub fn hide(window: &Window) {
    let (ns_window, _) = ns_window_of(window);
    if ns_window == 0 {
        return;
    }
    // SAFETY: the menu's own window, still open.
    let ns_window = unsafe { &*(ns_window as *const NSWindow) };
    ns_window.orderOut(None);
}

/// Keeps `window` from becoming the key window while it's a menu.
///
/// GPUI's panels answer `canBecomeKeyWindow` with YES, so any click that
/// leaves the menu open — into a submenu, or back out of one — would take
/// the keyboard from the host's window. `becomesKeyOnlyIfNeeded` isn't
/// enough: AppKit still makes the panel key on a later click. So the
/// panel class's answer is replaced with one that says NO for the windows
/// in [`KEYLESS_WINDOWS`], and what it said before for any other.
fn never_key(window: &NSWindow) {
    use objc2::runtime::{AnyObject, Bool, Sel};

    type CanBecomeKey = unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool;

    unsafe extern "C-unwind" fn can_become_key(this: &AnyObject, sel: Sel) -> Bool {
        let this_ptr = this as *const AnyObject as usize;
        if KEYLESS_WINDOWS.with(|set| set.borrow().contains(&this_ptr)) {
            return Bool::NO;
        }
        let Some(original) = ORIGINAL_CAN_BECOME_KEY.with(Cell::get) else {
            return Bool::YES;
        };
        // SAFETY: the implementation this one replaced, of the same shape.
        let original: CanBecomeKey = unsafe { std::mem::transmute(original) };
        unsafe { original(this, sel) }
    }

    KEYLESS_WINDOWS.with(|set| set.borrow_mut().insert(window as *const NSWindow as usize));
    if ORIGINAL_CAN_BECOME_KEY.with(Cell::get).is_some() {
        return;
    }
    let Some(method) = window
        .class()
        .instance_method(objc2::sel!(canBecomeKeyWindow))
    else {
        return;
    };
    let replacement: CanBecomeKey = can_become_key;
    // SAFETY: `canBecomeKeyWindow` takes nothing and returns a BOOL, as
    // the replacement does; the original is kept to answer for other
    // windows of the class.
    let original = unsafe {
        method.set_implementation(std::mem::transmute::<CanBecomeKey, objc2::runtime::Imp>(
            replacement,
        ))
    };
    ORIGINAL_CAN_BECOME_KEY.with(|slot| slot.set(Some(original)));
}

/// Lets `ns_window` become key again, once it's no longer a menu.
fn allow_key(ns_window: usize) {
    KEYLESS_WINDOWS.with(|set| set.borrow_mut().remove(&ns_window));
}

thread_local! {
    /// The menu windows open: those answer `canBecomeKeyWindow` with NO.
    static KEYLESS_WINDOWS: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
    /// What GPUI's panel class answered `canBecomeKeyWindow` with, before
    /// [`never_key`] replaced it.
    static ORIGINAL_CAN_BECOME_KEY: Cell<Option<objc2::runtime::Imp>> = const { Cell::new(None) };
}
