//! The capture tools' command lines: ffmpeg's avfoundation (macOS, `Capture screen N` with the
//! pointer, cropped to a window or region) and x11grab (X11, the window's or region's pixels
//! straight), wf-recorder for Hyprland's live pane and grim for its stills (both wlroots' screencopy,
//! by layout pixels), each scaled to a width and sent on as MJPEG or one JPEG.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Result};

use crate::geom::Crop;
use crate::hypr::Session;
use crate::proc;
use crate::screens::Resolved;

/// ffmpeg with what screen always wants: no banner, no keyboard.
pub fn command() -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin"]);
    cmd
}

/// The filters after a capture: at `fps` (none: every frame), cropped (`crop`, none: all of it), then at
/// most `width` across (never wider than it is, and even).
pub fn filters(fps: Option<u32>, crop: Option<String>, width: u32) -> String {
    let mut f: Vec<String> = Vec::new();
    if let Some(fps) = fps {
        f.push(format!("fps={fps}"));
    }
    f.extend(crop);
    f.push(format!("scale=w=trunc(min({width}\\,iw)/2)*2:h=-2"));
    f.join(",")
}

/// avfoundation's input for a display, with the pointer: `Capture screen <id>`, at `fps`.
pub fn avfoundation(r: &Resolved, fps: u32) -> Vec<String> {
    let screen = format!("Capture screen {}", r.display.id);
    [
        "-f",
        "avfoundation",
        "-capture_cursor",
        "1",
        "-pixel_format",
        "nv12",
        "-framerate",
        &fps.to_string(),
        "-i",
        &screen,
    ]
    .map(String::from)
    .to_vec()
}

/// x11grab's input for the target's own pixels, with the pointer.
pub fn x11grab(r: &Resolved, display: &str, fps: u32) -> Vec<String> {
    let (x, y, w, h) = r.area.pixels();
    let size = format!("{w}x{h}");
    let at = format!("{display}+{x},{y}");
    ["-f", "x11grab", "-draw_mouse", "1", "-framerate", &fps.to_string(), "-video_size", &size, "-i", &at]
        .map(String::from)
        .to_vec()
}

/// What of the display a macOS capture keeps: the window or region, as fractions of it.
fn crop(r: &Resolved) -> Option<String> {
    Crop::of(&r.display.bounds, &r.area).filter()
}

/// The live pane's ffmpeg: `input` as MJPEG on stdout, at most `width` across at `fps`.
pub fn live(input: Vec<String>, crop: Option<String>, fps: u32, width: u32, quality: u32) -> Command {
    let mut cmd = command();
    cmd.args(["-v", "error"]).args(input);
    cmd.args(["-an", "-vf", &filters(Some(fps), crop, width), "-c:v", "mjpeg", "-q:v", &quality.to_string()]);
    cmd.args(["-f", "mjpeg", "pipe:1"]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// The live pane's capture on a Mac.
pub fn live_mac(r: &Resolved, fps: u32, width: u32, quality: u32) -> Command {
    live(avfoundation(r, fps), crop(r), fps, width, quality)
}

/// … on X11.
pub fn live_x11(r: &Resolved, display: &str, fps: u32, width: u32, quality: u32) -> Command {
    live(x11grab(r, display, fps), None, fps, width, quality)
}

/// wf-recorder's place for a target: the whole output by its name, else its layout pixels.
fn region(r: &Resolved) -> Vec<String> {
    if Crop::of(&r.display.bounds, &r.area).is_whole() && !r.display.output.is_empty() {
        return vec!["-o".into(), r.display.output.clone()];
    }
    let (x, y, w, h) = r.area.pixels();
    vec!["-g".into(), format!("{x},{y} {w}x{h}")]
}

/// … on Hyprland: wf-recorder's own MJPEG (its encoder's quality; it takes no -q:v), every frame even
/// when nothing moved (`-D`), so a new viewer sees a picture at once.
pub fn live_hypr(r: &Resolved, session: &Session, fps: u32, width: u32) -> Command {
    let mut cmd = session.command("wf-recorder");
    cmd.args(["-y", "-D", "-r", &fps.to_string(), "-c", "mjpeg", "-x", "yuvj420p", "-m", "mjpeg"]);
    cmd.args(["-F", &filters(Some(fps), None, width)]).args(region(r));
    cmd.args(["-f", "pipe:1"]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// One JPEG of `input` (cropped), at most `width` across, into `out`. Its size, read from the file.
pub fn still(
    input: Vec<String>,
    crop: Option<String>,
    width: u32,
    quality: u32,
    out: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(u16, u16)> {
    let mut cmd = command();
    cmd.args(["-v", "error"]).args(input);
    cmd.args(["-an", "-vf", &filters(None, crop, width), "-frames:v", "1", "-q:v", &quality.to_string()]);
    cmd.args(["-f", "image2", "-update", "1", "-y"]).arg(out);
    let ran = proc::run(&mut cmd, Duration::from_secs(30), cancelled)?;
    let jpeg = std::fs::read(out).unwrap_or_default();
    match crate::jpeg::size(&jpeg) {
        Some(size) if ran.status.success() => Ok(size),
        _ => bail!("no still: {}", explain(&ran.stderr)),
    }
}

pub fn still_mac(
    r: &Resolved,
    width: u32,
    quality: u32,
    out: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(u16, u16)> {
    still(avfoundation(r, 30), crop(r), width, quality, out, cancelled)
}

pub fn still_x11(
    r: &Resolved,
    display: &str,
    width: u32,
    quality: u32,
    out: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(u16, u16)> {
    still(x11grab(r, display, 30), None, width, quality, out, cancelled)
}

/// grim's frame (with the pointer) as a PPM beside `out`, then ffmpeg makes it the JPEG.
pub fn still_hypr(
    r: &Resolved,
    session: &Session,
    width: u32,
    quality: u32,
    out: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(u16, u16)> {
    let grabbed = out.with_extension("ppm");
    let mut grim = session.command("grim");
    grim.args(["-c", "-t", "ppm"]).args(region(r)).arg(&grabbed);
    let ran = proc::run(&mut grim, Duration::from_secs(30), cancelled)?;
    if !ran.status.success() {
        let _ = std::fs::remove_file(&grabbed);
        bail!("grim gave no still: {}", explain(&ran.stderr));
    }
    let input = ["-i".to_string(), grabbed.to_string_lossy().into_owned()].to_vec();
    let made = still(input, None, width, quality, out, cancelled);
    let _ = std::fs::remove_file(&grabbed);
    made
}

/// A capture tool's error, with what to do about the usual ones.
pub fn explain(log: &str) -> String {
    let why = proc::tail(log, 3);
    if log.contains("Cannot open display") || log.contains("Can't open display") {
        format!("can't reach the X display: is the desktop session up, and is it this user's? ({why})")
    } else if log.contains("screencopy") || log.contains("compositor doesn't support") {
        format!("the compositor doesn't offer wlroots' screencopy, which Hyprland and Sway do ({why})")
    } else if log.contains("outside the screen") {
        format!("that area is off the screen ({why})")
    } else {
        why
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::screens::{Display, Resolved};

    fn on(display: Display, area: Rect) -> Resolved {
        Resolved { whole: area, area, title: "t".into(), window: None, display }
    }

    fn display(id: &str, bounds: Rect, pixels: (u32, u32), output: &str) -> Display {
        Display { id: id.into(), name: "d".into(), bounds, pixels, main: true, output: output.into() }
    }

    fn args(cmd: &Command) -> String {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn a_mac_window_is_cropped_from_its_display_with_the_pointer() {
        let d = display("1", Rect::new(1512.0, 0.0, 2560.0, 1440.0), (5120, 2880), "");
        let cmd = live_mac(&on(d.clone(), Rect::new(2152.0, 360.0, 1280.0, 720.0)), 10, 1920, 5);
        let a = args(&cmd);
        assert!(
            a.contains("-f avfoundation -capture_cursor 1 -pixel_format nv12 -framerate 10 -i Capture screen 1"),
            "{a}"
        );
        assert!(
            a.contains(
                "-vf fps=10,crop=w=trunc(iw*0.500000/2)*2:h=trunc(ih*0.500000/2)*2:x=trunc(iw*0.250000):y=trunc(ih*0.250000),scale=w=trunc(min(1920\\,iw)/2)*2:h=-2"
            ),
            "{a}"
        );
        assert!(a.ends_with("-c:v mjpeg -q:v 5 -f mjpeg pipe:1"), "{a}");
        // the whole display: no crop
        let a = args(&live_mac(&on(d.clone(), d.bounds), 10, 1920, 5));
        assert!(a.contains("-vf fps=10,scale="), "{a}");
    }

    #[test]
    fn an_x11_window_is_grabbed_by_its_own_pixels() {
        let d = display("0", Rect::new(0.0, 0.0, 2560.0, 1440.0), (2560, 1440), "");
        let a = args(&live_x11(&on(d, Rect::new(602.0, 122.0, 1921.0, 1080.0)), ":0", 10, 1280, 5));
        assert!(a.contains("-f x11grab -draw_mouse 1 -framerate 10 -video_size 1920x1080 -i :0+602,122"), "{a}");
        assert!(a.contains("-vf fps=10,scale=w=trunc(min(1280\\,iw)/2)*2:h=-2"), "{a}");
    }

    #[test]
    fn hyprland_records_an_output_by_name_and_a_window_by_layout_pixels() {
        let session = Session { env: vec![("WAYLAND_DISPLAY".into(), "wayland-1".into())] };
        let d = display("0", Rect::new(0.0, 0.0, 2400.0, 1350.0), (3840, 2160), "eDP-1");
        let whole = live_hypr(&on(d.clone(), d.bounds), &session, 10, 1920);
        assert_eq!(whole.get_program(), "wf-recorder");
        assert_eq!(
            args(&whole),
            "-y -D -r 10 -c mjpeg -x yuvj420p -m mjpeg -F fps=10,scale=w=trunc(min(1920\\,iw)/2)*2:h=-2 -o eDP-1 -f pipe:1"
        );
        let a = args(&live_hypr(&on(d, Rect::new(74.0, 24.0, 1143.0, 1302.0)), &session, 10, 1920));
        assert!(a.contains("-g 74,24 1142x1302 -f pipe:1"), "{a}");
        assert!(whole.get_envs().any(|(k, v)| k == "WAYLAND_DISPLAY" && v.is_some_and(|v| v == "wayland-1")));
    }

    #[test]
    fn the_usual_errors_say_what_to_do() {
        assert!(explain("[x11grab @ 0x1] Cannot open display :0, error 1.").starts_with("can't reach the X display"));
        assert!(explain("compositor doesn't support wlr-screencopy-unstable-v1")
            .starts_with("the compositor doesn't offer"));
        assert_eq!(explain(""), "it said nothing");
    }
}
