//! macOS: the displays (CGGetActiveDisplayList, each with the CoreGraphics number ScreenCaptureKit knows
//! it by), the windows on them (CGWindowList, front to back, in points), and whether the node may
//! record the screen (Screen Recording, under Privacy & Security). Without it, CGWindowList leaves out
//! other apps' titles and a capture shows only the desktop, so it's checked before anything is shown.
//!
//! `SCREEN_FAKE_LIST` names a JSON file to read instead, on any platform: for tests.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::geom::Rect;
use crate::screens::{place, Display, Found, Window};

pub const FAKE_ENV: &str = "SCREEN_FAKE_LIST";

/// The displays and windows, as CoreGraphics (or the fake list) has them.
pub fn list() -> Result<Found> {
    let mut found = match std::env::var_os(FAKE_ENV) {
        Some(path) => fake(&std::fs::read_to_string(&path).with_context(|| format!("reading {path:?}"))?)?,
        None => real()?,
    };
    if !allowed() {
        found.notes.push(refused());
    }
    Ok(found)
}

/// Whether macOS lets this process (its node) record the screen. Asking doesn't prompt.
pub fn allowed() -> bool {
    if std::env::var_os(FAKE_ENV).is_some() {
        return std::env::var("SCREEN_FAKE_ALLOWED").map_or(true, |a| a != "0");
    }
    preflight()
}

/// Say no to a capture macOS won't allow, and ask macOS for it, once a run: the first time ever, it
/// shows its prompt on the Mac.
pub fn check() -> Result<()> {
    if allowed() {
        return Ok(());
    }
    static ASKED: AtomicBool = AtomicBool::new(false);
    if !ASKED.swap(true, Ordering::SeqCst) && std::env::var_os(FAKE_ENV).is_none() {
        request();
    }
    Err(anyhow!("{}", refused()))
}

/// Where to allow it, and for what.
fn refused() -> String {
    let (name, path) = responsible().unwrap_or_else(|| ("pointman-node".into(), String::new()));
    let add = if path.is_empty() { String::new() } else { format!(" (+ adds it from {path} if it isn't listed)") };
    format!(
        "macOS hasn't allowed the node to record the screen, so it shows no window titles and no picture. On this \
         Mac: System Settings, Privacy & Security, Screen & System Audio Recording, turn on {name}{add}, then restart \
         the node (pointman restart)"
    )
}

/// What macOS asks about: the node that started this extension, by its name and path.
#[cfg(target_os = "macos")]
pub(crate) fn responsible() -> Option<(String, String)> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: proc_pidpath writes at most the buffer's size into it, and says how much it wrote
    let n = unsafe { libc::proc_pidpath(libc::getppid(), buf.as_mut_ptr().cast(), buf.len() as u32) };
    let path = std::str::from_utf8(buf.get(..usize::try_from(n).ok()?)?).ok()?;
    let name = path.rsplit('/').next().filter(|n| !n.is_empty())?;
    Some((name.to_string(), path.to_string()))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn responsible() -> Option<(String, String)> {
    None
}

/// The fake list: `{"displays": [{name, x, y, width, height, pixel_width, pixel_height, main, native}],
/// "windows": [{id, app, title, x, y, width, height}]}`, windows front to back. `native` is the display's
/// CoreGraphics number (its place in the list, from 1, if not given).
pub fn fake(json: &str) -> Result<Found> {
    let v: Value = serde_json::from_str(json).context("the fake list isn't JSON")?;
    let n = |v: &Value, k: &str| v[k].as_f64().unwrap_or(0.0);
    let displays: Vec<Display> = v["displays"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, d)| Display {
            id: i.to_string(),
            name: d["name"].as_str().unwrap_or("Display").to_string(),
            bounds: Rect::new(n(d, "x"), n(d, "y"), n(d, "width"), n(d, "height")),
            pixels: (n(d, "pixel_width") as u32, n(d, "pixel_height") as u32),
            main: d["main"].as_bool().unwrap_or(i == 0),
            output: d["native"].as_u64().unwrap_or(i as u64 + 1).to_string(),
        })
        .collect();
    let windows = v["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let bounds = Rect::new(n(w, "x"), n(w, "y"), n(w, "width"), n(w, "height"));
            Some(Window {
                id: match &w["id"] {
                    Value::String(s) => s.clone(),
                    id => id.to_string(),
                },
                app: w["app"].as_str().unwrap_or_default().to_string(),
                title: w["title"].as_str().unwrap_or_default().to_string(),
                display: place(&displays, &bounds)?,
                bounds,
            })
        })
        .collect();
    Ok(Found { displays, windows, notes: Vec::new() })
}

/// The smallest a window is to be one someone means: not a tooltip, a menu extra's or an app's
/// hidden helper.
#[cfg(target_os = "macos")]
const SMALLEST: f64 = 40.0;

#[cfg(target_os = "macos")]
fn real() -> Result<Found> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::display::CGDisplay;
    use core_graphics::window::{
        copy_window_info, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer,
        kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName, kCGWindowNumber,
        kCGWindowOwnerName,
    };

    let ids = CGDisplay::active_displays().map_err(|e| anyhow!("macOS didn't list its displays (CGError {e})"))?;
    let mut displays = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let d = CGDisplay::new(*id);
        let b = d.bounds();
        let bounds = Rect::new(b.origin.x, b.origin.y, b.size.width, b.size.height);
        let (w, h) = d
            .display_mode()
            .map(|m| (m.pixel_width() as u32, m.pixel_height() as u32))
            .unwrap_or((bounds.width as u32, bounds.height as u32));
        // a mode's pixels are the panel's way up; a display turned on its side is captured as it's seen
        let pixels = if matches!(d.rotation().round() as i64, 90 | 270) { (h, w) } else { (w, h) };
        let name = if d.is_builtin() { "Built-in display".to_string() } else { format!("Display {}", i + 1) };
        // ScreenCaptureKit knows it by its CoreGraphics number
        let output = id.to_string();
        displays.push(Display { id: i.to_string(), name, bounds, pixels, main: d.is_main(), output });
    }

    let mut windows = Vec::new();
    let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    if let Some(list) = copy_window_info(options, kCGNullWindowID) {
        for item in list.iter() {
            // SAFETY: CGWindowListCopyWindowInfo's entries are CFDictionaries, which the array keeps alive
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_get_rule(*item as CFDictionaryRef) };
            // SAFETY: CoreGraphics' own constant keys
            let get = |key: CFStringRef| info.find(unsafe { CFString::wrap_under_get_rule(key) }).map(|v| v.clone());
            let number = |key| get(key).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_f64());
            let text = |key| get(key).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string()).unwrap_or_default();
            // SAFETY: as above
            let (layer, alpha, number_key) = unsafe { (kCGWindowLayer, kCGWindowAlpha, kCGWindowNumber) };
            if number(layer) != Some(0.0) || number(alpha).is_some_and(|a| a <= 0.0) {
                continue; // the menu bar, the Dock, overlays, and windows that can't be seen
            }
            // SAFETY: as above
            let Some(bounds) = get(unsafe { kCGWindowBounds }).and_then(|v| {
                if v.type_of() != CFDictionary::<CFString, CFType>::type_id() {
                    return None;
                }
                // SAFETY: a CFDictionary, checked just now, which `v` keeps alive
                let d: CFDictionary<CFString, CFType> =
                    unsafe { CFDictionary::wrap_under_get_rule(v.as_CFTypeRef() as CFDictionaryRef) };
                let n =
                    |k: &str| d.find(CFString::new(k)).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_f64());
                Some(Rect::new(n("X")?, n("Y")?, n("Width")?, n("Height")?))
            }) else {
                continue;
            };
            if bounds.width < SMALLEST || bounds.height < SMALLEST {
                continue;
            }
            let Some(display) = place(&displays, &bounds) else { continue };
            let Some(id) = number(number_key) else { continue };
            // SAFETY: as above
            let (app, title) = unsafe { (text(kCGWindowOwnerName), text(kCGWindowName)) };
            windows.push(Window { id: (id as i64).to_string(), app, title, display, bounds });
        }
    }
    Ok(Found { displays, windows, notes: Vec::new() })
}

#[cfg(not(target_os = "macos"))]
fn real() -> Result<Found> {
    anyhow::bail!("this isn't a Mac")
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[cfg(target_os = "macos")]
fn preflight() -> bool {
    // SAFETY: no arguments; it only reads this process's permission
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(target_os = "macos")]
fn request() {
    // SAFETY: as above; it may show macOS's prompt, once
    unsafe { CGRequestScreenCaptureAccess() };
}

#[cfg(not(target_os = "macos"))]
fn preflight() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn request() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_list_places_each_window_on_its_display() {
        let found = fake(
            r#"{"displays": [{"name": "Built-in display", "x": 0, "y": 0, "width": 1512, "height": 982, "pixel_width": 3024, "pixel_height": 1964, "main": true},
                             {"name": "Display 2", "x": 1512, "y": 0, "width": 1920, "height": 1080, "pixel_width": 1920, "pixel_height": 1080}],
                "windows": [{"id": 7, "app": "Blender", "title": "Untitled", "x": 1600, "y": 40, "width": 800, "height": 600},
                            {"id": "8", "app": "Lost", "title": "", "x": -4000, "y": 0, "width": 800, "height": 600}]}"#,
        )
        .unwrap();
        assert_eq!(found.displays[1].id, "1");
        assert_eq!(found.displays[0].scale(), 2.0);
        assert!(found.displays[0].main && !found.displays[1].main);
        assert_eq!(found.windows.len(), 1, "one off every display is left out");
        assert_eq!((found.windows[0].id.as_str(), found.windows[0].display.as_str()), ("7", "1"));
    }
}
