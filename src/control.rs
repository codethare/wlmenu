//! Single-instance control: a second launch dismisses the running panel instead
//! of stacking another layer surface on top of it.
//!
//! Wayland has no "only one of me" primitive, so the first instance binds a
//! socket under `$XDG_RUNTIME_DIR` and the next one connects to it. Everything
//! here is Wayland-free (the listener is handed back to the caller to wire into
//! the event loop), so it is unit-testable without a compositor.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub(crate) enum Claim {
    /// This process is the only instance; the listener is the dismissal channel.
    Owner(UnixListener),
    /// Another instance was running and has been told to close.
    Dismissed,
    /// No control channel (no usable `$XDG_RUNTIME_DIR`, or the bind failed):
    /// fall back to today's behavior — no single-instance guarantee, no error.
    Disabled,
}

/// Claim the session's single-instance slot.
pub(crate) fn claim() -> Claim {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    match socket_path_from(std::env::var_os("XDG_RUNTIME_DIR"), &display) {
        Some(path) => claim_at(&path),
        None => Claim::Disabled,
    }
}

/// Per-session path: `$WAYLAND_DISPLAY` in the name keeps a nested compositor
/// (which shares `$XDG_RUNTIME_DIR`) from dismissing the outer session's menu.
fn socket_path_from(dir: Option<OsString>, display: &str) -> Option<PathBuf> {
    let dir = dir.filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join(format!("wlmenu-{}.sock", display_token(display))))
}

/// `$WAYLAND_DISPLAY` is usually a bare socket name, but some setups pass an
/// absolute path; keep the basename and reduce it to a filename-safe token.
fn display_token(display: &str) -> String {
    let base = display.rsplit('/').next().unwrap_or("");
    let token: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        "default".to_string()
    } else {
        token
    }
}

fn claim_at(path: &Path) -> Claim {
    // A live instance answers: ask it to close, then leave the stage. Nothing
    // has been created yet (no font, no Wayland connection), so a repeat launch
    // costs about a millisecond.
    if dismiss(path) {
        return Claim::Dismissed;
    }
    // Nobody is listening: either nothing is there, or an instance was killed
    // and left its socket file behind.
    let _ = std::fs::remove_file(path);
    match UnixListener::bind(path) {
        Ok(listener) => {
            // The event loop must never block in accept().
            let _ = listener.set_nonblocking(true);
            Claim::Owner(listener)
        }
        // ponytail: a racing start that bound first wins; assume it is live and
        // let it dismiss us. Only two launches in the same microsecond hit this,
        // and the loser degrades to "no single instance" rather than hanging.
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if dismiss(path) {
                Claim::Dismissed
            } else {
                Claim::Disabled
            }
        }
        Err(_) => Claim::Disabled,
    }
}

/// Tell a listening instance to close; true when it accepted the connection.
fn dismiss(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            let _ = stream.write_all(b"x");
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Unique per test so parallel tests never share a socket file.
    fn temp_sock(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("wlmenu-ctl-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn first_claim_wins_and_the_second_is_dismissed() {
        let path = temp_sock("first");
        let owner = claim_at(&path);
        assert!(
            matches!(owner, Claim::Owner(_)),
            "first claim owns the slot"
        );
        assert!(
            matches!(claim_at(&path), Claim::Dismissed),
            "a second claim must dismiss the running instance"
        );
        drop(owner);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dismissal_reaches_the_owners_listener() {
        let path = temp_sock("reach");
        let Claim::Owner(listener) = claim_at(&path) else {
            panic!("first claim must own the slot");
        };
        assert!(matches!(claim_at(&path), Claim::Dismissed));
        let (mut stream, _) = listener
            .accept()
            .expect("the dismissal must be queued for accept");
        let mut byte = [0u8; 1];
        assert_eq!(stream.read(&mut byte).unwrap(), 1, "dismissal byte arrives");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_socket_file_left_by_a_killed_instance_is_reclaimed() {
        let path = temp_sock("stale");
        drop(claim_at(&path)); // listener gone, socket file still on disk
        assert!(path.exists(), "unix sockets outlive their listener");
        assert!(
            matches!(claim_at(&path), Claim::Owner(_)),
            "a stale socket file must be reclaimed, not block startup"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn no_runtime_dir_disables_the_slot_and_odd_displays_stay_filename_safe() {
        assert!(socket_path_from(None, "wayland-1").is_none());
        assert!(socket_path_from(Some(OsString::new()), "wayland-1").is_none());
        assert_eq!(
            socket_path_from(Some(OsString::from("/run/user/1000")), "/abs/wayland-1"),
            Some(PathBuf::from("/run/user/1000/wlmenu-wayland-1.sock"))
        );
        let unnamed = socket_path_from(Some(OsString::from("/tmp")), "").unwrap();
        assert!(unnamed.ends_with("wlmenu-default.sock"));
    }
}
