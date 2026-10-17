//! Which desktop this machine has, and so how its screen is listed and captured: macOS, Hyprland
//! (Wayland, by wlroots' screencopy) or X11. Another Wayland desktop (GNOME, KDE) lets a program record
//! the screen only through its portal, which asks someone at the screen to allow it, so it's named and
//! refused rather than half-done. `SCREEN_BACKEND` (`macos`, `x11`, `hyprland`) picks one, for tests.

use std::path::Path;

use anyhow::{bail, Result};

use crate::ffmpeg::Pipeline;
use crate::geom::Rect;
use crate::hypr::{self, Session};
use crate::screens::{place, Found, Resolved, Window};
use crate::{ffmpeg, mac, sck, x11};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Mac,
    /// Its X display (`:0`).
    X11(String),
    Hypr(Session),
    /// Why this machine's screen can't be shown.
    Unsupported(String),
}

impl Backend {
    pub fn here() -> Backend {
        match std::env::var("SCREEN_BACKEND").as_deref() {
            Ok("macos") => return Backend::Mac,
            Ok("x11") => return Backend::X11(x11::display().unwrap_or_else(|| ":0".into())),
            Ok("hyprland") => return Backend::Hypr(Session::find().unwrap_or_default()),
            _ => {}
        }
        if cfg!(target_os = "macos") {
            return Backend::Mac;
        }
        if let Some(session) = Session::find() {
            return Backend::Hypr(session);
        }
        let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
            || hypr::wayland_socket(&hypr::runtime_dir()).is_some();
        if wayland {
            let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|d| !d.is_empty());
            return Backend::Unsupported(wayland_why(desktop.as_deref()));
        }
        match x11::display() {
            Some(display) => Backend::X11(display),
            None => Backend::Unsupported(
                "this machine has no desktop session to show: no X display, and no Wayland compositor".into(),
            ),
        }
    }

    /// What its coordinates are in.
    pub fn units(&self) -> &'static str {
        match self {
            Backend::Mac => "points",
            Backend::Hypr(_) => "layout pixels",
            _ => "pixels",
        }
    }

    /// Whether it may capture now; if not, why and what to do.
    pub fn ready(&self) -> Result<()> {
        match self {
            Backend::Mac => mac::check(),
            Backend::Unsupported(why) => bail!("{why}"),
            _ => Ok(()),
        }
    }

    pub fn list(&self) -> Result<Found> {
        match self {
            Backend::Mac => mac::list(),
            Backend::X11(display) => x11::list(display),
            Backend::Hypr(session) => session.list(),
            Backend::Unsupported(why) => bail!("{why}"),
        }
    }

    /// What's shown now, for a pane following `last`'s window: all of it, except on X11, where only that
    /// window is asked again (one xwininfo, not one a window). Its displays are the last ones there.
    pub fn refresh(&self, last: &Resolved, displays: &[crate::screens::Display]) -> Result<Found> {
        let (Backend::X11(display), Some(w)) = (self, &last.window) else { return self.list() };
        let mut found = Found { displays: displays.to_vec(), ..Found::default() };
        if let Some(bounds) = x11::locate(display, &w.id) {
            if let Some(on) = place(displays, &bounds) {
                found.windows.push(Window { bounds, display: on, ..w.clone() });
            }
        }
        Ok(found)
    }

    /// The live pane's capture: MJPEG on stdout, from one tool or two.
    pub fn live(&self, r: &Resolved, fps: u32, width: u32, quality: u32) -> Result<Pipeline> {
        match self {
            Backend::Mac => ffmpeg::live_mac(r, fps, width, quality),
            Backend::X11(display) => Ok(ffmpeg::live_x11(r, display, fps, width, quality).into()),
            Backend::Hypr(session) => Ok(ffmpeg::live_hypr(r, session, fps, width).into()),
            Backend::Unsupported(why) => bail!("{why}"),
        }
    }

    /// What a capture of `r` depends on, so a pane restarts its capture only when that changes: on a Mac,
    /// a window's own (its id, and the part of it, from its top-left: not where it is); else the display
    /// and the place on it.
    pub fn place(&self, r: &Resolved) -> (String, Rect) {
        match (self, &r.window) {
            (Backend::Mac, Some(w)) => (format!("window {}", w.id), sck::local(r)),
            _ => (r.display.id.clone(), r.area),
        }
    }

    /// One JPEG of it into `out`, and its size.
    pub fn still(
        &self,
        r: &Resolved,
        width: u32,
        quality: u32,
        out: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(u16, u16)> {
        match self {
            Backend::Mac => ffmpeg::still_mac(r, width, quality, out, cancelled),
            Backend::X11(display) => ffmpeg::still_x11(r, display, width, quality, out, cancelled),
            Backend::Hypr(session) => ffmpeg::still_hypr(r, session, width, quality, out, cancelled),
            Backend::Unsupported(why) => bail!("{why}"),
        }
    }
}

/// Why a Wayland desktop other than Hyprland isn't shown.
fn wayland_why(desktop: Option<&str>) -> String {
    let desktop = desktop.map_or("This Wayland desktop".to_string(), |d| format!("{d} on Wayland"));
    format!(
        "{desktop} lets a program record the screen only through its desktop portal (xdg-desktop-portal's \
         ScreenCast, over PipeWire), which asks someone at the screen to allow it: once, if the desktop keeps a \
         restore token, else every time. screen doesn't use the portal yet. Hyprland (wlroots' screencopy) and \
         X11 sessions work with no one at the screen"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_wayland_desktop_says_why_not() {
        let why = wayland_why(Some("GNOME"));
        assert!(why.starts_with("GNOME on Wayland lets a program record the screen only through its desktop portal"));
        assert!(why.contains("Hyprland (wlroots' screencopy) and X11 sessions work"));
        assert!(Backend::Unsupported(why.clone()).ready().unwrap_err().to_string() == why);
        assert_eq!(Backend::Mac.units(), "points");
    }
}
