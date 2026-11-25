//! The picture-in-picture window: the fullscreen player in a small window that stays above other
//! apps. Windows and X11 raise it here. A Wayland compositor lets no app raise its own window, so
//! there the title is the one compositor extensions such as GNOME's In Picture look for.

use anyhow::Result;
use gpui::{
    App, AppContext as _, Bounds, Pixels, Size, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use state::Sonora;
#[cfg(target_os = "linux")]
use state::{Outcome, Toast, Toasts};
use ui::ActiveTheme as _;
use views::PipView;

/// The title browsers give their own picture-in-picture windows, which compositor rules match
/// word for word. It stays untranslated.
const TITLE: &str = "Picture-in-Picture";
const FIRST_SIZE: Size<Pixels> = size(px(440.), px(480.));
const LEAST_SIZE: Size<Pixels> = size(px(320.), px(320.));
/// How far the window opens from the corner of the screen.
const MARGIN: Pixels = px(24.);
/// The GNOME Shell extension that keeps the window on top on Wayland, and its page.
#[cfg(target_os = "linux")]
const EXTENSION: &str = "in-picture@filiprund.cz";
#[cfg(target_os = "linux")]
const EXTENSION_PAGE: &str = "https://extensions.gnome.org/extension/8692/in-picture/";

/// Opens the window, or closes the one that is open. It opens only beside a main window.
pub fn toggle(cx: &mut App) {
    if close(cx) || crate::main_window(cx).is_none() {
        return;
    }
    if let Err(error) = open(cx) {
        log::warn!("pip: cannot open the window: {error:#}");
    }
}

/// Closes the window and returns whether one was open.
pub fn close(cx: &mut App) -> bool {
    let Some(window) = find(cx) else {
        return false;
    };
    // An action dispatched from the window itself still holds it, so it goes once that ends.
    cx.defer(move |cx| {
        window
            .update(cx, |_, window, _| window.remove_window())
            .ok();
    });
    true
}

fn find(cx: &App) -> Option<WindowHandle<PipView>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<PipView>())
}

fn open(cx: &mut App) -> Result<()> {
    let settings = Sonora::global(cx).settings.read(cx);
    let saver = settings.saver();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let decorations = settings.window_decorations();
    let look = settings.look();
    let bounds = match cx.primary_display() {
        Some(display) => {
            let screen = display.visible_bounds();
            let corner = screen.bottom_right() - point(MARGIN, MARGIN);
            Bounds::new(
                corner - point(FIRST_SIZE.width, FIRST_SIZE.height),
                FIRST_SIZE,
            )
        }
        None => Bounds::centered(None, FIRST_SIZE, cx),
    };

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: ui::backdrop(look.blur_window, look.transparent),
            titlebar: Some(TitlebarOptions {
                title: Some(TITLE.into()),
                appears_transparent: true,
                ..Default::default()
            }),
            inactive_frame_interval: saver.interval(),
            app_owns_titlebar_drag: true,
            app_id: Some(crate::assets::app_id().into()),
            window_min_size: Some(LEAST_SIZE),
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            window_decorations: Some(decorations),
            ..Default::default()
        },
        |window, cx| {
            #[cfg(target_os = "linux")]
            crate::x11_icon::install(window);
            window.set_rem_size(cx.theme().font_size);
            #[cfg(target_os = "windows")]
            crate::set_corner_preference(
                window,
                Sonora::global(cx).settings.read(cx).window_rounding(),
            );
            if let Err(error) = raise(window) {
                log::warn!("pip: cannot keep the window on top: {error:#}");
            }
            #[cfg(target_os = "linux")]
            if needs_extension(window) {
                Toasts::post(
                    Toast::new(Outcome::Info, "toast-pip-extension")
                        .note("toast-pip-extension-note")
                        .visit("toast-pip-extension-get", EXTENSION_PAGE),
                    cx,
                );
            }
            cx.new(|cx| PipView::new(window, cx))
        },
    )?;
    Ok(())
}

/// Whether only the missing extension would keep the window on top, which is GNOME on Wayland.
// shortcut: an extension that is installed but switched off counts as there, since the Flatpak
// cannot ask GNOME Shell. Ask over D-Bus if the Flatpak ever gets to talk to it.
#[cfg(target_os = "linux")]
fn needs_extension(window: &gpui::Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let wayland = HasWindowHandle::window_handle(window)
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Wayland(_)));
    let gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktop| desktop.split(':').any(|name| name == "GNOME"));
    let installed = || {
        let shared = std::path::PathBuf::from("/usr/share");
        // The Flatpak has its own data folder, so the host's is spelled out.
        let own = std::env::home_dir().map(|home| home.join(".local/share"));
        own.into_iter().chain([shared]).any(|folder| {
            folder
                .join("gnome-shell/extensions")
                .join(EXTENSION)
                .exists()
        })
    };
    wayland && gnome && !installed()
}

/// Asks the X11 window manager to keep the window above the others. Does nothing on Wayland.
#[cfg(target_os = "linux")]
fn raise(window: &gpui::Window) -> Result<()> {
    use anyhow::Context as _;
    use raw_window_handle::{HasDisplayHandle as _, RawDisplayHandle, RawWindowHandle};
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask};
    use x11rb::xcb_ffi::XCBConnection;

    /// `_NET_WM_STATE_ADD`, and the source indication of a normal application.
    const ADD: u32 = 1;
    const APPLICATION: u32 = 1;

    let RawWindowHandle::Xcb(handle) =
        raw_window_handle::HasWindowHandle::window_handle(window)?.as_raw()
    else {
        return Ok(());
    };
    let RawDisplayHandle::Xcb(display) = window.display_handle()?.as_raw() else {
        return Ok(());
    };
    let raw = display
        .connection
        .context("cannot find the X11 connection")?;
    // SAFETY: GPUI owns the live connection, and `false` leaves closing it to GPUI.
    let connection = unsafe { XCBConnection::from_raw_xcb_connection(raw.as_ptr(), false) }
        .context("cannot borrow the X11 connection")?;
    let state = connection
        .intern_atom(false, b"_NET_WM_STATE")?
        .reply()?
        .atom;
    let above = connection
        .intern_atom(false, b"_NET_WM_STATE_ABOVE")?
        .reply()?
        .atom;
    let root = connection.get_geometry(handle.window.get())?.reply()?.root;
    connection
        .send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ClientMessageEvent::new(
                32,
                handle.window.get(),
                state,
                [ADD, above, 0, APPLICATION, 0],
            ),
        )?
        .check()?;
    connection.flush()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn raise(window: &gpui::Window) -> Result<()> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)?.as_raw() else {
        return Ok(());
    };
    let handle = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
    // SAFETY: the handle is the live window being built, and the call only changes its z-order.
    unsafe {
        SetWindowPos(
            handle,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn raise(_window: &gpui::Window) -> Result<()> {
    Ok(())
}
