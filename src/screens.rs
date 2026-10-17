//! What a machine shows: its displays and the windows on them, front to back, and what a target names
//! in them. A target is a display (`{display}`: its id, `main`, or any part of its name), an app
//! (`{app}`: any part of its name, its frontmost window), a window (`{window}`: its id or any part of
//! its title), or none (the main display), with a `region` within it if you like. A target given as a
//! bare string (a slash command's) is an app, else a window, else a display.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::geom::Rect;

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// What it's named by: its place in the list (CGGetActiveDisplayList's, xrandr's monitors) or
    /// Hyprland's monitor id.
    pub id: String,
    pub name: String,
    /// Where it is, in the screen's coordinates.
    pub bounds: Rect,
    /// How big a capture of it is, in pixels.
    pub pixels: (u32, u32),
    pub main: bool,
    /// What the capture tool calls it: Hyprland's output (`eDP-1`), or macOS's CGDirectDisplayID, which
    /// ScreenCaptureKit takes; unused on X11.
    pub output: String,
}

impl Display {
    /// Its capture's pixels for each unit of its coordinates (2 on a Retina display).
    pub fn scale(&self) -> f64 {
        if self.bounds.width > 0.0 {
            f64::from(self.pixels.0) / self.bounds.width
        } else {
            1.0
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub id: String,
    pub app: String,
    pub title: String,
    /// The display most of it is on.
    pub display: String,
    pub bounds: Rect,
}

impl Window {
    /// "Blender: Untitled.blend", or just its app when it has no title.
    pub fn label(&self) -> String {
        if self.title.is_empty() || self.title == self.app {
            self.app.clone()
        } else {
            format!("{}: {}", self.app, self.title)
        }
    }
}

/// What a machine shows now, and anything that keeps part of it hidden (no window titles without a
/// permission, say).
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub displays: Vec<Display>,
    /// Front to back.
    pub windows: Vec<Window>,
    pub notes: Vec<String>,
}

impl Found {
    pub fn main(&self) -> Option<&Display> {
        self.displays.iter().find(|d| d.main).or(self.displays.first())
    }

    pub fn display(&self, id: &str) -> Option<&Display> {
        self.displays.iter().find(|d| d.id == id)
    }

    /// The display a rectangle is mostly on: its id, if it's on one at all.
    pub fn display_of(&self, r: &Rect) -> Option<String> {
        place(&self.displays, r)
    }
}

/// The display of `displays` a rectangle is mostly on.
pub fn place(displays: &[Display], r: &Rect) -> Option<String> {
    displays
        .iter()
        .filter_map(|d| r.intersect(&d.bounds).map(|o| (o.area(), d)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, d)| d.id.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// None: the main display.
    Display(Option<String>),
    App(String),
    Window(String),
    /// A bare string: an app, else a window, else a display.
    Any(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub what: What,
    /// Within it, from its top-left, in the screen's coordinates.
    pub region: Option<Rect>,
}

impl Target {
    pub const MAIN: Target = Target { what: What::Display(None), region: None };

    /// A target as a command or a pane gives it: `{display}`, `{app}`, `{window}`, any with `region`,
    /// a bare string, or nothing (the main display). `region` given beside it (`screen.show`'s) wins.
    pub fn parse(v: &Value, region: &Value) -> Result<Target> {
        let text = |v: &Value| match v {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        let mut target = match v {
            Value::Null => Target::MAIN,
            Value::String(s) if s.trim().is_empty() => Target::MAIN,
            Value::String(s) => Target { what: What::Any(s.trim().to_string()), region: None },
            Value::Object(o) => {
                let mut what = None;
                for (key, value) in o {
                    let given = text(value);
                    let named = match (key.as_str(), given) {
                        ("display", Some(d)) => What::Display(Some(d).filter(|d| !d.is_empty())),
                        ("display", None) if value.is_null() => What::Display(None),
                        ("app", Some(a)) if !a.is_empty() => What::App(a),
                        ("window", Some(w)) if !w.is_empty() => What::Window(w),
                        ("region", _) => continue,
                        ("display" | "app" | "window", _) => {
                            bail!("a target's {key} is a name or a number, not {value}")
                        }
                        _ => bail!(
                            "a target is {{display}}, {{app}} or {{window}}, with a region if you like: not {key}"
                        ),
                    };
                    if what.replace(named).is_some() {
                        bail!("a target is one of display, app or window, not more");
                    }
                }
                let region = match &o.get("region") {
                    None | Some(Value::Null) => None,
                    Some(r) => Some(region_of(r)?),
                };
                Target { what: what.unwrap_or(What::Display(None)), region }
            }
            _ => bail!("a target is {{display}}, {{app}} or {{window}}, or a name: not {v}"),
        };
        if !region.is_null() {
            target.region = Some(region_of(region)?);
        }
        Ok(target)
    }

    /// Whether it may move: an app's or a window's, so the pane follows it.
    pub fn follows(&self) -> bool {
        !matches!(self.what, What::Display(_))
    }
}

fn region_of(v: &Value) -> Result<Rect> {
    match Rect::from_json(v) {
        Some(r) => Ok(r),
        None => bail!("a region is {{x, y, width, height}}, its size above 0: not {v}"),
    }
}

/// What a target shows now.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub display: Display,
    /// The target itself, before any region: a display's bounds, or a window's.
    pub whole: Rect,
    /// What's captured: the region within it (or all of it), cut to the display.
    pub area: Rect,
    /// For the pane's title and the still's name.
    pub title: String,
    /// The window it shows, which the pane follows.
    pub window: Option<Window>,
}

impl Resolved {
    /// How big a capture of it is, in pixels.
    pub fn pixels(&self) -> (u32, u32) {
        let s = self.display.scale();
        let even = |n: f64| ((n * s).round() as u32 / 2 * 2).max(2);
        (even(self.area.width), even(self.area.height))
    }
}

/// What `target` shows in `found`, or why it shows nothing, naming what there is.
pub fn resolve(found: &Found, target: &Target, machine: &str) -> Result<Resolved> {
    let (display, whole, title, window) = match &target.what {
        What::Display(wanted) => {
            let d = display(found, wanted.as_deref(), machine)?;
            (d.clone(), d.bounds, d.name.clone(), None)
        }
        What::App(name) => shown(found, app(found, name).ok_or_else(|| no_app(found, name, machine))?, machine)?,
        What::Window(name) => {
            shown(found, window(found, name).ok_or_else(|| no_window(found, name, machine))?, machine)?
        }
        What::Any(name) => match app(found, name).or_else(|| window(found, name)) {
            Some(w) => shown(found, w, machine)?,
            None => match display(found, Some(name), machine) {
                Ok(d) => (d.clone(), d.bounds, d.name.clone(), None),
                Err(_) => bail!("nothing called '{name}' on {machine}: {}", everything(found)),
            },
        },
    };
    let inside = whole.intersect(&display.bounds).unwrap_or(display.bounds);
    let (area, title) = match &target.region {
        None => (inside, title),
        Some(region) => match whole.within(region).and_then(|r| r.intersect(&display.bounds)) {
            Some(area) => (area, format!("{title} (a region)")),
            None => bail!(
                "that region is outside {title}, which is {}×{} from its top-left",
                whole.width.round(),
                whole.height.round()
            ),
        },
    };
    Ok(Resolved { display, whole, area, title, window })
}

/// The window again (by its id, wherever it went), for a pane that follows it; none when it's gone.
pub fn again(found: &Found, last: &Resolved, target: &Target, machine: &str) -> Option<Resolved> {
    let id = last.window.as_ref()?.id.clone();
    resolve(found, &Target { what: What::Window(id), region: target.region }, machine).ok()
}

/// A window, with the display it's on and what its pane is called.
fn shown(found: &Found, w: &Window, machine: &str) -> Result<(Display, Rect, String, Option<Window>)> {
    let Some(d) = found.display(&w.display) else { bail!("{} isn't on any display of {machine} now", w.label()) };
    Ok((d.clone(), w.bounds, w.label(), Some(w.clone())))
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// The display `wanted` names: its id, `main`, or any part of its name; none named, the main one.
fn display<'a>(found: &'a Found, wanted: Option<&str>, machine: &str) -> Result<&'a Display> {
    let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty()) else {
        return found.main().ok_or_else(|| anyhow::anyhow!("{machine} has no display to show"));
    };
    let w = lower(wanted);
    let d = found.display(wanted).or_else(|| if w == "main" { found.main() } else { None }).or_else(|| {
        found
            .displays
            .iter()
            .find(|d| lower(&d.name) == w)
            .or_else(|| found.displays.iter().find(|d| lower(&d.name).contains(&w)))
    });
    match d {
        Some(d) => Ok(d),
        None => bail!("no display '{wanted}' on {machine}: it has {}", displays(found)),
    }
}

/// The frontmost window of the app `name` names: its whole name first, then the start of one, then any
/// part.
fn app<'a>(found: &'a Found, name: &str) -> Option<&'a Window> {
    let n = lower(name);
    let windows = || found.windows.iter().filter(|w| !w.app.is_empty());
    windows()
        .find(|w| lower(&w.app) == n)
        .or_else(|| windows().find(|w| lower(&w.app).starts_with(&n)))
        .or_else(|| windows().find(|w| lower(&w.app).contains(&n)))
}

/// The window `name` names: its id, else the frontmost whose title is it, then has it in it.
fn window<'a>(found: &'a Found, name: &str) -> Option<&'a Window> {
    let n = lower(name);
    found
        .windows
        .iter()
        .find(|w| w.id == name)
        .or_else(|| found.windows.iter().find(|w| !w.title.is_empty() && lower(&w.title) == n))
        .or_else(|| found.windows.iter().find(|w| !w.title.is_empty() && lower(&w.title).contains(&n)))
}

fn no_app(found: &Found, name: &str, machine: &str) -> anyhow::Error {
    let mut apps: Vec<&str> = Vec::new();
    for w in &found.windows {
        if !w.app.is_empty() && !apps.contains(&w.app.as_str()) {
            apps.push(&w.app);
        }
    }
    if apps.is_empty() {
        return anyhow::anyhow!("no app '{name}' with a window on {machine}: it shows no windows{}", why(found));
    }
    anyhow::anyhow!("no app '{name}' with a window on {machine}: it has {}", short(&apps, 12))
}

fn no_window(found: &Found, name: &str, machine: &str) -> anyhow::Error {
    if found.windows.is_empty() {
        return anyhow::anyhow!("no window '{name}' on {machine}: it shows none{}", why(found));
    }
    let windows: Vec<String> = found.windows.iter().map(|w| format!("{} {}", w.id, clip(&w.label(), 50))).collect();
    let windows: Vec<&str> = windows.iter().map(String::as_str).collect();
    anyhow::anyhow!("no window '{name}' on {machine}: it has {}{}", short(&windows, 10), why(found))
}

fn displays(found: &Found) -> String {
    let all: Vec<String> = found.displays.iter().map(|d| format!("{} {}", d.id, d.name)).collect();
    if all.is_empty() {
        "none".into()
    } else {
        all.join(", ")
    }
}

fn everything(found: &Found) -> String {
    let mut apps: Vec<&str> = Vec::new();
    for w in &found.windows {
        if !apps.contains(&w.app.as_str()) {
            apps.push(&w.app);
        }
    }
    format!("its displays are {}; its apps with windows {}", displays(found), short(&apps, 12))
}

fn why(found: &Found) -> String {
    if found.notes.is_empty() {
        String::new()
    } else {
        format!(". {}", found.notes.join(" "))
    }
}

fn short(items: &[&str], most: usize) -> String {
    if items.is_empty() {
        return "none".into();
    }
    let shown = items[..items.len().min(most)].join(", ");
    match items.len().saturating_sub(most) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

fn clip(s: &str, most: usize) -> String {
    if s.chars().count() <= most {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(most - 1).collect::<String>())
    }
}

/// `screen.list`'s answer: the displays, the windows front to back, and the same as text.
pub fn listing(found: &Found, machine: &str, units: &str) -> Value {
    let mut text = format!("Displays on {machine} (in {units}):\n");
    for d in &found.displays {
        let main = if d.main { ", the main one" } else { "" };
        text += &format!("- **{}** {}, {}×{}{main}\n", d.id, d.name, d.bounds.width.round(), d.bounds.height.round());
    }
    if found.windows.is_empty() {
        text += "\nNo windows.\n";
    } else {
        text += "\nWindows, front to back:\n";
        for w in &found.windows {
            let (b, on) = (w.bounds, &w.display);
            text += &format!(
                "- **{}** {}, {}×{} at {},{} on {on}\n",
                w.id,
                w.label(),
                b.width.round(),
                b.height.round(),
                b.x.round(),
                b.y.round()
            );
        }
    }
    for note in &found.notes {
        text += &format!("\n{note}\n");
    }
    let displays: Vec<Value> = found
        .displays
        .iter()
        .map(|d| {
            let b = d.bounds.to_json();
            json!({"id": d.id, "name": d.name, "width": b["width"], "height": b["height"], "x": b["x"], "y": b["y"],
                   "main": d.main})
        })
        .collect();
    let windows: Vec<Value> = found
        .windows
        .iter()
        .map(|w| {
            let b = w.bounds.to_json();
            json!({"id": w.id, "app": w.app, "title": w.title, "display": w.display, "x": b["x"], "y": b["y"],
                   "width": b["width"], "height": b["height"]})
        })
        .collect();
    json!({"displays": displays, "windows": windows, "units": units, "notes": found.notes, "text": text.trim_end()})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Mac with a Retina display and a 4K one to its right, a little higher, as CGWindowList gives
    /// its windows: front to back, in points.
    pub fn mac() -> Found {
        let d = |id: &str, name: &str, bounds: Rect, pixels, main| Display {
            id: id.into(),
            name: name.into(),
            bounds,
            pixels,
            main,
            output: String::new(),
        };
        let w = |id: &str, app: &str, title: &str, display: &str, bounds| Window {
            id: id.into(),
            app: app.into(),
            title: title.into(),
            display: display.into(),
            bounds,
        };
        Found {
            displays: vec![
                d("0", "Built-in display", Rect::new(0.0, 0.0, 1512.0, 982.0), (3024, 1964), true),
                d("1", "Display 2", Rect::new(1512.0, -200.0, 2560.0, 1440.0), (5120, 2880), false),
            ],
            windows: vec![
                w("410", "Ghostty", "~/Documents", "0", Rect::new(40.0, 60.0, 900.0, 600.0)),
                w("388", "Blender", "Untitled.blend", "1", Rect::new(1600.0, -150.0, 2000.0, 1200.0)),
                w("390", "Blender", "Preferences", "1", Rect::new(2000.0, 100.0, 600.0, 400.0)),
                w("12", "Code", "screen.rs — pointman-screen", "0", Rect::new(0.0, 25.0, 1512.0, 957.0)),
                w("15", "Xcode", "Pointman.xcodeproj", "0", Rect::new(0.0, 25.0, 1512.0, 957.0)),
            ],
            notes: vec![],
        }
    }

    fn target(v: Value) -> Target {
        Target::parse(&v, &Value::Null).unwrap()
    }

    #[test]
    fn targets_are_a_display_an_app_a_window_or_a_name() {
        assert_eq!(target(Value::Null), Target::MAIN);
        assert_eq!(target(json!({})), Target::MAIN);
        assert_eq!(target(json!({"display": 1})).what, What::Display(Some("1".into())));
        assert_eq!(target(json!({"app": "Blender"})).what, What::App("Blender".into()));
        assert_eq!(target(json!({"window": 388})).what, What::Window("388".into()));
        assert_eq!(target(json!("blender")).what, What::Any("blender".into()));
        let zoomed = target(json!({"app": "Blender", "region": {"x": 10, "y": 20, "width": 300, "height": 200}}));
        assert_eq!(zoomed.region, Some(Rect::new(10.0, 20.0, 300.0, 200.0)));
        assert!(zoomed.follows() && !Target::MAIN.follows());
        // a region beside it (screen.show's) wins
        let beside = Target::parse(&json!({"app": "Blender"}), &json!({"width": 100, "height": 50})).unwrap();
        assert_eq!(beside.region, Some(Rect::new(0.0, 0.0, 100.0, 50.0)));
        // and what isn't one says what is
        let e = |v: Value| Target::parse(&v, &Value::Null).unwrap_err().to_string();
        assert_eq!(e(json!({"app": "a", "window": "b"})), "a target is one of display, app or window, not more");
        assert!(e(json!({"screen": 1})).starts_with("a target is {display}, {app} or {window}"));
        assert!(e(json!({"app": "x", "region": {"x": 1}})).starts_with("a region is {x, y, width, height}"));
        assert!(e(json!({"app": ["x"]})).starts_with("a target's app is a name or a number"));
    }

    #[test]
    fn an_app_is_its_frontmost_window_by_any_part_of_its_name() {
        let found = mac();
        let r = resolve(&found, &target(json!({"app": "blender"})), "mac-m4").unwrap();
        assert_eq!(r.window.as_ref().unwrap().id, "388", "the frontmost of its two");
        assert_eq!(r.title, "Blender: Untitled.blend");
        assert_eq!(r.display.id, "1");
        // its whole name before a part of another's (Code, not Xcode), then the start of one
        assert_eq!(resolve(&found, &target(json!({"app": "code"})), "m").unwrap().window.unwrap().id, "12");
        assert_eq!(resolve(&found, &target(json!({"app": "xc"})), "m").unwrap().window.unwrap().id, "15");
        assert_eq!(resolve(&found, &target(json!({"app": "ghost"})), "m").unwrap().window.unwrap().id, "410");
        let e = resolve(&found, &target(json!({"app": "Resolve"})), "mac-m4").unwrap_err().to_string();
        assert_eq!(e, "no app 'Resolve' with a window on mac-m4: it has Ghostty, Blender, Code, Xcode");
    }

    #[test]
    fn a_window_is_its_id_or_any_part_of_its_title() {
        let found = mac();
        let by = |v: Value| resolve(&found, &target(v), "m").unwrap().window.unwrap().id;
        assert_eq!(by(json!({"window": 390})), "390");
        assert_eq!(by(json!({"window": "prefer"})), "390");
        assert_eq!(by(json!({"window": "XCODEPROJ"})), "15");
        let e = resolve(&found, &target(json!({"window": "nope"})), "mac-m4").unwrap_err().to_string();
        assert!(e.starts_with("no window 'nope' on mac-m4: it has 410 Ghostty: ~/Documents, 388 Blender"), "{e}");
        // a bare name: an app first, then a window's title, then a display
        assert_eq!(by(json!("blender")), "388");
        assert_eq!(by(json!("preferences")), "390");
        assert_eq!(resolve(&found, &target(json!("display 2")), "m").unwrap().display.id, "1");
        let e = resolve(&found, &target(json!("nothing")), "mac-m4").unwrap_err().to_string();
        assert!(e.starts_with("nothing called 'nothing' on mac-m4: its displays are 0 Built-in display, 1 Display 2"));
    }

    #[test]
    fn a_display_is_its_id_main_or_its_name_and_none_is_the_main_one() {
        let found = mac();
        let d = |v: Value| resolve(&found, &target(v), "m").unwrap();
        let main = d(Value::Null);
        assert_eq!(
            (main.display.id.as_str(), main.area, main.title.as_str()),
            ("0", main.display.bounds, "Built-in display")
        );
        assert_eq!(main.pixels(), (3024, 1964));
        assert_eq!(d(json!({"display": "main"})).display.id, "0");
        assert_eq!(d(json!({"display": 1})).display.id, "1");
        assert_eq!(d(json!({"display": "built-in"})).display.id, "0");
        let e = resolve(&found, &target(json!({"display": "lg"})), "mac-m4").unwrap_err().to_string();
        assert_eq!(e, "no display 'lg' on mac-m4: it has 0 Built-in display, 1 Display 2");
    }

    #[test]
    fn a_region_is_within_its_target_and_cut_to_its_display() {
        let found = mac();
        let region = json!({"app": "Blender", "region": {"x": 100, "y": 50, "width": 800, "height": 450}});
        let r = resolve(&found, &target(region), "m").unwrap();
        assert_eq!(r.whole, Rect::new(1600.0, -150.0, 2000.0, 1200.0));
        assert_eq!(r.area, Rect::new(1700.0, -100.0, 800.0, 450.0));
        assert_eq!(r.title, "Blender: Untitled.blend (a region)");
        assert_eq!(r.pixels(), (1600, 900), "2 pixels a point");
        // a window past its display's edge: what's on it
        let mut found = mac();
        found.windows[0].bounds = Rect::new(1000.0, 500.0, 900.0, 600.0);
        let r = resolve(&found, &target(json!({"window": 410})), "m").unwrap();
        assert_eq!(r.area, Rect::new(1000.0, 500.0, 512.0, 482.0));
        let e = resolve(
            &found,
            &target(json!({"window": 410, "region": {"x": 950, "y": 0, "width": 5, "height": 5}})),
            "m",
        );
        assert_eq!(
            e.unwrap_err().to_string(),
            "that region is outside Ghostty: ~/Documents, which is 900×600 from its top-left"
        );
    }

    #[test]
    fn a_moved_window_is_found_again_by_its_id() {
        let mut found = mac();
        let t = target(json!({"app": "Blender"}));
        let first = resolve(&found, &t, "m").unwrap();
        // it moves to the main display, and another Blender window comes in front of it
        found.windows[1].bounds = Rect::new(100.0, 100.0, 1000.0, 600.0);
        found.windows[1].display = "0".into();
        found.windows.swap(1, 2);
        let moved = again(&found, &first, &t, "m").unwrap();
        assert_eq!((moved.window.as_ref().unwrap().id.as_str(), moved.display.id.as_str()), ("388", "0"));
        assert_eq!(moved.area, Rect::new(100.0, 100.0, 1000.0, 600.0));
        assert_eq!(moved.title, "Blender: Untitled.blend");
        found.windows.retain(|w| w.id != "388");
        assert_eq!(again(&found, &first, &t, "m"), None);
    }

    #[test]
    fn a_window_is_placed_on_the_display_its_mostly_on() {
        let found = mac();
        assert_eq!(found.display_of(&Rect::new(1400.0, 0.0, 400.0, 300.0)), Some("1".into()));
        assert_eq!(found.display_of(&Rect::new(1300.0, 0.0, 400.0, 300.0)), Some("0".into()));
        assert_eq!(found.display_of(&Rect::new(-900.0, 0.0, 400.0, 300.0)), None);
    }

    #[test]
    fn the_listing_has_ids_sizes_and_units() {
        let list = listing(&mac(), "mac-m4", "points");
        assert_eq!(
            list["displays"][1],
            json!({"id": "1", "name": "Display 2", "width": 2560, "height": 1440, "x": 1512, "y": -200, "main": false})
        );
        assert_eq!(
            list["windows"][1],
            json!({"id": "388", "app": "Blender", "title": "Untitled.blend", "display": "1", "x": 1600, "y": -150, "width": 2000, "height": 1200})
        );
        let text = list["text"].as_str().unwrap();
        assert!(
            text.starts_with("Displays on mac-m4 (in points):\n- **0** Built-in display, 1512×982, the main one\n"),
            "{text}"
        );
        assert!(text.contains("- **388** Blender: Untitled.blend, 2000×1200 at 1600,-150 on 1\n"), "{text}");
    }
}
