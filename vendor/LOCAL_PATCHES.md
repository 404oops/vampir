# Local changes to the vendored GPUI-CE

One change, in four crates: `WindowKind::AnchoredPopup`, which GPUI-CE 0.2.2 already defines and only its Wayland backend implements, now works on macOS, Windows and X11. The toolkit's command bar (`src/command_bar.rs`) is a popup of this kind: a window owned by the host's, above it and no other application's, over a live native view the toolkit cannot draw over from inside the host window.

Each commit touching `vendor/` after the one that adds it is one of these, and says what it does in full.

## The contract every backend keeps

- **Owned.** The popup is owned by the window `PopupOptions::parent` names: a child window (AppKit), an owned window (Win32), a transient (X11). A parent that is not open is an error, never an unowned window.
- **Stacked above its parent only.** Never globally on top: no raised AppKit window level, no `WS_EX_TOPMOST`, no `_NET_WM_STATE_ABOVE`. A popup shown later is in front of one shown earlier.
- **Placed by `gpui::popup::place_popup`.** One function in `gpui-ce` (`src/platform/popup.rs`) resolves the anchor, gravity, offset and the flip, slide and resize constraints against the parent's content area and the work area of its display. Each backend converts its own coordinates in and out, at the parent's scale. Tested in `tests/popup_placement.rs`.
- **Follows its parent.** A parent move places its popups again. `Window::set_popup_anchor` (new; a no-op on other kinds and backends) re-anchors one when its anchor moves without the parent moving, as on a resize. `resize` keeps the anchor.
- **Closes with its parent.** Each popup's own close callback runs once.
- **Keyboard.** `WindowOptions::focus: true` takes the keyboard, with IME as in any window. `focus: false` never does — not on show, not on a click — and still receives clicks: that is the toolkit's dimming scrim between the parent and the focused bar.
- **The parent stays active.** While its popup has the keyboard the parent is still the window in front, as under a menu: `is_active` and the activation callback count its popups, and report a change only once the platform has settled where the keyboard went. A popup reports going inactive when the keyboard leaves it.
- **`PopupOptions::grab` has no effect** on these backends.

## Per backend

| | macOS | Windows | X11 |
|---|---|---|---|
| Window | Activating `NSPanel`, normal level, `addChildWindow:ordered:` | `WS_POPUP` tool window, owner = parent `HWND` | Transient `DIALOG` (not modal), undecorated; override-redirect when `focus: false` |
| Follows parent | AppKit moves child windows; `windowDidMove` re-applies constraints | Owner's `WM_MOVE` / `WM_DPICHANGED` | Parent's `ConfigureNotify` |
| Minimised parent | Ordered out with it (AppKit) | Hidden with it (Win32) | Closed when the parent becomes iconic |
| Parent's active look | Popup never becomes main | `WM_NCACTIVATE` kept active for its own popups | GPUI-drawn title bars follow `is_active`; the window manager may draw its own frame inactive |
| No-focus popup | `canBecomeKeyWindow` is NO | `WS_EX_NOACTIVATE`, `MA_NOACTIVATE` | Override-redirect, `WM_HINTS` input off |

## What has been run where

- **macOS:** on screen, in the gallery's Native view page, over an `NSTextView` and a spinning `NSProgressIndicator`: opening, typing, choosing, Escape, a click on the parent, a click on the scrim, and the parent keeping its active title bar.
- **Windows, X11:** type-checked and linted from macOS (`tools/check-windows.sh`, `tools/check-linux.sh`, each with `clippy` as its first argument for the lints: zig stands in for the C and resource compilers, nothing is linked), and built and tested by CI on both. Not yet run on screen on either.
