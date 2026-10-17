//! X11: the displays are xrandr's monitors (else the root window, as one), and the windows the window
//! manager's clients (xprop's `_NET_CLIENT_LIST_STACKING`, bottom to top), each with its title, class
//! and place from `xwininfo -root -tree`, less those minimised or that are a dock or the desktop.
//! A window followed is found again with `xwininfo -id`. Everything is in the screen's pixels, which
//! is also what x11grab captures.

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Result};

use crate::geom::Rect;
use crate::proc;
use crate::screens::{place, Display, Found, Window};

/// This machine's X display: `$DISPLAY`, else the first server with a socket in /tmp/.X11-unix.
pub fn display() -> Option<String> {
    if let Some(d) = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty()) {
        return Some(d);
    }
    let mut servers: Vec<u32> = std::fs::read_dir("/tmp/.X11-unix")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_prefix('X')?.parse().ok())
        .collect();
    servers.sort_unstable();
    servers.first().map(|n| format!(":{n}"))
}

fn tool(name: &str, display: &str, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(name);
    cmd.args(args).env("DISPLAY", display);
    let ran = proc::run(&mut cmd, Duration::from_secs(10), &|| false)?;
    if !ran.status.success() {
        bail!("{name} {} failed on {display}: {}", args.join(" "), ran.why());
    }
    Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
}

/// The displays, and the windows front to back.
pub fn list(display: &str) -> Result<Found> {
    let mut found = Found { displays: displays(display)?, ..Found::default() };
    let tree = tree(&tool("xwininfo", display, &["-root", "-tree"])?);
    let clients = match tool("xprop", display, &["-root", "_NET_CLIENT_LIST_STACKING", "_NET_CLIENT_LIST"]) {
        Ok(out) => clients(&out),
        Err(e) => {
            found.notes.push(format!("No window manager's list of windows: {e:#}"));
            Vec::new()
        }
    };
    for id in clients.iter().rev() {
        let Some((title, app, bounds)) = tree.get(id) else { continue };
        let state = tool("xprop", display, &["-id", id, "_NET_WM_STATE", "_NET_WM_WINDOW_TYPE"]).unwrap_or_default();
        if !shown(&state) || bounds.width < 2.0 || bounds.height < 2.0 {
            continue;
        }
        let Some(on) = place(&found.displays, bounds) else { continue };
        found.windows.push(Window {
            id: id.clone(),
            app: app.clone(),
            title: title.clone(),
            display: on,
            bounds: *bounds,
        });
    }
    if found.windows.is_empty() && !clients.is_empty() {
        found.notes.push("None of the window manager's windows is on a display now.".into());
    }
    Ok(found)
}

/// One window again, wherever it is now, or none if it's gone or not shown.
pub fn locate(display: &str, id: &str) -> Option<Rect> {
    info(&tool("xwininfo", display, &["-id", id]).ok()?)
}

/// xrandr's monitors, else the whole root window.
fn displays(display: &str) -> Result<Vec<Display>> {
    if let Ok(out) = tool("xrandr", display, &["--listactivemonitors"]) {
        let found = monitors(&out);
        if !found.is_empty() {
            return Ok(found);
        }
    }
    let root = tool("xwininfo", display, &["-root"])?;
    let Some(bounds) = info(&root) else { bail!("xwininfo didn't say how big {display}'s screen is") };
    let pixels = (bounds.width as u32, bounds.height as u32);
    Ok(vec![Display {
        id: "0".into(),
        name: format!("Screen {display}"),
        bounds,
        pixels,
        main: true,
        output: String::new(),
    }])
}

/// `xrandr --listactivemonitors`: ` 0: +*DP-1 2560/597x1440/336+0+0  DP-1`, `*` the primary.
pub fn monitors(out: &str) -> Vec<Display> {
    let mut found = Vec::new();
    for line in out.lines() {
        let Some((n, rest)) = line.trim().split_once(": ") else { continue };
        let Ok(n) = n.trim().parse::<u32>() else { continue };
        let mut words = rest.split_whitespace();
        let (Some(flags), Some(geometry)) = (words.next(), words.next()) else { continue };
        let name = flags.trim_start_matches(['+', '*']).to_string();
        let Some(bounds) = geometry_of(geometry) else { continue };
        let pixels = (bounds.width as u32, bounds.height as u32);
        found.push(Display {
            id: n.to_string(),
            name,
            bounds,
            pixels,
            main: flags.contains('*'),
            output: String::new(),
        });
    }
    if !found.is_empty() && !found.iter().any(|d| d.main) {
        found[0].main = true;
    }
    found
}

/// xrandr's `2560/597x1440/336+0+0` (its millimetres after each slash), or `800x600+10+20`.
fn geometry_of(g: &str) -> Option<Rect> {
    let (size, place) = g.split_once('+')?;
    let (x, y) = place.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let n = |s: &str| s.split('/').next()?.parse::<f64>().ok();
    Some(Rect::new(n(x)?, n(y)?, n(w)?, n(h)?))
}

/// The client windows xprop lists, bottom to top: the stacking order if the window manager keeps one,
/// else its plain list. Ids as `0x1e00007`.
pub fn clients(out: &str) -> Vec<String> {
    let ids = |prop: &str| -> Option<Vec<String>> {
        let line = out.lines().find(|l| l.starts_with(&format!("{prop}(")))?;
        let (_, list) = line.split_once('#')?;
        Some(list.split(',').filter_map(|id| hex(id.trim())).collect())
    };
    ids("_NET_CLIENT_LIST_STACKING").or_else(|| ids("_NET_CLIENT_LIST")).unwrap_or_default()
}

/// An X window id, the same however it's written (`0x01e00007` and `0x1e00007`).
fn hex(id: &str) -> Option<String> {
    let n = u64::from_str_radix(id.strip_prefix("0x")?, 16).ok()?;
    Some(format!("{n:#x}"))
}

/// `xwininfo -root -tree`'s windows by id: title, class and place on the screen. A line is
/// `0x2400003 "Blender": ("blender" "Blender")  1920x1080+0+0  +0+0`, its absolute place last.
pub fn tree(out: &str) -> HashMap<String, (String, String, Rect)> {
    let mut found = HashMap::new();
    for line in out.lines() {
        let line = line.trim();
        let Some((id, rest)) = line.split_once(' ') else { continue };
        let Some(id) = hex(id) else { continue };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let [.., size, absolute] = words[..] else { continue };
        let Some(cut) = rest.rfind(size) else { continue };
        let named = rest[..cut].trim_end();
        let (Some(relative), Some(at)) = (geometry_of(size), absolute.strip_prefix('+')) else { continue };
        let Some((x, y)) = at.split_once('+').and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)))
        else {
            continue;
        };
        let (title, class) = match named.rfind("\": (") {
            Some(end) if named.starts_with('"') => (&named[1..end], &named[end + 4..]),
            _ => ("", named.split_once(": (").map_or("", |(_, c)| c)),
        };
        // the class's second word, its name for people: ("blender" "Blender")
        let app = class.trim_end_matches(')').split('"').rfind(|w| !w.trim().is_empty()).unwrap_or("");
        found.insert(id, (title.to_string(), app.to_string(), Rect::new(x, y, relative.width, relative.height)));
    }
    found
}

/// Whether xprop's state and type for a window say it's one to show: not minimised, not a dock or the
/// desktop.
fn shown(props: &str) -> bool {
    !["_NET_WM_STATE_HIDDEN", "_NET_WM_WINDOW_TYPE_DOCK", "_NET_WM_WINDOW_TYPE_DESKTOP"]
        .iter()
        .any(|p| props.contains(p))
}

/// `xwininfo -id`'s (or `-root`'s) place for a window that's shown: its absolute corner and size.
pub fn info(out: &str) -> Option<Rect> {
    let field = |name: &str| -> Option<&str> {
        out.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim_start_matches(':').trim()))
    };
    if field("Map State").is_some_and(|s| s != "IsViewable") {
        return None;
    }
    let n = |name: &str| field(name)?.parse::<f64>().ok();
    Some(Rect::new(n("Absolute upper-left X")?, n("Absolute upper-left Y")?, n("Width")?, n("Height")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrandrs_monitors_are_the_displays() {
        let out =
            "Monitors: 2\n 0: +*DP-1 2560/597x1440/336+0+0  DP-1\n 1: +HDMI-1 1920/527x1080/296+2560+180  HDMI-1\n";
        let d = monitors(out);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].id.as_str(), d[0].name.as_str(), d[0].main), ("0", "DP-1", true));
        assert_eq!(d[1].bounds, Rect::new(2560.0, 180.0, 1920.0, 1080.0));
        assert_eq!(d[1].pixels, (1920, 1080));
        // none marked primary: the first
        assert!(monitors(" 0: +VNC-0 1280/339x800/212+0+0  VNC-0\n")[0].main);
        assert!(monitors("Monitors: 0\n").is_empty());
    }

    #[test]
    fn the_client_list_is_bottom_to_top() {
        let out = "_NET_CLIENT_LIST_STACKING(WINDOW): window id # 0x1e00007, 0x02400003\n_NET_CLIENT_LIST(WINDOW): window id # 0x2400003, 0x1e00007\n";
        assert_eq!(clients(out), ["0x1e00007", "0x2400003"]);
        assert_eq!(
            clients("_NET_CLIENT_LIST_STACKING:  not found.\n_NET_CLIENT_LIST(WINDOW): window id # 0x5\n"),
            ["0x5"]
        );
        assert!(clients("_NET_CLIENT_LIST_STACKING:  not found.\n_NET_CLIENT_LIST:  not found.\n").is_empty());
    }

    #[test]
    fn the_tree_has_each_windows_title_class_and_absolute_place() {
        let out = r#"
xwininfo: Window id: 0x6c5 (the root window) (has no name)

  Root window id: 0x6c5 (the root window) (has no name)
  Parent window id: 0x0 (none)
     3 children:
     0x1a00003 (has no name): ()  1924x1104+600+100  +600+100
        1 child:
        0x2400003 "Blender* [~/loft.blend]": ("blender" "Blender")  1920x1080+2+22  +602+122
     0x1e00007 "a \"quoted\": (title)": ("xterm" "XTerm")  800x600+-10+0  +-10+0
     0x1c00001 (has no name): ()  1x1+-1+-1  +-1+-1
"#;
        let t = tree(out);
        assert_eq!(
            t["0x2400003"],
            ("Blender* [~/loft.blend]".into(), "Blender".into(), Rect::new(602.0, 122.0, 1920.0, 1080.0))
        );
        assert_eq!(t["0x1e00007"].0, r#"a \"quoted\": (title)"#);
        assert_eq!(t["0x1e00007"].2, Rect::new(-10.0, 0.0, 800.0, 600.0));
        assert_eq!((t["0x1a00003"].0.as_str(), t["0x1a00003"].1.as_str()), ("", ""));
        assert!(!t.contains_key("0x6c5"), "the root's own lines aren't windows");
    }

    #[test]
    fn a_window_is_found_again_only_while_its_shown() {
        let out = "xwininfo: Window id: 0x2400003 \"Blender\"\n\n  Absolute upper-left X:  640\n  Absolute upper-left Y:  80\n  Relative upper-left X:  2\n  Relative upper-left Y:  22\n  Width: 1280\n  Height: 720\n  Depth: 24\n  Map State: IsViewable\n";
        assert_eq!(info(out), Some(Rect::new(640.0, 80.0, 1280.0, 720.0)));
        assert_eq!(info(&out.replace("IsViewable", "IsUnMapped")), None);
        assert!(shown("_NET_WM_STATE(ATOM) = _NET_WM_STATE_MAXIMIZED_VERT\n_NET_WM_WINDOW_TYPE(ATOM) = _NET_WM_WINDOW_TYPE_NORMAL\n"));
        assert!(!shown("_NET_WM_STATE(ATOM) = _NET_WM_STATE_HIDDEN\n"));
        assert!(!shown("_NET_WM_WINDOW_TYPE(ATOM) = _NET_WM_WINDOW_TYPE_DOCK\n"));
    }
}
