//! Control of what a live pane shows (Pointman's EXT-57, the design in thread 649 [18195]): while the node
//! says a person holds a grant (`pane.control`, signed by their device and at most an hour long), the app's
//! `pointer`, `scroll`, `key` and `text` inputs become events on this Mac, at the place in the pane's picture
//! they point at. The node passes those inputs only while a grant holds; this refuses them too, after
//! `until` or with none.
//!
//! - **Only macOS for now,** and only with Accessibility allowed for the node: a grant without it is
//!   turned down with where to allow it, as a capture without Screen Recording is.
//! - **Where:** `x` and `y` are 0–1 of the picture, so they land on what the pane shows now: the window
//!   where it is, or the region zoomed into.
//! - **Nothing left held down:** when control ends (stopped, run out, or the pane closed), a button or
//!   key still down is let go.
//!
//! The tests run it anywhere with SCREEN_FAKE_CONTROL, a file each event is written to instead of being
//! posted, and SCREEN_FAKE_TRUSTED=0 for a Mac without Accessibility.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::geom::Rect;

pub const FAKE_ENV: &str = "SCREEN_FAKE_CONTROL";
/// The longest text a paste may be.
const MAX_TEXT: usize = 4000;
/// UTF-16 units in one typed event: macOS takes at most 20.
const CHUNK: usize = 20;
/// The Return key's code: a paste's newlines.
const RETURN: u16 = 36;

/// Who holds control of a pane, and until when (unix seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub until: u64,
    pub by: String,
    pub device: String,
}

impl Grant {
    /// The grant in `pane.control`, as the SDK gives it: `{until, by, device}`.
    pub fn parse(v: &Value) -> Result<Grant> {
        let until = v["until"].as_u64().ok_or_else(|| anyhow!("a grant's until is unix seconds"))?;
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Ok(Grant { until, by: text("by"), device: text("device") })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Button {
    Left,
    Right,
}

/// One event for macOS, at a place in the screen's own coordinates (global points, top-left origin, as
/// CGDisplayBounds and CGEvent both have them).
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// The pointer moves there: dragging while a button is held.
    Move {
        x: f64,
        y: f64,
        drag: Option<Button>,
    },
    Press {
        button: Button,
        down: bool,
        x: f64,
        y: f64,
        clicks: i64,
        flags: u64,
    },
    /// Pixels, signed as NSEvent's scrollingDelta: dy above 0 scrolls up.
    Scroll {
        x: f64,
        y: f64,
        dx: i32,
        dy: i32,
    },
    Key {
        code: u16,
        down: bool,
        flags: u64,
    },
    /// Typed as it is, at most [`CHUNK`] UTF-16 units.
    Type(String),
}

impl Act {
    /// As SCREEN_FAKE_CONTROL writes it.
    pub fn to_json(&self) -> Value {
        let b = |b: &Button| if *b == Button::Left { "left" } else { "right" };
        match self {
            Act::Move { x, y, drag } => json!({"move": [x, y], "drag": drag.as_ref().map(b)}),
            Act::Press { button, down, x, y, clicks, flags } => {
                json!({"press": b(button), "down": down, "at": [x, y], "clicks": clicks, "flags": flags})
            }
            Act::Scroll { x, y, dx, dy } => json!({"scroll": [dx, dy], "at": [x, y]}),
            Act::Key { code, down, flags } => json!({"key": code, "down": down, "flags": flags}),
            Act::Type(text) => json!({"type": text}),
        }
    }
}

/// A pane's control: its grant, and what's held down, so it can all be let go.
#[derive(Debug, Default)]
pub struct Control {
    grant: Option<Grant>,
    held: Option<Button>,
    keys: BTreeSet<u16>,
    at: (f64, f64),
}

impl Control {
    pub fn grant(&self) -> Option<&Grant> {
        self.grant.as_ref()
    }

    /// A grant given (checked by the caller), or None: control ended, and what's held is let go.
    pub fn set(&mut self, grant: Option<Grant>) -> Vec<Act> {
        let done = if grant.is_none() { self.release() } else { Vec::new() };
        self.grant = grant;
        done
    }

    /// Everything still held down, let go.
    pub fn release(&mut self) -> Vec<Act> {
        let (x, y) = self.at;
        let mut acts: Vec<Act> = self
            .held
            .take()
            .map(|button| Act::Press { button, down: false, x, y, clicks: 1, flags: 0 })
            .into_iter()
            .collect();
        acts.extend(std::mem::take(&mut self.keys).into_iter().map(|code| Act::Key { code, down: false, flags: 0 }));
        acts
    }

    /// An input as what macOS should do, on `area` (what the pane shows now), at `now` (unix seconds):
    /// refused without a grant or after its `until`, or when it isn't one of these.
    pub fn acts(&mut self, event: &Value, area: Rect, now: u64) -> Result<Vec<Act>> {
        match &self.grant {
            None => bail!("control is off"),
            Some(g) if now >= g.until => bail!("control ran out: turn it on again"),
            Some(_) => {}
        }
        let place = |e: &Value| -> Result<(f64, f64)> {
            let f = |k: &str| e[k].as_f64().filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 1.0));
            let (Some(x), Some(y)) = (f("x"), f("y")) else { bail!("x and y say where, each 0-1 of the picture") };
            Ok((area.x + x * area.width, area.y + y * area.height))
        };
        let flags = modifiers(&event["mods"])?;
        Ok(match event["kind"].as_str() {
            Some("pointer") => {
                let (x, y) = place(event)?;
                self.at = (x, y);
                let button = match event["button"].as_str() {
                    None | Some("left") => Button::Left,
                    Some("right") => Button::Right,
                    Some(b) => bail!("button is left or right, not {b}"),
                };
                let clicks = event["clicks"].as_i64().unwrap_or(1).clamp(1, 3);
                match event["phase"].as_str() {
                    None | Some("move") => vec![Act::Move { x, y, drag: self.held }],
                    Some("down") => {
                        let mut acts = self.release_button(x, y);
                        self.held = Some(button);
                        acts.push(Act::Press { button, down: true, x, y, clicks, flags });
                        acts
                    }
                    Some("up") => {
                        self.held = None;
                        vec![Act::Press { button, down: false, x, y, clicks, flags }]
                    }
                    Some(p) => bail!("phase is move, down or up, not {p}"),
                }
            }
            Some("scroll") => {
                let (x, y) = if event["x"].is_null() && event["y"].is_null() { self.at } else { place(event)? };
                self.at = (x, y);
                let px =
                    |k: &str| event[k].as_f64().filter(|n| n.is_finite()).unwrap_or(0.0).clamp(-10_000.0, 10_000.0);
                vec![
                    Act::Move { x, y, drag: self.held },
                    Act::Scroll { x, y, dx: px("dx").round() as i32, dy: px("dy").round() as i32 },
                ]
            }
            Some("key") => {
                let code = event["code"]
                    .as_u64()
                    .filter(|c| *c < 128)
                    .ok_or_else(|| anyhow!("code is a macOS key code, 0-127"))? as u16;
                let down = match event["phase"].as_str() {
                    Some("down") => true,
                    Some("up") => false,
                    None => {
                        return Ok(vec![Act::Key { code, down: true, flags }, Act::Key { code, down: false, flags }])
                    }
                    Some(p) => bail!("phase is down or up, not {p}"),
                };
                if down {
                    self.keys.insert(code);
                } else {
                    self.keys.remove(&code);
                }
                vec![Act::Key { code, down, flags }]
            }
            Some("text") => typed(event["text"].as_str().ok_or_else(|| anyhow!("text is the text to type"))?)?,
            Some(k) => bail!("{k} isn't an input that controls the screen"),
            None => bail!("an input without a kind"),
        })
    }

    /// A press while another button is held lets that one go first.
    fn release_button(&mut self, x: f64, y: f64) -> Vec<Act> {
        self.held
            .take()
            .map(|button| Act::Press { button, down: false, x, y, clicks: 1, flags: 0 })
            .into_iter()
            .collect()
    }
}

/// `mods` as CGEventFlags: cmd, opt (or alt), ctrl and shift.
fn modifiers(mods: &Value) -> Result<u64> {
    let mut flags = 0;
    for m in mods.as_array().map(Vec::as_slice).unwrap_or_default() {
        flags |= match m.as_str().unwrap_or_default() {
            "shift" => 0x0002_0000,
            "ctrl" | "control" => 0x0004_0000,
            "opt" | "alt" | "option" => 0x0008_0000,
            "cmd" | "command" => 0x0010_0000,
            other => bail!("mods are cmd, opt, ctrl and shift, not {other}"),
        };
    }
    Ok(flags)
}

/// A paste as typed text, in chunks macOS takes, with each line break as Return.
fn typed(text: &str) -> Result<Vec<Act>> {
    if text.chars().count() > MAX_TEXT {
        bail!("text is at most {MAX_TEXT} characters at once");
    }
    let mut acts = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            acts.push(Act::Key { code: RETURN, down: true, flags: 0 });
            acts.push(Act::Key { code: RETURN, down: false, flags: 0 });
        }
        let mut chunk = String::new();
        for c in line.chars().filter(|c| *c != '\r') {
            if chunk.encode_utf16().count() + c.len_utf16() > CHUNK {
                acts.push(Act::Type(std::mem::take(&mut chunk)));
            }
            chunk.push(c);
        }
        if !chunk.is_empty() {
            acts.push(Act::Type(chunk));
        }
    }
    Ok(acts)
}

/// Whether macOS lets the node post events (Accessibility), asking it once a run when it doesn't.
pub fn check() -> Result<()> {
    if std::env::var_os(FAKE_ENV).is_some() {
        return match std::env::var("SCREEN_FAKE_TRUSTED").as_deref() {
            Ok("0") => Err(anyhow!("{}", refused())),
            _ => Ok(()),
        };
    }
    if trusted(true) {
        Ok(())
    } else {
        Err(anyhow!("{}", refused()))
    }
}

/// Where to allow it.
fn refused() -> String {
    let (name, path) = crate::mac::responsible().unwrap_or_else(|| ("pointman-node".into(), String::new()));
    let add = if path.is_empty() { String::new() } else { format!(" (+ adds it from {path} if it isn't listed)") };
    format!(
        "macOS hasn't allowed the node to control this Mac. On it: System Settings, Privacy & Security, \
         Accessibility, turn on {name}{add}, then turn Control on again"
    )
}

/// The acts, done: posted to macOS, or written to SCREEN_FAKE_CONTROL's file.
pub fn post(acts: &[Act]) -> Result<()> {
    if acts.is_empty() {
        return Ok(());
    }
    if let Some(path) = std::env::var_os(FAKE_ENV) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        for a in acts {
            writeln!(f, "{}", a.to_json())?;
        }
        return Ok(());
    }
    real::post(acts)
}

#[cfg(target_os = "macos")]
fn trusted(ask: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
    }
    static ASKED: AtomicBool = AtomicBool::new(false);
    if ask && !ASKED.swap(true, Ordering::SeqCst) {
        // kAXTrustedCheckOptionPrompt: macOS shows its prompt, once
        let options =
            CFDictionary::from_CFType_pairs(&[(CFString::new("AXTrustedCheckOptionPrompt"), CFBoolean::true_value())]);
        // SAFETY: a dictionary of one CFString key and a CFBoolean, alive across the call
        return unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    }
    // SAFETY: no arguments; it only reads this process's trust
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
fn trusted(_ask: bool) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod real {
    use anyhow::{anyhow, Result};
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    use super::{Act, Button};

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|()| anyhow!("macOS made no event source"))
    }

    pub fn post(acts: &[Act]) -> Result<()> {
        for act in acts {
            let event = match act {
                Act::Move { x, y, drag } => {
                    let (kind, button) = match drag {
                        None => (CGEventType::MouseMoved, CGMouseButton::Left),
                        Some(Button::Left) => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
                        Some(Button::Right) => (CGEventType::RightMouseDragged, CGMouseButton::Right),
                    };
                    CGEvent::new_mouse_event(source()?, kind, CGPoint::new(*x, *y), button)
                }
                Act::Press { button, down, x, y, clicks, flags } => {
                    let (kind, b) = match (button, down) {
                        (Button::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                        (Button::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                        (Button::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                        (Button::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                    };
                    CGEvent::new_mouse_event(source()?, kind, CGPoint::new(*x, *y), b).inspect(|e| {
                        e.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, *clicks);
                        e.set_flags(CGEventFlags::from_bits_truncate(*flags));
                    })
                }
                Act::Scroll { dx, dy, .. } => {
                    CGEvent::new_scroll_event(source()?, ScrollEventUnit::PIXEL, 2, *dy, *dx, 0)
                }
                Act::Key { code, down, flags } => CGEvent::new_keyboard_event(source()?, *code, *down)
                    .inspect(|e| e.set_flags(CGEventFlags::from_bits_truncate(*flags))),
                Act::Type(text) => {
                    for down in [true, false] {
                        let e =
                            CGEvent::new_keyboard_event(source()?, 0, down).map_err(|()| anyhow!("no key event"))?;
                        e.set_string(text);
                        e.post(CGEventTapLocation::HID);
                    }
                    continue;
                }
            };
            event.map_err(|()| anyhow!("macOS made no event for {act:?}"))?.post(CGEventTapLocation::HID);
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod real {
    use anyhow::{bail, Result};

    pub fn post(_acts: &[super::Act]) -> Result<()> {
        bail!("control works on macOS only for now")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(2152.0, 160.0, 1280.0, 720.0);
    const NOW: u64 = 1_792_000_000;

    fn held() -> Control {
        let mut c = Control::default();
        c.set(Some(Grant { until: NOW + 600, by: "james".into(), device: "MacBook Pro".into() }));
        c
    }

    #[test]
    fn nothing_moves_without_a_grant_or_after_it_runs_out() {
        let click = json!({"kind": "pointer", "x": 0.5, "y": 0.5, "phase": "down"});
        let e = Control::default().acts(&click, AREA, NOW).unwrap_err();
        assert_eq!(e.to_string(), "control is off");
        let e = held().acts(&click, AREA, NOW + 600).unwrap_err();
        assert!(e.to_string().starts_with("control ran out"), "{e}");
    }

    #[test]
    fn a_place_in_the_picture_is_the_same_place_on_the_screen() {
        let mut c = held();
        let acts = c.acts(&json!({"kind": "pointer", "x": 0.25, "y": 0.5}), AREA, NOW).unwrap();
        assert_eq!(acts, vec![Act::Move { x: 2152.0 + 320.0, y: 160.0 + 360.0, drag: None }]);
        // past an edge, it stays on the picture
        let acts = c.acts(&json!({"kind": "pointer", "x": 1.5, "y": -1, "phase": "move"}), AREA, NOW).unwrap();
        assert_eq!(acts, vec![Act::Move { x: 3432.0, y: 160.0, drag: None }]);
    }

    #[test]
    fn a_click_a_drag_and_a_double_click() {
        let mut c = held();
        let ev = |x: f64, phase: &str, clicks: i64| json!({"kind": "pointer", "x": x, "y": 0.0, "phase": phase, "clicks": clicks, "button": "left", "mods": ["cmd"]});
        let down = c.acts(&ev(0.0, "down", 2), AREA, NOW).unwrap();
        assert_eq!(
            down,
            vec![Act::Press { button: Button::Left, down: true, x: 2152.0, y: 160.0, clicks: 2, flags: 0x0010_0000 }]
        );
        let drag = c.acts(&ev(0.5, "move", 1), AREA, NOW).unwrap();
        assert_eq!(drag, vec![Act::Move { x: 2792.0, y: 160.0, drag: Some(Button::Left) }]);
        let up = c.acts(&ev(0.5, "up", 1), AREA, NOW).unwrap();
        assert!(matches!(up[0], Act::Press { down: false, .. }));
        assert_eq!(c.release(), vec![]); // nothing left held
    }

    #[test]
    fn scrolling_happens_where_it_points() {
        let mut c = held();
        let acts = c.acts(&json!({"kind": "scroll", "dx": 0, "dy": 12.4, "x": 0.5, "y": 0.5}), AREA, NOW).unwrap();
        assert_eq!(
            acts,
            vec![Act::Move { x: 2792.0, y: 520.0, drag: None }, Act::Scroll { x: 2792.0, y: 520.0, dx: 0, dy: 12 }]
        );
        // without a place, where the pointer last was
        let acts = c.acts(&json!({"kind": "scroll", "dy": -3}), AREA, NOW).unwrap();
        assert_eq!(acts[1], Act::Scroll { x: 2792.0, y: 520.0, dx: 0, dy: -3 });
    }

    #[test]
    fn keys_and_their_modifiers() {
        let mut c = held();
        let acts = c
            .acts(
                &json!({"kind": "key", "code": 8, "chars": "c", "mods": ["cmd", "shift"], "phase": "down"}),
                AREA,
                NOW,
            )
            .unwrap();
        assert_eq!(acts, vec![Act::Key { code: 8, down: true, flags: 0x0012_0000 }]);
        // a key with no phase is pressed and let go
        let acts = c.acts(&json!({"kind": "key", "code": 36}), AREA, NOW).unwrap();
        assert_eq!(acts.len(), 2);
        assert!(c.acts(&json!({"kind": "key", "code": 300}), AREA, NOW).is_err());
        assert!(c.acts(&json!({"kind": "key", "code": 8, "mods": ["hyper"]}), AREA, NOW).is_err());
    }

    #[test]
    fn a_paste_is_typed_in_chunks_with_return_for_each_line() {
        let mut c = held();
        let acts = c.acts(&json!({"kind": "text", "text": "hello world, this is long\nok"}), AREA, NOW).unwrap();
        assert_eq!(
            acts,
            vec![
                Act::Type("hello world, this is".into()),
                Act::Type(" long".into()),
                Act::Key { code: 36, down: true, flags: 0 },
                Act::Key { code: 36, down: false, flags: 0 },
                Act::Type("ok".into()),
            ]
        );
        assert!(c.acts(&json!({"kind": "text", "text": "x".repeat(4001)}), AREA, NOW).is_err());
    }

    #[test]
    fn ending_control_lets_go_of_what_was_held() {
        let mut c = held();
        c.acts(&json!({"kind": "pointer", "x": 0.0, "y": 0.0, "phase": "down", "button": "right"}), AREA, NOW).unwrap();
        c.acts(&json!({"kind": "key", "code": 56, "phase": "down"}), AREA, NOW).unwrap();
        let let_go = c.set(None);
        assert_eq!(
            let_go,
            vec![
                Act::Press { button: Button::Right, down: false, x: 2152.0, y: 160.0, clicks: 1, flags: 0 },
                Act::Key { code: 56, down: false, flags: 0 },
            ]
        );
        assert!(c.grant().is_none());
        assert!(c.acts(&json!({"kind": "pointer", "x": 0.0, "y": 0.0}), AREA, NOW).is_err());
    }
}
