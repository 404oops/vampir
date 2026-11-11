# Local changes to the vendored GPUI-CE

Four changes to GPUI-CE 0.3.0.

- **Anchored popups**, in four crates: `WindowKind::AnchoredPopup`, which GPUI-CE defines and only its Wayland backend implements, works on macOS, Windows and X11. The toolkit's command bar (`src/command_bar.rs`) is a popup of this kind: a window owned by the host's, above it and no other application's, over a live native view the toolkit cannot draw over from inside the host window.
- **Native child windows on Windows**: a native child window a host embeds shows and paints ([below](#native-child-windows-on-windows)).
- **The macOS launch callback runs without the platform's lock.** 0.3.0 calls it inside `if let Some(callback) = platform.0.lock().finish_launching.take()`, and under the 2024 edition that guard lives to the end of the block, so the first thing a launch does through the platform — open a window, set the menus — waits on the lock forever. gpui-ce's main branch already takes the callback out first.
- **`App::windows_are_composited`** (`Platform::windows_are_composited`, true by default): whether the desktop blends one window over another. False on an X11 screen where nothing owns the `_NET_WM_CM_Sn` selection, where the clear parts of a window come out opaque. The command bar leaves its scrim out there and fills its own corners.

Each commit touching `vendor/` after the one that adds it is one of these, and says what it does in full.

## The contract every backend keeps

- **Owned.** The popup is owned by the window `PopupOptions::parent` names: a child window (AppKit), an owned window (Win32), a transient (X11). A parent that is not open is an error, never an unowned window.
- **Stacked above its parent only.** Never globally on top: no raised AppKit window level, no `WS_EX_TOPMOST`, no `_NET_WM_STATE_ABOVE`. A popup shown later is in front of one shown earlier. On X11 an override-redirect window is above every managed one while it is open, so a popup there relies on closing when its parent stops being the active window, as the command bar and its scrim do.
- **Placed by `gpui::popup::place_popup`.** One function in `gpui-ce` (`src/platform/popup.rs`) resolves the anchor, gravity, offset and the flip, slide and resize constraints against the parent's content area and the work area of its display. Each backend converts its own coordinates in and out, at the parent's scale. Tested in `tests/popup_placement.rs`.
- **Follows its parent.** A parent move places its popups again. `Window::set_popup_anchor` (new; a no-op on other kinds and backends) re-anchors one when its anchor moves without the parent moving, as on a resize. `resize` keeps the anchor.
- **Closes with its parent.** Each popup's own close callback runs once.
- **Keyboard.** `WindowOptions::focus: true` takes the keyboard, with IME as in any window. `focus: false` never does — not on show, not on a click — and still receives clicks: that is the toolkit's dimming scrim between the parent and the focused bar.
- **The parent stays active.** While its popup has the keyboard the parent is still the window in front, as under a menu: `is_active` and the activation callback count its popups, and report a change only once the platform has settled where the keyboard went. A popup reports going inactive when the keyboard leaves it.
- **`PopupOptions::grab` has no effect** on these backends.

## Per backend

| | macOS | Windows | X11 |
|---|---|---|---|
| Window | Activating `NSPanel`, normal level, `addChildWindow:ordered:` | `WS_POPUP` tool window, owner = parent `HWND` | Override-redirect `POPUP_MENU`, transient for the parent; never has the X input focus, the parent passes it its keystrokes |
| Follows parent | AppKit moves child windows; `windowDidMove` re-applies constraints | Owner's `WM_MOVE` / `WM_DPICHANGED` | Parent's `ConfigureNotify` |
| Minimised parent | Ordered out with it (AppKit) | Hidden with it (Win32) | Closed when the parent is unmapped or its `WM_STATE` stops being normal: minimised, or on a workspace that is not showing |
| Parent's active look | Popup never becomes main | `WM_NCACTIVATE` kept active for its own popups | The parent keeps the X input focus, so the window manager keeps drawing it active |
| No-focus popup | `canBecomeKeyWindow` is NO | `WS_EX_NOACTIVATE`, `MA_NOACTIVATE` | The same window, without the keystrokes |

## Native child windows on Windows

The command bar exists for a host window that embeds a live native view, and on Windows GPUI-CE could not show one at all. Two changes to `gpui_ce_windows` make a host's child `HWND` behave as a native view does over a GPUI window on macOS and X11:

- **Composited under its children.** The DirectComposition target is created with `topmost: false`, so GPUI's swap chain is drawn behind the window's child windows rather than over them. A window without children looks the same.
- **`WS_CLIPCHILDREN`.** Each frame ends with `ValidateRect` on the whole client area; without this style that also validated a child's area, so the child never received `WM_PAINT`.
A third, in 0.2.2, sent GPUI's private key-down message only to GPUI's own windows, since a foreign child answered it with `DefWindowProc`'s 0, which read as handled. 0.3.0 does that itself. GPUI's shortcuts do not fire while a native child has the keyboard, as they do not for any other Win32 control that has it.

GPUI's window is `WS_EX_NOREDIRECTIONBITMAP`, so a plain child window has no surface for the desktop compositor to show: the child has to be layered (`WS_EX_LAYERED`, Windows 8 and later), as the gallery's `EDIT` control is. A web view or video player that renders through DirectComposition itself needs nothing.

## What has been run where

- **macOS, 0.3.0:** on screen, in the gallery's Native view page, over an `NSTextView` and a spinning `NSProgressIndicator`: opening, typing, choosing, Escape (the caret back in the text view), a click on the parent, the scrim and a click on it, and the parent keeping its active title bar, in both schemes.
- **Windows, 0.2.2:** on screen on Windows 11 (x86_64-pc-windows-gnu; scale 1.0, and the bar's placement, size, choosing and typing at 2.0, and through a live change of the display from 2.0 to 1.0 with the window open), in the gallery's Native view page, over a layered `EDIT` control: opening, typing, choosing, Escape, a click on the parent, a click on the native view, another application coming forward, the scrim and a click on it, moving and resizing the parent with the bar open, minimising it, and the parent keeping its active caption. Not yet across monitors of different DPIs.
- **X11, 0.3.0:** on screen under GNOME Shell 48.7 (mutter) on Debian 13 (aarch64, virtio GPU, scale 1), in the gallery's Native view page, over the colour-cycling child window, in both schemes: opening by shortcut and by button, typing, the arrow keys, choosing, Escape (the gallery's shortcuts work again without a click), a click on the parent, another application coming forward, the scrim and a click on it, the parent keeping its active title bar, dragging the parent (the bar closes as deactivated) and closing it with the bar open. The same sequence in Xephyr under xfwm4 4.20 (composited: the scrim tints), and under openbox, IceWM and i3 with floating windows (not composited: no scrim, a press outside the bar closes it as deactivated), and under openbox with picom (the scrim tints). Also under those: moving and resizing the parent with the bar open (the bar and scrim follow), minimising it and switching workspace (the bar closes), a 2x screen (`Xft.dpi: 192`, under xfwm4), i3 with tiled windows, and dead keys and non-ASCII text typed into the bar. Not yet: an input method with composition, or more than one monitor.
- **Windows, 0.3.0:** type-checked and linted from macOS, and built and tested by CI, as X11 is. Not yet run on screen. `tools/check-windows.sh` and `tools/check-linux.sh` run the check, or the lints with `clippy` as the first argument: zig stands in for the C and resource compilers, the Windows renderer's shaders are allowed to be missing, and nothing is linked.
