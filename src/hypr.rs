//! Hyprland (Wayland): the displays are `hyprctl monitors -j`, and the windows `hyprctl clients -j`,
//! those on a workspace their monitor shows, most recently focused first. Both are in layout pixels,
//! which is what wf-recorder's and grim's `-g` take; a capture is in the monitor's own pixels (its
//! layout pixels times its scale). wlroots' screencopy, which both use, needs no one at the screen to
//! allow it, unlike the desktop portal.
//!
//! The node may run outside the desktop's session (a systemd user unit), without
//! `WAYLAND_DISPLAY` or `HYPRLAND_INSTANCE_SIGNATURE`: both are found in the runtime folder then.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::geom::Rect;
use crate::proc;
use crate::screens::{place, Display, Found, Window};

/// What a Hyprland session's tools need in their environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub env: Vec<(String, String)>,
}

impl Session {
    /// Hyprland's session, if this machine has one running: from the environment, else the runtime
    /// folder (`hypr/<instance>` with its socket, and `wayland-N`).
    pub fn find() -> Option<Session> {
        let runtime = runtime_dir();
        let instance = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| newest_instance(&runtime.join("hypr")))?;
        let wayland =
            std::env::var("WAYLAND_DISPLAY").ok().filter(|s| !s.is_empty()).or_else(|| wayland_socket(&runtime));
        let mut env = vec![
            ("XDG_RUNTIME_DIR".to_string(), runtime.to_string_lossy().into_owned()),
            ("HYPRLAND_INSTANCE_SIGNATURE".to_string(), instance),
        ];
        if let Some(w) = wayland {
            env.push(("WAYLAND_DISPLAY".to_string(), w));
        }
        Some(Session { env })
    }

    /// `name` with the session's environment.
    pub fn command(&self, name: &str) -> Command {
        let mut cmd = Command::new(name);
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }

    fn hyprctl(&self, what: &str) -> Result<Value> {
        let ran = proc::run(self.command("hyprctl").args([what, "-j"]), Duration::from_secs(10), &|| false)?;
        if !ran.status.success() {
            bail!("hyprctl {what} failed: {}", ran.why());
        }
        serde_json::from_slice(&ran.stdout).with_context(|| format!("hyprctl {what} -j didn't give JSON"))
    }

    /// The displays, and the windows front to back.
    pub fn list(&self) -> Result<Found> {
        let monitors = self.hyprctl("monitors")?;
        let clients = self.hyprctl("clients")?;
        Ok(found(&monitors, &clients))
    }
}

/// The user's runtime folder: `$XDG_RUNTIME_DIR`, else /run/user/<uid>.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        // SAFETY: getuid only reads this process's user id
        None => PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
    }
}

/// The Hyprland instance most recently started, of those under `hypr` that have a socket.
fn newest_instance(hypr: &Path) -> Option<String> {
    std::fs::read_dir(hypr)
        .ok()?
        .flatten()
        .filter(|e| e.path().join(".socket.sock").exists())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.file_name().to_string_lossy().into_owned())))
        .max()
        .map(|(_, name)| name)
}

/// A Wayland compositor's socket in the runtime folder: `wayland-1`, not its `.lock`.
pub fn wayland_socket(runtime: &Path) -> Option<String> {
    let mut found: Vec<String> = std::fs::read_dir(runtime)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// What `hyprctl monitors -j` and `hyprctl clients -j` say, as displays and windows.
pub fn found(monitors: &Value, clients: &Value) -> Found {
    let n = |v: &Value| v.as_f64().unwrap_or(0.0);
    let mut displays = Vec::new();
    let mut shows: Vec<(String, Vec<i64>)> = Vec::new();
    for m in monitors.as_array().into_iter().flatten().filter(|m| m["disabled"] != true) {
        let id = m["id"].as_i64().unwrap_or(displays.len() as i64).to_string();
        let (w, h) = (n(&m["width"]), n(&m["height"]));
        let scale = Some(n(&m["scale"])).filter(|s| *s > 0.0).unwrap_or(1.0);
        // a monitor on its side (transform 1, 3, 5, 7) is taller than its mode
        let turned = m["transform"].as_i64().is_some_and(|t| t % 2 == 1);
        let (w, h) = if turned { (h, w) } else { (w, h) };
        let bounds = Rect::new(n(&m["x"]), n(&m["y"]), (w / scale).round(), (h / scale).round());
        let output = m["name"].as_str().unwrap_or_default().to_string();
        let name = match m["description"].as_str().filter(|d| !d.is_empty()) {
            Some(d) => format!("{output} ({d})"),
            None => output.clone(),
        };
        let workspaces =
            ["activeWorkspace", "specialWorkspace"].iter().filter_map(|k| m[*k]["id"].as_i64()).filter(|i| *i != 0);
        shows.push((id.clone(), workspaces.collect()));
        displays.push(Display { id, name, bounds, pixels: (w as u32, h as u32), main: m["focused"] == true, output });
    }
    if !displays.is_empty() && !displays.iter().any(|d| d.main) {
        displays[0].main = true;
    }
    let mut windows: Vec<(i64, Window)> = Vec::new();
    for c in clients.as_array().into_iter().flatten() {
        if c["mapped"] == false || c["hidden"] == true {
            continue;
        }
        let at = |i: usize| n(&c["at"][i]);
        let size = |i: usize| n(&c["size"][i]);
        let bounds = Rect::new(at(0), at(1), size(0), size(1));
        let monitor = c["monitor"].as_i64().map(|m| m.to_string()).unwrap_or_default();
        let workspace = c["workspace"]["id"].as_i64().unwrap_or(0);
        let on_view = c["pinned"] == true || shows.iter().any(|(m, ws)| *m == monitor && ws.contains(&workspace));
        if !on_view || bounds.width < 2.0 || bounds.height < 2.0 {
            continue; // on a workspace no monitor shows now
        }
        let Some(display) = place(&displays, &bounds) else { continue };
        let text = |k: &str| c[k].as_str().unwrap_or_default().to_string();
        let order = c["focusHistoryID"].as_i64().unwrap_or(i64::MAX);
        windows
            .push((order, Window { id: text("address"), app: text("class"), title: text("title"), display, bounds }));
    }
    windows.sort_by_key(|(order, _)| *order);
    Found { displays, windows: windows.into_iter().map(|(_, w)| w).collect(), notes: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// nixos's, as rtx-render saw them (#extensions "Cam" 11576 [12331]), with Blender added and one
    /// window on a workspace that isn't shown.
    pub fn nixos() -> (Value, Value) {
        let monitors = json!([{"id": 0, "name": "eDP-1", "description": "AU Optronics 0x97A2", "width": 3840, "height": 2160,
                               "x": 0, "y": 0, "scale": 1.6, "transform": 0, "focused": true, "disabled": false,
                               "activeWorkspace": {"id": 1, "name": "1"}, "specialWorkspace": {"id": 0, "name": ""}}]);
        let clients = json!([
            {"address": "0x55d0a1", "class": "com.mitchellh.ghostty", "title": "~", "at": [1233, 24], "size": [1143, 1302],
             "monitor": 0, "workspace": {"id": 1, "name": "1"}, "mapped": true, "hidden": false, "focusHistoryID": 1},
            {"address": "0x55d0a2", "class": "com.mitchellh.ghostty", "title": "nvim", "at": [74, 24], "size": [1143, 1302],
             "monitor": 0, "workspace": {"id": 1, "name": "1"}, "mapped": true, "hidden": false, "focusHistoryID": 0},
            {"address": "0x55d0a3", "class": "blender", "title": "Blender [loft.blend]", "at": [0, 0], "size": [2400, 1350],
             "monitor": 0, "workspace": {"id": 2, "name": "2"}, "mapped": true, "hidden": false, "focusHistoryID": 2}
        ]);
        (monitors, clients)
    }

    #[test]
    fn monitors_are_in_layout_pixels_and_captured_in_their_own() {
        let (monitors, clients) = nixos();
        let found = found(&monitors, &clients);
        let d = &found.displays[0];
        assert_eq!((d.id.as_str(), d.name.as_str(), d.output.as_str()), ("0", "eDP-1 (AU Optronics 0x97A2)", "eDP-1"));
        assert_eq!(d.bounds, Rect::new(0.0, 0.0, 2400.0, 1350.0));
        assert_eq!(d.pixels, (3840, 2160));
        assert_eq!(d.scale(), 1.6);
        assert!(d.main);
        // on its side
        let turned = json!([{"id": 1, "name": "DP-2", "width": 2560, "height": 1440, "x": 2400, "y": 0, "scale": 1.0, "transform": 1}]);
        let d = &super::found(&turned, &json!([])).displays[0];
        assert_eq!((d.bounds.width, d.bounds.height, d.pixels), (1440.0, 2560.0, (1440, 2560)));
        assert!(d.main, "the only one is the main one");
    }

    #[test]
    fn windows_are_those_shown_most_recently_focused_first() {
        let (monitors, clients) = nixos();
        let found = found(&monitors, &clients);
        let ids: Vec<&str> = found.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, ["0x55d0a2", "0x55d0a1"], "Blender's workspace isn't shown");
        assert_eq!(found.windows[0].bounds, Rect::new(74.0, 24.0, 1143.0, 1302.0));
        assert_eq!((found.windows[0].app.as_str(), found.windows[0].display.as_str()), ("com.mitchellh.ghostty", "0"));
    }

    #[test]
    fn the_session_is_found_in_the_runtime_folder() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["wayland-1", "wayland-1.lock", "wayland-0.lock"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        assert_eq!(wayland_socket(dir.path()).as_deref(), Some("wayland-1"));
        let hypr = dir.path().join("hypr");
        std::fs::create_dir_all(hypr.join("old_1")).unwrap();
        std::fs::create_dir_all(hypr.join("abc_123")).unwrap();
        std::fs::write(hypr.join("abc_123/.socket.sock"), "").unwrap();
        assert_eq!(newest_instance(&hypr).as_deref(), Some("abc_123"), "the one with a socket");
        assert_eq!(newest_instance(&dir.path().join("none")), None);
    }
}
