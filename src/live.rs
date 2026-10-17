//! The live pane (Pointman's docs/dev/video.md): a display, app or window as `pane.frame`, captured only
//! while a path is attached. The capture tool writes MJPEG; the splitter cuts it into JPEGs; the newest
//! waits in one slot, and a sender takes whatever is newest each time, so a slow viewer never backs up
//! the capture. A hint restarts it at its rate and width (`max_fps: 0` stops it), and so does
//! [`Live::show`], an agent pointing the pane somewhere else.
//!
//! A window's pane follows it: once a second while it's captured, the window is found again, and if it
//! moved or changed size the capture starts again on its new place. If it's gone, the target is looked
//! for afresh (an app's next window); if nothing's there, the pane keeps the place it had. If the
//! capture stops on its own, it's tried again, longer apart each time.

use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use pointman_extension::Pane;
use serde_json::{json, Value};

use crate::backend::Backend;
use crate::ffmpeg::Pipeline;
use crate::geom::Rect;
use crate::jpeg::Splitter;
use crate::proc;
use crate::screens::{self, Display, Resolved, Target};

/// What the machine's settings say a stream should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub fps: u32,
    pub width: u32,
    pub quality: u32,
}

/// How often a followed window is found again.
pub const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// The widths a hint is rounded up to, so dragging a window doesn't restart the capture at each pixel.
const WIDTHS: [u32; 8] = [320, 480, 640, 960, 1280, 1920, 2560, 3840];

/// What a capture is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub fps: u32,
    pub width: u32,
}

/// The stream for this many paths attached and the app's last hint: none when nobody's watching or the
/// app paused it; otherwise the settings, lowered to the hint (its width rounded up to [`WIDTHS`]).
pub fn wanted(settings: Settings, viewers: u64, max_fps: u32, max_width: u32) -> Option<Want> {
    if viewers == 0 || max_fps == 0 {
        return None;
    }
    let width = WIDTHS.iter().copied().find(|w| *w >= max_width).unwrap_or(WIDTHS[WIDTHS.len() - 1]);
    Some(Want { fps: settings.fps.min(max_fps), width: settings.width.min(width) })
}

/// What a pane opens with: its title, and frames `width` across (the setting, so the app's hints may
/// ask for that much whatever it's pointed at later) in the target's shape.
pub fn opening(shown: &Resolved, settings: Settings) -> Value {
    let (w, h) = shown.pixels();
    let (w, h) = (u64::from(w.max(1)), u64::from(h));
    let width = u64::from((settings.width / 2 * 2).max(2));
    // its height at that width, rounded to even as ffmpeg's scale (h=-2) rounds it
    let height = ((h * width + w) / (w * 2)).max(1) * 2;
    json!({"title": shown.title, "video": {"codec": "mjpeg", "width": width, "height": height, "fps": settings.fps}})
}

/// One frame, as it came out of the splitter.
struct Frame {
    jpeg: Vec<u8>,
    width: u16,
    height: u16,
    captured_ms: u64,
}

/// The newest frame, and whether its capture is over.
#[derive(Default)]
struct Slot {
    frame: Mutex<Option<Frame>>,
    ready: Condvar,
    over: AtomicBool,
}

/// What one capture is of: the rate and width, and the place on which display.
#[derive(Debug, Clone, PartialEq)]
struct Plan {
    want: Want,
    display: String,
    area: Rect,
}

/// One run of the capture for the pane: its tools, the one that writes MJPEG last.
struct Capture {
    plan: Plan,
    tools: Vec<Arc<Mutex<Child>>>,
    slot: Arc<Slot>,
}

impl Capture {
    fn stop(&self) {
        thread::spawn(self.stopping());
    }

    /// Its frames over, and what stops its tools (the encoder first, so what feeds it finds no reader).
    fn stopping(&self) -> impl FnOnce() + Send + 'static {
        self.slot.over.store(true, Ordering::SeqCst);
        self.slot.ready.notify_all();
        let tools = self.tools.clone();
        move || {
            for tool in tools.iter().rev() {
                proc::stop(&mut lock(tool));
            }
        }
    }
}

struct State {
    viewers: u64,
    max_fps: u32,
    max_width: u32,
    closed: bool,
    capture: Option<Capture>,
    /// Counts each capture started, so a capture that ended knows if it's still the pane's.
    run: u64,
    /// Captures in a row that ended on their own.
    failures: u32,
    target: Target,
    shown: Resolved,
    /// The displays as last listed, for finding a window again where that asks only for the window.
    displays: Vec<Display>,
    /// The window it followed is gone, and that's been logged.
    lost: bool,
}

/// A live pane on a display, app or window.
pub struct Live {
    pane: Pane,
    backend: Backend,
    settings: Settings,
    machine: String,
    state: Mutex<State>,
    me: Weak<Live>,
}

/// The live panes open now: what keeps each alive until it closes (its pane's handlers hold it weakly,
/// as it holds its pane), and what `screen.show` points elsewhere.
static OPEN: Mutex<Vec<Arc<Live>>> = Mutex::new(Vec::new());

/// The open live panes: the one with this id, or every one.
pub fn find(pane: Option<&str>) -> Vec<Arc<Live>> {
    lock(&OPEN).iter().filter(|l| pane.is_none_or(|p| l.pane.id() == p) && l.pane.is_open()).cloned().collect()
}

/// Every pane's capture stopped, and waited for: the node is shutting the extension down, and a capture
/// tool left behind would outlive it (ffmpeg until its next write fails, wf-recorder perhaps longer).
pub fn close_all() {
    let lives: Vec<Arc<Live>> = lock(&OPEN).drain(..).collect();
    let stopping: Vec<_> = lives
        .iter()
        .filter_map(|l| {
            let mut state = lock(&l.state);
            state.closed = true;
            state.capture.take()
        })
        .map(|capture| thread::spawn(capture.stopping()))
        .collect();
    for stop in stopping {
        let _ = stop.join();
    }
}

impl Live {
    /// A live pane showing `shown` (what `target` is now; `displays` all there are), and what the pane
    /// opens with. It's kept until [`close`](Self::close).
    pub fn open(
        pane: Pane,
        backend: Backend,
        target: Target,
        shown: Resolved,
        displays: Vec<Display>,
        settings: Settings,
        machine: String,
    ) -> (Weak<Live>, Value) {
        let answer = opening(&shown, settings);
        let live = Arc::new_cyclic(|me| Live {
            pane,
            backend,
            settings,
            machine,
            state: Mutex::new(State {
                viewers: 0,
                max_fps: settings.fps,
                max_width: settings.width,
                closed: false,
                capture: None,
                run: 0,
                failures: 0,
                target,
                shown,
                displays,
                lost: false,
            }),
            me: me.clone(),
        });
        let weak = Arc::downgrade(&live);
        let following = weak.clone();
        thread::spawn(move || loop {
            thread::sleep(FOLLOW_EVERY);
            let Some(live) = following.upgrade() else { return };
            if lock(&live.state).closed {
                return;
            }
            live.follow();
        });
        lock(&OPEN).push(live);
        (weak, answer)
    }

    pub fn id(&self) -> &str {
        self.pane.id()
    }

    pub fn title(&self) -> String {
        lock(&self.state).shown.title.clone()
    }

    /// The node's count of paths attached.
    pub fn viewers(&self, attached: u64) {
        lock(&self.state).viewers = attached;
        self.apply();
    }

    /// The app's hint, which says only what changed: at most this rate (0 pauses), this width.
    pub fn hint(&self, max_fps: Option<u32>, max_width: Option<u32>) {
        {
            let mut state = lock(&self.state);
            if let Some(fps) = max_fps {
                state.max_fps = fps;
            }
            if let Some(width) = max_width {
                state.max_width = if width == 0 { self.settings.width } else { width };
            }
        }
        self.apply();
    }

    /// Point it at `target`, which is `shown` now: its title changes, and so does what it captures, at
    /// once if someone's watching. It stays open.
    pub fn show(&self, target: Target, shown: Resolved, displays: Vec<Display>) {
        {
            let mut state = lock(&self.state);
            state.target = target;
            state.displays = displays;
            state.lost = false;
            state.failures = 0;
            self.retitle(&mut state, shown);
        }
        self.apply();
    }

    /// Its pane closed: stop the capture, and let it go.
    pub fn close(&self) {
        lock(&self.state).closed = true;
        self.apply();
        lock(&OPEN).retain(|l| !std::ptr::eq(Arc::as_ptr(l), self));
    }

    fn retitle(&self, state: &mut State, shown: Resolved) {
        if shown.title != state.shown.title {
            self.pane.set_title(&shown.title);
        }
        state.shown = shown;
    }

    /// While it's captured: its window found again, and the capture moved with it.
    fn follow(&self) {
        let (target, last, displays) = {
            let state = lock(&self.state);
            if state.capture.is_none() || !state.target.follows() {
                return;
            }
            (state.target.clone(), state.shown.clone(), state.displays.clone())
        };
        let moved = self
            .backend
            .refresh(&last, &displays)
            .ok()
            .and_then(|found| screens::again(&found, &last, &target, &self.machine));
        let now = match moved {
            Some(now) => Ok((now, None)),
            // gone from where it was: the target afresh (an app's next window)
            None => self.backend.list().and_then(|found| {
                let now = screens::resolve(&found, &target, &self.machine)?;
                Ok((now, Some(found.displays)))
            }),
        };
        {
            let mut state = lock(&self.state);
            if state.target != target || state.shown != last {
                return; // pointed elsewhere meanwhile
            }
            match now {
                Ok((now, _)) if now == last => return,
                Ok((now, displays)) => {
                    state.lost = false;
                    if let Some(displays) = displays {
                        state.displays = displays;
                    }
                    self.retitle(&mut state, now);
                }
                Err(e) => {
                    if !state.lost {
                        state.lost = true;
                        self.pane
                            .handle()
                            .log(format!("live {}: it's gone, so the pane stays where it was: {e:#}", last.title));
                    }
                    return;
                }
            }
        }
        self.apply();
    }

    /// Start, stop or restart the capture to match what's wanted now.
    fn apply(&self) {
        let failed = {
            let mut state = lock(&self.state);
            let want =
                if state.closed { None } else { wanted(self.settings, state.viewers, state.max_fps, state.max_width) };
            let plan = want.map(|want| {
                let (display, area) = self.backend.place(&state.shown);
                Plan { want, display, area }
            });
            if state.capture.as_ref().map(|c| &c.plan) == plan.as_ref() {
                return;
            }
            if let Some(old) = state.capture.take() {
                old.stop();
            }
            let Some(plan) = plan else { return };
            state.run += 1;
            let run = state.run;
            match self.start(plan, &state.shown, run) {
                Ok(capture) => {
                    state.capture = Some(capture);
                    None
                }
                Err(e) => {
                    self.pane.handle().log(format!("live {}: {e:#}", state.shown.title));
                    Some(run)
                }
            }
        };
        if let Some(run) = failed {
            self.again(run, false);
        }
    }

    fn start(&self, plan: Plan, shown: &Resolved, run: u64) -> Result<Capture> {
        self.backend.ready()?;
        let Pipeline { source, mut encoder } =
            self.backend.live(shown, plan.want.fps, plan.want.width, self.settings.quality)?;
        let mut tools = Vec::new();
        let mut logs = Vec::new();
        if let Some(mut source) = source {
            let mut feeding = proc::spawn(&mut source)?;
            if let Some(out) = feeding.stdout.take() {
                encoder.stdin(Stdio::from(out));
            }
            logs.push(keep_log(feeding.stderr.take()));
            tools.push(feeding);
        }
        let mut child = match proc::spawn(&mut encoder) {
            Ok(child) => child,
            Err(e) => {
                tools.iter_mut().for_each(proc::stop);
                return Err(e);
            }
        };
        let stdout = child.stdout.take();
        logs.push(keep_log(child.stderr.take()));
        tools.push(child);
        let tools: Vec<Arc<Mutex<Child>>> = tools.into_iter().map(|t| Arc::new(Mutex::new(t))).collect();
        let slot = Arc::new(Slot::default());
        let (me, reading, title) = (self.me.clone(), slot.clone(), shown.title.clone());
        thread::spawn(move || {
            let started = Instant::now();
            let frames = stdout.map_or(0, |out| read(out, &reading));
            if reading.over.load(Ordering::SeqCst) {
                return; // stopped: what's wanted changed, or the pane closed
            }
            reading.over.store(true, Ordering::SeqCst);
            reading.ready.notify_all();
            if let Some(live) = me.upgrade() {
                // what the feeding tool said first: it says why (a window that closed), ffmpeg only that
                // its input ended
                let said: String = logs.iter().map(|l| lock(l).clone()).collect::<Vec<_>>().join("\n");
                let why = crate::ffmpeg::explain(&said);
                let ran = started.elapsed().as_secs();
                live.pane.handle().log(format!("live {title}: it stopped after {frames} frames in {ran} s: {why}"));
                live.again(run, frames > 0 && ran >= 10);
            }
        });
        let (pane, sending) = (self.pane.clone(), slot.clone());
        thread::spawn(move || send(&pane, &sending));
        Ok(Capture { plan, tools, slot })
    }

    /// A capture ended on its own (or didn't start): try again, longer apart each time it happens in a
    /// row (1 s, doubling to 30), unless it ran a good while first.
    fn again(&self, run: u64, ran_well: bool) {
        let wait = {
            let mut state = lock(&self.state);
            if state.run != run {
                return;
            }
            state.failures = if ran_well { 1 } else { state.failures + 1 };
            if let Some(gone) = state.capture.take() {
                gone.stop();
            }
            Duration::from_secs(1u64 << state.failures.saturating_sub(1).min(5)).min(Duration::from_secs(30))
        };
        let me = self.me.clone();
        thread::spawn(move || {
            thread::sleep(wait);
            let Some(live) = me.upgrade() else { return };
            if lock(&live.state).run != run {
                return; // something else has started it, or stopped it, since
            }
            live.apply();
        });
    }
}

/// JPEGs out of the tool's stdout into the slot, each newest replacing the last, until it ends or the
/// capture is stopped. How many came.
fn read(mut out: impl Read, slot: &Slot) -> u64 {
    let mut splitter = Splitter::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut count = 0;
    while !slot.over.load(Ordering::SeqCst) {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for jpeg in splitter.push(&buf[..n]) {
            let Some((width, height)) = crate::jpeg::size(&jpeg) else { continue };
            count += 1;
            *lock(&slot.frame) = Some(Frame { jpeg, width, height, captured_ms: now_ms() });
            slot.ready.notify_all();
        }
    }
    count
}

/// The last few KiB of what a tool says, kept for when it stops on its own.
fn keep_log(stderr: Option<impl Read + Send + 'static>) -> Arc<Mutex<String>> {
    let log = Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = stderr {
        let log = log.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut log = lock(&log);
                log.push_str(&String::from_utf8_lossy(&buf[..n]));
                if log.len() > 8192 {
                    let cut = log.len() - 4096;
                    let cut = (cut..log.len()).find(|i| log.is_char_boundary(*i)).unwrap_or(0);
                    log.drain(..cut);
                }
            }
        });
    }
    log
}

/// The newest frame to the pane each time, until the capture's over.
fn send(pane: &Pane, slot: &Slot) {
    loop {
        let frame = {
            let mut frame = lock(&slot.frame);
            loop {
                if slot.over.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(f) = frame.take() {
                    break f;
                }
                frame = slot.ready.wait_timeout(frame, Duration::from_millis(500)).unwrap_or_else(|e| e.into_inner()).0;
            }
        };
        pane.frame(&frame.jpeg, frame.width, frame.height, frame.captured_ms);
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::Display;

    const SETTINGS: Settings = Settings { fps: 10, width: 1920, quality: 5 };

    #[test]
    fn a_stream_only_while_watched_and_never_past_the_settings() {
        assert_eq!(wanted(SETTINGS, 0, 10, 1920), None);
        assert_eq!(wanted(SETTINGS, 1, 0, 1920), None); // paused
        assert_eq!(wanted(SETTINGS, 2, 10, 1920), Some(Want { fps: 10, width: 1920 }));
        assert_eq!(wanted(SETTINGS, 1, 30, 4000), Some(Want { fps: 10, width: 1920 }));
        assert_eq!(wanted(SETTINGS, 1, 5, 700), Some(Want { fps: 5, width: 960 }));
        assert_eq!(wanted(SETTINGS, 1, 10, 641), wanted(SETTINGS, 1, 10, 900)); // no restart for a few pixels
    }

    #[test]
    fn it_opens_at_the_settings_width_in_its_targets_shape() {
        let display = Display {
            id: "0".into(),
            name: "Built-in display".into(),
            bounds: Rect::new(0.0, 0.0, 1512.0, 982.0),
            pixels: (3024, 1964),
            main: true,
            output: String::new(),
        };
        let whole = Resolved {
            whole: display.bounds,
            area: display.bounds,
            title: "Built-in display".into(),
            window: None,
            display: display.clone(),
        };
        assert_eq!(
            opening(&whole, SETTINGS),
            json!({"title": "Built-in display", "video": {"codec": "mjpeg", "width": 1920, "height": 1246, "fps": 10}})
        );
        // a small window, 400 by 300 points: still 1920 across, so a hint may ask for that much once the
        // pane's pointed at the whole display
        let small = Resolved { area: Rect::new(10.0, 10.0, 400.0, 300.0), title: "Notes".into(), ..whole };
        assert_eq!(
            opening(&small, SETTINGS)["video"],
            json!({"codec": "mjpeg", "width": 1920, "height": 1440, "fps": 10})
        );
    }

    #[test]
    fn the_slot_keeps_only_the_newest() {
        let jpeg = |n: u8| {
            let mut j = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x30, 0x00, 0x40, 0x01, 0x01, 0x11, 0x00];
            j.extend([0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, n, 0xff, 0xd9]);
            j
        };
        let stream: Vec<u8> = (1..=5).flat_map(jpeg).collect();
        let slot = Slot::default();
        assert_eq!(read(&stream[..], &slot), 5);
        let newest = lock(&slot.frame).take().unwrap();
        assert_eq!((newest.width, newest.height, newest.jpeg), (64, 48, jpeg(5)));
    }
}
