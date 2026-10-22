//! The extension its node runs: `screen.list`, `screen.still`, `screen.show` and the `live` pane.
//! What they show answers only whoever asked: a list or a still is the request's result (a still is
//! posted only where its `thread_id` says), and nothing about the screen is ever an event. The screen
//! is captured only for a still someone asked for, or while a live pane has a viewer.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use pointman_extension::{Extension, Handle, Job, Pane, Refused};
use serde_json::{json, Value};

use crate::backend::Backend;
use crate::control::{self, Control, Grant};
use crate::live::{self, Live, Settings};
use crate::screens::{self, Found, Resolved, Target};

pub fn serve() -> Result<()> {
    let mut ext = Extension::new()?;
    ext.command("screen.list", list);
    ext.command("screen.still", still);
    ext.command("screen.show", show);
    ext.pane("live", open);
    let ran = ext.run();
    // the node said shutdown (or went): no capture outlives the extension
    live::close_all();
    ran
}

fn settings(h: &Handle) -> Settings {
    let s = h.settings();
    let number = |key: &str, default: u32| s[key].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(default);
    Settings { fps: number("fps", 10).max(1), width: number("width", 1920).max(16), quality: number("quality", 5) }
}

fn machine(h: &Handle) -> String {
    h.node()["machine"].as_str().filter(|m| !m.is_empty()).unwrap_or("this machine").to_string()
}

/// What a target is now, with everything listed.
fn resolve(backend: &Backend, target: &Target, machine: &str) -> Result<(Resolved, Found)> {
    let found = backend.list()?;
    let shown = screens::resolve(&found, target, machine)?;
    Ok((shown, found))
}

/// A name as a file's.
fn file_name(name: &str) -> String {
    let name: String =
        name.chars().map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c }).take(80).collect();
    match name.trim() {
        "" => "screen.jpg".into(),
        name => format!("{name}.jpg"),
    }
}

// ------------------------------------------------------------------ commands

/// How an agent shows the screen live and steers it, in screen.list's and screen.still's answers: that's
/// where an agent reads it (run_on_machine lists only each command's title and params, and a summary is
/// the "/" menu's).
fn live_how(machine: &str) -> String {
    format!(
        "To show it live on their device: open_pane(extension=\"screen\", pane=\"live\", target={{\"app\": \
         \"Blender\"}}). Then run_on_machine(\"screen.show\", {{\"target\": …}}, machine=\"{machine}\") points it \
         at another display, app or window, with a region to zoom in, while they watch."
    )
}

fn list(job: &Job) -> Result<Value> {
    let h = job.handle();
    let (backend, machine) = (Backend::here(), machine(h));
    let found = backend.list()?;
    let mut answer = screens::listing(&found, &machine, backend.units());
    answer["text"] = json!(format!("{}\n\n{}", answer["text"].as_str().unwrap_or_default(), live_how(&machine)));
    Ok(answer)
}

fn still(job: &Job) -> Result<Value> {
    let h = job.handle();
    let (backend, s, machine) = (Backend::here(), settings(h), machine(h));
    let target = Target::parse(&job.params["target"], &job.params["region"])?;
    backend.ready()?;
    let (shown, _) = resolve(&backend, &target, &machine)?;
    let width = job.params["width"].as_u64().and_then(|w| u32::try_from(w).ok()).unwrap_or(s.width);
    job.progress(None, "taking a still");
    let out = job.work.join(file_name(&shown.title));
    let cancelled = || job.cancelled();
    let (w, h) = backend.still(&shown, width, s.quality, &out, &cancelled)?;
    let bytes = std::fs::metadata(&out)?.len();
    let units = backend.units();
    let whole = shown.whole.to_json();
    let meta = json!({"screen": shown.title, "display": shown.display.id, "width": w, "height": h});
    job.output(&out, "image", "main", meta);
    // a region is in screen.list's units, from the target's top-left: what one of them is in this picture
    let scale = f64::from(w) / shown.area.width.max(1.0);
    let text = format!(
        "A still of {} on {machine}, {w}×{h}: {}×{} {units} of it, so {:.2} pixels of the still a unit. A region \
         ({{x, y, width, height}}) is in {units} from its top-left. {}",
        shown.title,
        shown.area.width.round(),
        shown.area.height.round(),
        scale,
        live_how(&machine)
    );
    Ok(json!({"name": shown.title, "display": shown.display.id, "window": shown.window.as_ref().map(|w| &w.id),
              "width": w, "height": h, "bytes": bytes, "target": whole, "area": shown.area.to_json(), "units": units,
              "scale": (scale * 1000.0).round() / 1000.0, "text": text}))
}

fn show(job: &Job) -> Result<Value> {
    let h = job.handle();
    let (backend, machine) = (Backend::here(), machine(h));
    let target = Target::parse(&job.params["target"], &job.params["region"])?;
    let pane = job.params["pane"].as_str().map(str::trim).filter(|p| !p.is_empty());
    let lives = live::find(pane);
    if lives.is_empty() {
        let open = live::find(None).iter().map(|l| l.id().to_string()).collect::<Vec<_>>();
        match (pane, open.is_empty()) {
            (Some(p), false) => bail!("no live pane {p} is open on {machine}: {} is", open.join(", ")),
            _ => bail!(
                "no live screen pane is open on {machine}: open one first, with open_pane(extension=\"screen\", \
                 pane=\"live\", target={{\"app\": \"Blender\"}}), then screen.show steers it"
            ),
        }
    }
    let (shown, found) = resolve(&backend, &target, &machine)?;
    let mut panes = Vec::new();
    for l in &lives {
        l.show(target.clone(), shown.clone(), found.displays.clone());
        panes.push(json!({"pane": l.id(), "title": l.title()}));
    }
    let which = if lives.len() == 1 { "The live pane".to_string() } else { format!("{} live panes", lives.len()) };
    Ok(json!({"panes": panes, "title": shown.title, "area": shown.area.to_json(), "units": backend.units(),
              "text": format!("{which} on {machine} shows {} now", shown.title)}))
}

// ------------------------------------------------------------------ the live pane

fn open(pane: &Pane) -> Result<Value> {
    let h = pane.handle();
    let (backend, s, machine) = (Backend::here(), settings(h), machine(h));
    let target = Target::parse(pane.target(), &Value::Null)?;
    backend.ready()?;
    let (shown, found) = resolve(&backend, &target, &machine)?;
    let (live, answer) = Live::open(pane.clone(), backend, target, shown, found.displays, s, machine);
    let watching = live.clone();
    pane.on_viewers(move |_: &Pane, attached: u64| {
        if let Some(live) = watching.upgrade() {
            live.viewers(attached);
        }
    });
    // control: a person's grant, checked by the node; their pointer, scroll, key and text inputs act on this
    // Mac while it holds (src/control.rs)
    let control = Arc::new(Mutex::new(Control::default()));
    let (granted, on_mac) = (control.clone(), matches!(Backend::here(), Backend::Mac));
    pane.on_control(move |pane: &Pane, grant: Option<&Value>| -> Result<()> {
        let Some(grant) = grant else {
            let let_go = lock(&granted).set(None);
            pane.handle().log(format!("pane {}: control ended", pane.id()));
            return control::post(&let_go);
        };
        if !on_mac {
            return Err(Refused::new("control works on macOS only for now").into());
        }
        if let Err(e) = control::check() {
            return Err(Refused::new(e.to_string()).into());
        }
        let grant = Grant::parse(grant)?;
        pane.handle().log(format!(
            "pane {}: {} controls it from {} until {}",
            pane.id(),
            grant.by,
            grant.device,
            grant.until
        ));
        lock(&granted).set(Some(grant));
        Ok(())
    });
    let (hinted, acting) = (live.clone(), control.clone());
    pane.on_input(move |_: &Pane, event: &Value| -> Result<()> {
        if matches!(event["kind"].as_str(), Some("pointer" | "scroll" | "key" | "text")) {
            let Some(live) = hinted.upgrade() else { return Err(Refused::new("the pane is closing").into()) };
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let acts = lock(&acting).acts(event, live.area(), now).map_err(|e| Refused::new(e.to_string()))?;
            return control::post(&acts);
        }
        if event["kind"] != "hint" {
            return Err(Refused::new(
                "the screen pane takes hints, and pointer, scroll, key and text while someone controls it: steer \
                 it with screen.show",
            )
            .into());
        }
        // a hint says only what changed; a rate may be a fraction
        let number =
            |key: &str| event[key].as_f64().filter(|n| *n >= 0.0).map(|n| n.ceil().min(f64::from(u32::MAX)) as u32);
        if let Some(live) = hinted.upgrade() {
            live.hint(number("max_fps"), number("max_width"));
        }
        Ok(())
    });
    pane.on_close(move |_: &Pane, _: &str| {
        let _ = control::post(&lock(&control).set(None)); // nothing left held down
        if let Some(live) = live.upgrade() {
            live.close();
        }
    });
    Ok(answer)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_as_a_file_name() {
        assert_eq!(file_name("Blender: Untitled.blend"), "Blender- Untitled.blend.jpg");
        assert_eq!(file_name("~/a/b"), "~-a-b.jpg");
        assert_eq!(file_name("  "), "screen.jpg");
    }
}
