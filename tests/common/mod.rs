//! What the tests share: screen under the SDK's stand-in node, with fakes of what it asks on a PATH of
//! their own, so no test lists or captures this machine's real screen: a Mac's displays and windows
//! from a file (SCREEN_FAKE_LIST, as CGWindowList would give them), X11's from fake xrandr, xprop and
//! xwininfo, Hyprland's from a fake hyprctl, and the captures from a fake ffmpeg, wf-recorder and grim
//! that send the fixture frame, and a fake of screen's own macOS capture (SCREEN_FRAMES) that sends
//! black frames. With `Capture::Pattern`, ffmpeg is the real one with the screen swapped
//! for its test pattern (testsrc2), so its crops and scales are real.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pointman_extension::testing::StandIn;
use serde_json::{json, Value};
use tempfile::TempDir;

pub const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// ffmpeg as far as screen needs it: live MJPEG and stills made of the fixture frame. Each call is a
/// line in $SCREEN_CALLS.
pub const FFMPEG: &str = r#"#!/bin/sh
echo "ffmpeg $*" >> "$SCREEN_CALLS"
for last; do :; done
# a capture's frames on stdin: none at all (it failed) is ffmpeg's end of input
fed() {
  case "$*" in
    *"-i pipe:0"*)
      if [ "$(dd bs=1 count=1 2>/dev/null | wc -c)" -eq 0 ]; then echo "[in#0 @ 0x1] Error opening input: End of file" >&2; exit 1; fi ;;
  esac
}
case "$*" in
  *"-f mjpeg pipe:1"*)
    trap 'echo "ffmpeg stopped" >> "$SCREEN_CALLS"; exit 255' INT TERM
    fed "$@"
    n=0
    while :; do
      cat "$SCREEN_FRAME" || exit 0  # nothing's reading: gone, as ffmpeg goes on a broken pipe
      n=$((n+1))
      if [ -n "$SCREEN_DIE_AFTER" ] && [ $n -ge "$SCREEN_DIE_AFTER" ]; then echo "[in#0] the capture went away" >&2; exit 1; fi
      sleep 0.05
    done ;;
  *"-frames:v 1"*)
    fed "$@"
    cp "$SCREEN_FRAME" "$last"
    exit 0 ;;
esac
echo "fake ffmpeg: nothing for $*" >&2
exit 1
"#;

/// The real ffmpeg ($REAL_FFMPEG), with X11's screen (x11grab's input, up to its `-i`) swapped for
/// testsrc2 at $SCREEN_PATTERN_SIZE, in real time as a screen sends it (`-re`): what's
/// after it, the crop and scale, is as asked.
pub const PATTERN: &str = r#"#!/bin/sh
echo "ffmpeg $*" >> "$SCREEN_CALLS"
n=$#; i=0; mode=keep
while [ $i -lt $n ]; do
  a=$1; shift; i=$((i+1))
  case "$mode" in
    drop) [ "$a" = "-i" ] && mode=value; continue ;;
    value) mode=keep; set -- "$@" -re -f lavfi -i "testsrc2=size=$SCREEN_PATTERN_SIZE:rate=30"; continue ;;
  esac
  if [ "$a" = "-f" ] && [ "$1" = x11grab ]; then mode=drop; continue; fi
  set -- "$@" "$a"
done
exec "$REAL_FFMPEG" "$@"
"#;

/// screen's own macOS capture (`screen frames window|display <id> <width> <height> <fps> [x y w h]`) as
/// far as the tests need it: black BGRA frames of that size at that rate, until nothing reads them. With
/// $SCREEN_CAPTURE_GONE it says what it was asked for has gone, as ScreenCaptureKit's would.
pub const FRAMES: &str = r#"#!/bin/sh
echo "screen $*" >> "$SCREEN_CALLS"
if [ -n "$SCREEN_CAPTURE_GONE" ]; then echo "screen: $2 $3 isn't there to capture: closed, minimised or on another Space" >&2; exit 1; fi
# at the rate asked, however long a frame takes to write, until nothing reads them
exec perl -MTime::HiRes=time,sleep -e '$| = 1; my $frame = "\0" x $ARGV[0]; my $next = time;
  while (1) { print $frame or exit 0; $next += 1 / $ARGV[1]; my $wait = $next - time; sleep $wait if $wait > 0 }' \
  $(($4 * $5 * 4)) "$6"
"#;

pub const WF_RECORDER: &str = r#"#!/bin/sh
echo "wf-recorder $* (WAYLAND_DISPLAY=$WAYLAND_DISPLAY)" >> "$SCREEN_CALLS"
trap 'echo "wf-recorder stopped" >> "$SCREEN_CALLS"; exit 0' INT TERM
while :; do cat "$SCREEN_FRAME" || exit 0; sleep 0.05; done
"#;

pub const GRIM: &str = r#"#!/bin/sh
echo "grim $*" >> "$SCREEN_CALLS"
for last; do :; done
printf 'P6\n2 2\n255\n............' > "$last"
"#;

pub const HYPRCTL: &str = r#"#!/bin/sh
echo "hyprctl $* (HYPRLAND_INSTANCE_SIGNATURE=$HYPRLAND_INSTANCE_SIGNATURE)" >> "$SCREEN_CALLS"
case "$1" in
  monitors) cat "$SCREEN_HYPR/monitors.json" ;;
  clients) cat "$SCREEN_HYPR/clients.json" ;;
  *) echo "unknown request"; exit 1 ;;
esac
"#;

pub const XRANDR: &str = r#"#!/bin/sh
echo "xrandr $* (DISPLAY=$DISPLAY)" >> "$SCREEN_CALLS"
cat "$SCREEN_X/monitors"
"#;

pub const XPROP: &str = r#"#!/bin/sh
echo "xprop $*" >> "$SCREEN_CALLS"
case "$1" in
  -root) cat "$SCREEN_X/stack" ;;
  -id)
    if grep -qx "$2" "$SCREEN_X/hidden" 2>/dev/null; then echo "_NET_WM_STATE(ATOM) = _NET_WM_STATE_HIDDEN"
    else echo "_NET_WM_STATE(ATOM) = "; echo "_NET_WM_WINDOW_TYPE(ATOM) = _NET_WM_WINDOW_TYPE_NORMAL"; fi ;;
esac
"#;

pub const XWININFO: &str = r#"#!/bin/sh
echo "xwininfo $*" >> "$SCREEN_CALLS"
case "$*" in
  "-root -tree") cat "$SCREEN_X/tree" ;;
  "-root") cat "$SCREEN_X/root" ;;
  -id*) if [ -f "$SCREEN_X/$2" ]; then cat "$SCREEN_X/$2"; else echo "xwininfo: error: No such window with id $2." >&2; exit 1; fi ;;
esac
"#;

/// A Mac with a Retina display and a 4K one to its right, a little higher, with windows front to back.
pub fn mac_list() -> Value {
    json!({
        "displays": [
            {"name": "Built-in display", "x": 0, "y": 0, "width": 1512, "height": 982, "pixel_width": 3024, "pixel_height": 1964, "main": true},
            {"name": "Display 2", "x": 1512, "y": -200, "width": 2560, "height": 1440, "pixel_width": 5120, "pixel_height": 2880}
        ],
        "windows": [
            {"id": 410, "app": "Ghostty", "title": "~/Documents", "x": 40, "y": 60, "width": 900, "height": 600},
            {"id": 388, "app": "Blender", "title": "Untitled.blend", "x": 2152, "y": 160, "width": 1280, "height": 720},
            {"id": 390, "app": "Blender", "title": "Preferences", "x": 2000, "y": 100, "width": 600, "height": 400}
        ]
    })
}

/// nixos's Hyprland (#extensions "Cam" 11576 [12331]), with Blender beside its terminals and a window on
/// a workspace that isn't shown.
pub fn hypr_monitors() -> Value {
    json!([{"id": 0, "name": "eDP-1", "description": "AU Optronics 0x97A2", "width": 3840, "height": 2160, "x": 0, "y": 0,
            "scale": 1.6, "transform": 0, "focused": true, "disabled": false,
            "activeWorkspace": {"id": 1, "name": "1"}, "specialWorkspace": {"id": 0, "name": ""}}])
}

pub fn hypr_clients(blender_at: [i64; 2]) -> Value {
    json!([
        {"address": "0x55d0a1", "class": "com.mitchellh.ghostty", "title": "~", "at": [1233, 24], "size": [1143, 1302],
         "monitor": 0, "workspace": {"id": 1, "name": "1"}, "mapped": true, "hidden": false, "focusHistoryID": 1},
        {"address": "0x55d0b7", "class": "blender", "title": "Blender [loft.blend]", "at": blender_at, "size": [1200, 800],
         "monitor": 0, "workspace": {"id": 1, "name": "1"}, "mapped": true, "hidden": false, "focusHistoryID": 0},
        {"address": "0x55d0c9", "class": "firefox", "title": "Mail", "at": [0, 0], "size": [2400, 1350],
         "monitor": 0, "workspace": {"id": 3, "name": "3"}, "mapped": true, "hidden": false, "focusHistoryID": 2}
    ])
}

/// An X11 desktop: two monitors, Blender in front of an xterm, and a minimised window.
pub const X_MONITORS: &str =
    "Monitors: 2\n 0: +*DP-1 2560/597x1440/336+0+0  DP-1\n 1: +HDMI-1 1920/527x1080/296+2560+0  HDMI-1\n";
pub const X_STACK: &str = "_NET_CLIENT_LIST_STACKING(WINDOW): window id # 0x1e00007, 0x3000001, 0x2400003\n";
pub const X_TREE: &str = r#"
xwininfo: Window id: 0x6c5 (the root window) (has no name)

  Root window id: 0x6c5 (the root window) (has no name)
  Parent window id: 0x0 (none)
     3 children:
     0x1a00003 (has no name): ()  1924x1104+600+100  +600+100
        1 child:
        0x2400003 "Blender* [~/loft.blend]": ("blender" "Blender")  1920x1080+2+22  +602+122
     0x1e00007 "xterm": ("xterm" "XTerm")  800x600+2600+40  +2600+40
     0x3000001 "Hidden": ("gimp" "Gimp")  800x600+0+0  +0+0
"#;

pub fn x_info(x: i64, y: i64, w: i64, h: i64) -> String {
    format!(
        "xwininfo: Window id: 0x2400003 \"Blender\"\n\n  Absolute upper-left X:  {x}\n  Absolute upper-left Y:  {y}\n  Width: {w}\n  Height: {h}\n  Map State: IsViewable\n"
    )
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(ROOT).join("tests/fixtures").join(name)
}

/// The real ffmpeg, if this machine has one.
pub fn real_ffmpeg() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("ffmpeg")).find(|p| p.is_file())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Mac,
    X11,
    Hyprland,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Fake,
    /// The real ffmpeg, its screen the test pattern at this size.
    Pattern(u32, u32),
}

pub struct Tools {
    pub backend: Backend,
    pub capture: Capture,
    /// More for the fakes' environment (SCREEN_DIE_AFTER, SCREEN_FAKE_ALLOWED).
    pub env: &'static [(&'static str, &'static str)],
}

/// screen with these fakes and settings, and where they say what they were asked.
pub struct Screen {
    pub node: StandIn,
    pub dir: TempDir,
}

impl Screen {
    pub fn start(tools: Tools, settings: Value) -> Screen {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = |name: &str, body: &str| {
            let path = bin.join(name);
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        script("ffmpeg", if matches!(tools.capture, Capture::Fake) { FFMPEG } else { PATTERN });
        for (name, body) in [
            ("frames", FRAMES),
            ("wf-recorder", WF_RECORDER),
            ("grim", GRIM),
            ("hyprctl", HYPRCTL),
            ("xrandr", XRANDR),
            ("xprop", XPROP),
            ("xwininfo", XWININFO),
        ] {
            script(name, body);
        }
        let d = dir.path();
        write_mac(d, &mac_list());
        write_hypr(d, &hypr_clients([600, 300]));
        let x = d.join("x");
        fs::create_dir_all(&x).unwrap();
        for (name, body) in [("monitors", X_MONITORS), ("stack", X_STACK), ("tree", X_TREE), ("hidden", "0x3000001\n")]
        {
            fs::write(x.join(name), body).unwrap();
        }
        write_x_window(d, 602, 122, 1920, 1080);
        fs::write(d.join("calls"), "").unwrap();
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let backend = match tools.backend {
            Backend::Mac => "macos",
            Backend::X11 => "x11",
            Backend::Hyprland => "hyprland",
        };
        // a Hyprland session in a runtime folder of its own, as a systemd unit's node would find it
        let runtime = d.join("run");
        fs::create_dir_all(runtime.join("hypr/abc_123")).unwrap();
        fs::write(runtime.join("hypr/abc_123/.socket.sock"), "").unwrap();
        fs::write(runtime.join("wayland-1"), "").unwrap();
        let mut node = StandIn::new(ROOT);
        for (key, value) in tools.env {
            node = node.env(key, value);
        }
        if let Capture::Pattern(w, h) = tools.capture {
            node = node
                .env("REAL_FFMPEG", real_ffmpeg().expect("a real ffmpeg"))
                .env("SCREEN_PATTERN_SIZE", format!("{w}x{h}"));
        }
        let node = node
            .binary(env!("CARGO_BIN_EXE_screen"))
            .settings(settings)
            .machine("mac-m4")
            .env("PATH", path)
            .env("SCREEN_BACKEND", backend)
            .env("SCREEN_CALLS", d.join("calls"))
            .env("SCREEN_FRAME", fixture("frame.jpg"))
            .env("SCREEN_FAKE_LIST", d.join("list.json"))
            // a control input is written here, never posted to the machine running the tests
            .env("SCREEN_FAKE_CONTROL", d.join("control"))
            .env("SCREEN_FRAMES", bin.join("frames"))
            .env("SCREEN_HYPR", d.join("hypr"))
            .env("SCREEN_X", d.join("x"))
            .env("DISPLAY", ":0")
            .env("XDG_RUNTIME_DIR", &runtime)
            // not this machine's own session, if it has one: only the fakes'
            .env("HYPRLAND_INSTANCE_SIGNATURE", "")
            .env("WAYLAND_DISPLAY", "")
            .start()
            .unwrap();
        Screen { node, dir }
    }

    /// The events control posted, in order, as SCREEN_FAKE_CONTROL writes them.
    pub fn controls(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.path().join("control"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The Mac's displays and windows, as CGWindowList would say them now.
    pub fn write_mac(&self, list: &Value) {
        write_mac(self.dir.path(), list);
    }

    /// Hyprland's windows.
    pub fn write_hypr(&self, clients: &Value) {
        write_hypr(self.dir.path(), clients);
    }

    /// Where X11's Blender window is, as xwininfo -id says.
    pub fn write_x_window(&self, x: i64, y: i64, w: i64, h: i64) {
        write_x_window(self.dir.path(), x, y, w, h);
    }

    /// What the fakes were asked, a line each.
    pub fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("calls")).unwrap().lines().map(String::from).collect()
    }

    pub fn count(&self, needle: &str) -> usize {
        self.calls().iter().filter(|c| c.contains(needle)).count()
    }

    /// The `n`th call (from 0) that has `needle`.
    pub fn call(&self, needle: &str, n: usize) -> String {
        self.calls().into_iter().filter(|c| c.contains(needle)).nth(n).unwrap_or_default()
    }

    /// The `n`th call (from 0) that has `needle`, once there is one: a tool fed by another may say what it
    /// was asked after the tool it feeds does.
    pub fn nth(&self, needle: &str, n: usize) -> String {
        self.wait_calls(needle, n + 1, Duration::from_secs(10));
        self.call(needle, n)
    }

    /// Wait until `needle` has been asked `n` times.
    pub fn wait_calls(&self, needle: &str, n: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.count(needle) < n {
            assert!(Instant::now() < deadline, "no {n} of '{needle}' in {:?}:\n{}", timeout, self.calls().join("\n"));
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A file written whole, so a fake never reads half of it.
fn put(path: &Path, body: &str) {
    fs::write(path.with_extension("tmp"), body).unwrap();
    fs::rename(path.with_extension("tmp"), path).unwrap();
}

fn write_mac(dir: &Path, list: &Value) {
    put(&dir.join("list.json"), &list.to_string());
}

fn write_hypr(dir: &Path, clients: &Value) {
    let hypr = dir.join("hypr");
    fs::create_dir_all(&hypr).unwrap();
    put(&hypr.join("monitors.json"), &hypr_monitors().to_string());
    put(&hypr.join("clients.json"), &clients.to_string());
}

fn write_x_window(dir: &Path, x: i64, y: i64, w: i64, h: i64) {
    put(&dir.join("x/0x2400003"), &x_info(x, y, w, h));
}
