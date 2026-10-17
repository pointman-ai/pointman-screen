//! screen with the real ffmpeg, its screen swapped for ffmpeg's test pattern (testsrc2) at a display's
//! size: the crops and scales are ffmpeg's own, so a window's still and live frames come out the size
//! the crop maths says. The displays and windows are the fake list's; no real screen is ever captured.
//! Skipped where there's no ffmpeg.

mod common;

use std::time::{Duration, Instant};

use common::{mac_list, real_ffmpeg, Backend, Capture, Screen, Tools};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(20);

fn pattern(backend: Backend, width: u32, height: u32) -> Option<Screen> {
    if real_ffmpeg().is_none() {
        eprintln!("skipped: no ffmpeg on this machine");
        return None;
    }
    Some(Screen::start(Tools { backend, capture: Capture::Pattern(width, height), env: &[] }, json!({})))
}

fn size(done: &pointman_extension::testing::Done) -> (u64, u64) {
    let jpeg = std::fs::read(&done.outputs[0].path).unwrap();
    assert!(jpeg.starts_with(&[0xff, 0xd8]) && jpeg.ends_with(&[0xff, 0xd9]), "a JPEG");
    assert_eq!(done.result["bytes"], jpeg.len());
    (done.result["width"].as_u64().unwrap(), done.result["height"].as_u64().unwrap())
}

#[test]
fn a_windows_still_is_its_part_of_a_retina_display() {
    // Display 2: 2560×1440 points, 5120×2880 pixels; Blender's window is half of it each way
    let Some(screen) = pattern(Backend::Mac, 5120, 2880) else { return };
    let done = screen.node.run("screen.still", json!({"target": {"app": "Blender"}})).unwrap();
    assert_eq!(size(&done), (1920, 1080), "2560×1440 pixels of it, at most 1920 across");
    let done = screen.node.run("screen.still", json!({"target": {"app": "Blender"}, "width": 4000})).unwrap();
    assert_eq!(size(&done), (2560, 1440), "never wider than it is");
}

#[test]
fn an_odd_window_comes_out_the_size_the_crop_maths_says() {
    let Some(screen) = pattern(Backend::Mac, 3024, 1964) else { return };
    let mut list = mac_list();
    list["windows"][0] =
        json!({"id": 410, "app": "Ghostty", "title": "~", "x": 0, "y": 0, "width": 501, "height": 333});
    screen.write_mac(&list);
    let done = screen.node.run("screen.still", json!({"target": {"app": "ghostty"}})).unwrap();
    // geom's unit test: 501 of 1512 points is 0.331349 of 3024 pixels, which ffmpeg cuts to 1000
    assert_eq!(size(&done), (1000, 666));
    let done = screen.node.run("screen.still", json!({})).unwrap();
    assert_eq!(size(&done), (1920, 1246), "the whole display, as geom::scaled says");
}

#[test]
fn the_live_pattern_follows_its_hints() {
    let Some(screen) = pattern(Backend::Mac, 5120, 2880) else { return };
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    assert_eq!(view.video(), json!({"codec": "mjpeg", "width": 1920, "height": 1080, "fps": 10}));
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 5).unwrap();
    let frame = view.video_frames().pop().unwrap();
    assert_eq!((frame.width, frame.height), (1920, 1080));
    let (from, start) = (view.frame_count(), Instant::now());
    std::thread::sleep(Duration::from_secs(2));
    let rate = (view.frame_count() - from) as f64 / start.elapsed().as_secs_f64();
    assert!((7.0..=13.0).contains(&rate), "{rate} frames a second at 10");

    view.hint(5, 640).unwrap();
    view.wait(WAIT, |v| v.video_frames().last().is_some_and(|f| f.width == 640)).unwrap();
    assert_eq!(view.video_frames().last().unwrap().height, 360);
    let (from, start) = (view.frame_count(), Instant::now());
    std::thread::sleep(Duration::from_secs(2));
    let rate = (view.frame_count() - from) as f64 / start.elapsed().as_secs_f64();
    assert!((3.0..=7.0).contains(&rate), "{rate} frames a second at 5");

    // zoomed into a quarter of the window: 1280×720 pixels of it
    let region = json!({"x": 320, "y": 180, "width": 640, "height": 360});
    screen.node.run("screen.show", json!({"target": {"app": "Blender"}, "region": region})).unwrap();
    view.hint(10, 1920).unwrap();
    view.wait(WAIT, |v| v.video_frames().last().is_some_and(|f| (f.width, f.height) == (1280, 720))).unwrap();
    view.close("closed").unwrap();
    assert!(view.problems().is_empty(), "{:?}", view.problems());
}

#[test]
fn an_x11_window_is_grabbed_at_its_own_size() {
    let Some(screen) = pattern(Backend::X11, 1920, 1080) else { return };
    let done = screen.node.run("screen.still", json!({"target": {"app": "blender"}, "width": 1280})).unwrap();
    assert_eq!(size(&done), (1280, 720));
}
