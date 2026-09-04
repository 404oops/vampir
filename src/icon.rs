//! The application icon.
//!
//! Not a widget, but the same shape of problem: every platform keeps the
//! icon somewhere different, and none of those places is where a host would
//! think to look. macOS wants it handed to AppKit at startup unless the app
//! is bundled, Windows wants it linked into the executable as a resource,
//! X11 wants decoded pixels on the window, and Wayland will not take it from
//! the program at all.
//!
//! So the toolkit carries one, and a host swaps it for its own in a line:
//!
//! ```ignore
//! use vampir::icon::AppIcon;
//!
//! const ICON: AppIcon = AppIcon::png(include_bytes!("../assets/my-icon.png"));
//!
//! // ...in the application's startup, before the window opens:
//! ICON.install(cx);
//!
//! // ...and in the window's options, for the platforms that take one there:
//! options.icon = ICON.window_icon();
//! ```
//!
//! Artwork should be a square PNG, 512px or larger, drawn edge to edge.
//! macOS is the exception and it is handled here rather than left to the
//! host: see [`AppIcon::install`].
//!
//! Windows is the one platform this module cannot help with, because the
//! icon has to be in the binary before it runs. `build.rs` in this
//! repository shows the shape of it: write a one-line `.rc` naming an
//! `.ico` and hand it to whichever resource compiler is on the machine.

/// Vampir's own icon, for an application that has not drawn its own.
///
/// It is a placeholder, not a brand. Replace it the moment your app has a
/// face of its own.
const PLACEHOLDER: &[u8] = include_bytes!("../assets/icon-512.png");

/// The artwork an application shows in the dock, the task bar and its own
/// window.
///
/// `Default` is Vampir's placeholder; [`AppIcon::png`] is your own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppIcon {
    png: &'static [u8],
}

impl Default for AppIcon {
    fn default() -> Self {
        Self::default_icon()
    }
}

impl AppIcon {
    /// Vampir's placeholder, as a constant.
    ///
    /// The same thing [`Default`] gives, in a form a `const` can hold: an
    /// application names its icon once, at the top of the file, and
    /// `Default::default()` is not `const`.
    pub const fn default_icon() -> Self {
        Self::png(PLACEHOLDER)
    }

    /// An icon from PNG bytes, square and drawn edge to edge.
    ///
    /// `const`, so it can be a constant beside the `include_bytes!` that
    /// feeds it.
    pub const fn png(png: &'static [u8]) -> Self {
        Self { png }
    }

    /// The bytes, for a host that needs them for something this module does
    /// not cover — a tray icon, an about box, a file it writes out.
    pub const fn bytes(self) -> &'static [u8] {
        self.png
    }

    /// Hands the icon to the platform, where the platform takes one at
    /// runtime.
    ///
    /// On macOS that is the dock. An unbundled binary has no
    /// `Contents/Resources` for the dock to look in, so an application run
    /// straight out of `target/` shows the generic placeholder macOS
    /// synthesises — this replaces it.
    ///
    /// The artwork is inset to Apple's icon grid on the way. A square macOS
    /// icon fills 824 pixels of a 1024 canvas; artwork drawn edge to edge
    /// sits about a fifth larger than every icon beside it in the dock,
    /// which is what makes a new app look wrong before anyone can say why.
    /// Doing it here rather than asking for a second, pre-padded file means
    /// one PNG is enough to look right everywhere.
    ///
    /// Does nothing on the other platforms, which take their icon somewhere
    /// this cannot reach: see the module docs.
    #[allow(unused_variables)]
    pub fn install(self, cx: &gpui::App) {
        #[cfg(target_os = "macos")]
        install_macos(self.png);
    }

    /// The window icon, for the platforms that take one when the window
    /// opens.
    ///
    /// Pass it to [`gpui::WindowOptions::icon`]. X11 reads it as
    /// `_NET_WM_ICON`; every other platform ignores it, including Wayland,
    /// which matches a window to a `.desktop` file by app id and takes the
    /// icon named there instead.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub fn window_icon(self) -> Option<std::sync::Arc<image::RgbaImage>> {
        let decoded =
            image::load_from_memory_with_format(self.png, image::ImageFormat::Png).ok()?;
        Some(std::sync::Arc::new(decoded.into_rgba8()))
    }
}

/// Apple's icon grid: the body of a square icon fills this much of its
/// canvas, and the rest is the margin every other icon in the dock has.
#[cfg(target_os = "macos")]
const MACOS_BODY_FRACTION: f64 = 824.0 / 1024.0;

#[cfg(target_os = "macos")]
fn install_macos(png: &[u8]) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSApplication, NSCompositingOperation, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData, NSPoint, NSRect, NSSize};

    // AppKit is main-thread only, and there is nothing useful to do from
    // anywhere else.
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(png);
    let Some(art) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };

    const CANVAS: f64 = 1024.0;
    let body = CANVAS * MACOS_BODY_FRACTION;
    let inset = (CANVAS - body) / 2.0;

    let canvas = NSImage::initWithSize(NSImage::alloc(), NSSize::new(CANVAS, CANVAS));
    // A zero source rect means the whole image, which is what a plain scale
    // wants.
    let whole = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let into = NSRect::new(NSPoint::new(inset, inset), NSSize::new(body, body));

    // `lockFocus` is deprecated in favour of a block-based drawing handler,
    // which would mean carrying a block runtime for one composite. It still
    // does exactly what it says.
    #[allow(deprecated)]
    {
        canvas.lockFocus();
        art.drawInRect_fromRect_operation_fraction(
            into,
            whole,
            NSCompositingOperation::SourceOver,
            1.0,
        );
        canvas.unlockFocus();
    }

    // Safe: on the main thread, with an image that outlives the call.
    unsafe {
        NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&canvas));
    }
}

#[cfg(test)]
mod tests {
    use super::{AppIcon, PLACEHOLDER};

    #[test]
    fn the_placeholder_is_a_png() {
        // The PNG signature, so a swapped file that is not one fails here
        // rather than silently leaving the app with no icon.
        assert_eq!(&PLACEHOLDER[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn an_icon_carries_the_bytes_it_was_given() {
        const MINE: &[u8] = b"\x89PNG\r\n\x1a\nnot really";
        assert_eq!(AppIcon::png(MINE).bytes(), MINE);
        assert_eq!(AppIcon::default().bytes(), PLACEHOLDER);
        assert_ne!(AppIcon::png(MINE), AppIcon::default());
    }
}
