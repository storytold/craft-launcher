//! Request winit's frame only on Wayland compositors without xdg-decoration.
//! Desktop/session names cannot identify the backend of an individual window.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// Capture the socket path before winit consumes an inherited WAYLAND_SOCKET.
/// Never consume, duplicate or query that inherited connection: there may be no
/// independently addressable display, or WAYLAND_DISPLAY may name another server.
pub fn display_socket() -> Option<PathBuf> {
    if std::env::var_os("WAYLAND_SOCKET").is_some_and(|s| !s.is_empty()) {
        return None;
    }
    let display = std::env::var_os("WAYLAND_DISPLAY").filter(|s| !s.is_empty())?;
    let path = PathBuf::from(display);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()).map(|dir| PathBuf::from(dir).join(path))
    }
}

pub fn needs_native_frame(cc: &eframe::CreationContext<'_>, socket: Option<PathBuf>) -> bool {
    let wayland = cc.display_handle().is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)));
    if !wayland {
        log::debug!("window controls: non-Wayland window, retaining custom controls");
        return false;
    }
    let server_decorations = socket.and_then(probe);
    log::debug!("window controls: Wayland xdg-decoration available: {server_decorations:?}");
    native_frame(wayland, server_decorations)
}

fn native_frame(wayland: bool, server_decorations: Option<bool>) -> bool {
    wayland && server_decorations == Some(false)
}

fn probe(socket: PathBuf) -> Option<bool> {
    // The compositor must not be able to block startup on a registry roundtrip.
    // On failure/timeout retain the app's own working title bar and buttons.
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("wayland-decorations".into())
        .spawn(move || {
            let _ = send.send(registry_has_decorations(socket));
        })
        .ok()?;
    receive.recv_timeout(Duration::from_millis(500)).ok().flatten()
}

fn registry_has_decorations(socket: PathBuf) -> Option<bool> {
    // A fresh connection leaves winit's event queue and inherited descriptors alone.
    let stream = UnixStream::connect(socket).ok()?;
    let connection = Connection::from_socket(stream).ok()?;
    let (globals, _queue) = registry_queue_init::<Registry>(&connection).ok()?;
    Some(globals.contents().with_list(|list| list.iter().any(|g| g.interface == "zxdg_decoration_manager_v1")))
}

struct Registry;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Registry {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_wayland_without_server_decorations_uses_the_frame() {
        for (wayland, server_decorations, expected) in [
            (false, None, false),
            (false, Some(false), false),
            (false, Some(true), false),
            (true, Some(true), false),
            (true, Some(false), true),
            (true, None, false),
        ] {
            assert_eq!(super::native_frame(wayland, server_decorations), expected);
        }
    }

    #[test]
    fn an_unavailable_display_keeps_the_custom_controls() {
        assert_eq!(super::probe("/nonexistent/artcraft-launcher-wayland-test".into()), None);
    }

    #[test]
    fn an_unresponsive_display_cannot_block_window_startup() -> std::io::Result<()> {
        let socket = std::env::temp_dir().join(format!("craft-launcher-decoration-timeout-{}", std::process::id()));
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        let start = std::time::Instant::now();
        assert_eq!(super::probe(socket.clone()), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        drop(listener);
        let _ = std::fs::remove_file(socket);
        Ok(())
    }
}
