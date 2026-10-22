//! screen under a stand-in node, with fakes for the window lists and the capture tools: listing,
//! stills, the live pane capturing only while someone watches, an agent steering it, and following a
//! window as it moves, on macOS, X11 and Hyprland alike.

mod common;

use std::thread::sleep;
use std::time::Duration;

use common::{hypr_clients, mac_list, Backend, Capture, Screen, Tools};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(10);

fn fakes(backend: Backend) -> Tools {
    Tools { backend, capture: Capture::Fake, env: &[] }
}

fn mac() -> Screen {
    Screen::start(fakes(Backend::Mac), json!({}))
}

#[test]
fn it_lists_displays_and_windows_front_to_back_without_capturing() {
    let screen = mac();
    let done = screen.node.run("screen.list", json!({})).unwrap();
    assert_eq!(
        done.result["displays"],
        json!([{"id": "0", "name": "Built-in display", "width": 1512, "height": 982, "x": 0, "y": 0, "main": true},
               {"id": "1", "name": "Display 2", "width": 2560, "height": 1440, "x": 1512, "y": -200, "main": false}])
    );
    assert_eq!(
        done.result["windows"][1],
        json!({"id": "388", "app": "Blender", "title": "Untitled.blend", "display": "1", "x": 2152, "y": 160, "width": 1280, "height": 720})
    );
    assert_eq!(done.result["units"], "points");
    let text = done.result["text"].as_str().unwrap();
    assert!(
        text.starts_with("Displays on mac-m4 (in points):\n- **0** Built-in display, 1512×982, the main one\n"),
        "{text}"
    );
    assert!(
        text.contains("Windows, front to back:\n- **410** Ghostty: ~/Documents, 900×600 at 40,60 on 0\n"),
        "{text}"
    );
    // and how to show it live: an agent reads it here, since run_on_machine lists only titles and params
    assert!(
        text.ends_with(
            "To show it live on their device: open_pane(extension=\"screen\", pane=\"live\", target={\"app\": \
             \"Blender\"}). Then run_on_machine(\"screen.show\", {\"target\": …}, machine=\"mac-m4\") points it at \
             another display, app or window, with a region to zoom in, while they watch."
        ),
        "{text}"
    );
    // nothing captured, nothing posted, and nothing about the screen as an event
    assert_eq!(screen.count("ffmpeg"), 0, "{:?}", screen.calls());
    assert!(done.outputs.is_empty());
    assert_eq!(screen.node.events(), json!({}));
}

#[test]
fn a_still_of_an_app_is_its_frontmost_windows_own_frame() {
    let screen = Screen::start(fakes(Backend::Mac), json!({"quality": 3}));
    let done = screen.node.run("screen.still", json!({"target": {"app": "blender"}, "thread_id": 11576})).unwrap();
    assert_eq!(done.result["name"], "Blender: Untitled.blend");
    assert_eq!((done.result["display"].clone(), done.result["window"].clone()), (json!("1"), json!("388")));
    assert_eq!(done.result["area"], json!({"x": 2152, "y": 160, "width": 1280, "height": 720}));
    let out = &done.outputs[0];
    assert_eq!((out.kind.as_str(), out.role.as_str()), ("image", "main"));
    assert_eq!(out.path.file_name().unwrap(), "Blender- Untitled.blend.jpg");
    assert_eq!(out.meta["screen"], "Blender: Untitled.blend");
    // the window's own frame (ScreenCaptureKit), not its part of the display, which another window may cover:
    // 1280×720 points at 2 pixels a point, at most 1920 across
    assert_eq!(screen.nth("screen frames", 0), "screen frames window 388 1920 1080 30");
    let still = screen.call("-frames:v 1", 0);
    assert!(still.contains("-f rawvideo -pix_fmt bgra -video_size 1920x1080 -framerate 30 -i pipe:0"), "{still}");
    assert!(!still.contains("avfoundation") && !still.contains("crop="), "{still}");
    assert!(still.contains("scale=w=trunc(min(1920\\,iw)/2)*2:h=-2") && still.contains("-q:v 3"), "{still}");
    // the fixture is 640 across: half a pixel of it a point
    assert_eq!(done.result["scale"], 0.5);
    let text = done.result["text"].as_str().unwrap();
    assert!(
        text.contains("A region ({x, y, width, height}) is in points from its top-left. To show it live"),
        "{text}"
    );
}

#[test]
fn a_region_zooms_into_its_target_and_a_name_finds_what_it_names() {
    let screen = mac();
    let target = json!({"display": 0, "region": {"x": 756, "y": 0, "width": 756, "height": 491}});
    let done = screen.node.run("screen.still", json!({"target": target, "width": 640})).unwrap();
    assert_eq!(done.result["name"], "Built-in display (a region)");
    // the display's right half, by its CoreGraphics number (1, the fake list's first), from its top-left
    // in points, at 2 pixels a point and at most 640 across
    assert_eq!(screen.nth("screen frames", 0), "screen frames display 1 640 416 30 756 0 756 491");
    let still = screen.call("-frames:v 1", 0);
    assert!(
        still.contains("-f rawvideo -pix_fmt bgra -video_size 640x416") && still.contains("min(640\\,iw)"),
        "{still}"
    );
    // a slash command's bare name: an app, else a window's title
    let done = screen.node.run("screen.still", json!({"target": "preferences"})).unwrap();
    assert_eq!(done.result["window"], "390");
    let e = screen.node.run("screen.still", json!({"target": {"app": "resolve"}})).unwrap_err().message;
    assert_eq!(e, "no app 'resolve' with a window on mac-m4: it has Ghostty, Blender");
}

#[test]
fn without_screen_recording_it_says_where_to_allow_it_and_captures_nothing() {
    let screen = Screen::start(Tools { env: &[("SCREEN_FAKE_ALLOWED", "0")], ..fakes(Backend::Mac) }, json!({}));
    let e = screen.node.run("screen.still", json!({})).unwrap_err().message;
    assert!(e.starts_with("macOS hasn't allowed the node to record the screen, so it shows no window titles"), "{e}");
    assert!(
        e.contains("On this Mac: System Settings, Privacy & Security, Screen & System Audio Recording, turn on "),
        "{e}"
    );
    assert!(e.ends_with("then restart the node (pointman restart)"), "{e}");
    let e = screen.node.open_pane("live", json!({"app": "Blender"})).err().unwrap().message;
    assert!(e.starts_with("macOS hasn't allowed"), "{e}");
    // the list says so too (macOS leaves the titles out then)
    let list = screen.node.run("screen.list", json!({})).unwrap();
    assert!(list.result["notes"][0].as_str().unwrap().contains("Screen & System Audio Recording"));
    assert_eq!(screen.count("ffmpeg") + screen.count("screen frames"), 0, "{:?}", screen.calls());
}

#[test]
fn the_live_pane_captures_only_while_someone_watches() {
    let screen = mac();
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    assert_eq!(view.format().as_deref(), Some("video"));
    assert_eq!(view.title().as_deref(), Some("Blender: Untitled.blend"));
    // 1920 across (the setting) in the window's shape, 16:9
    assert_eq!(view.video(), json!({"codec": "mjpeg", "width": 1920, "height": 1080, "fps": 10}));
    sleep(Duration::from_millis(300));
    let captured = screen.count("ffmpeg") + screen.count("screen frames");
    assert_eq!(captured, 0, "nothing captured with no one watching: {:?}", screen.calls());

    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 5).unwrap();
    let frame = view.video_frames().pop().unwrap();
    assert_eq!((frame.width, frame.height), (640, 424));
    assert_eq!(frame.jpeg, std::fs::read(common::fixture("frame.jpg")).unwrap());
    // Blender's own window, its frames fed to ffmpeg
    assert_eq!(screen.nth("screen frames", 0), "screen frames window 388 1920 1080 10");
    let live = screen.call("-f mjpeg pipe:1", 0);
    assert!(live.contains("-f rawvideo -pix_fmt bgra -video_size 1920x1080 -framerate 10 -i pipe:0"), "{live}");
    assert!(live.contains("-vf fps=10,scale=") && live.contains("min(1920\\,iw)") && live.contains("-q:v 5"), "{live}");

    // a smaller view: again at its rate and width; a second path changes nothing
    view.hint(5, 600).unwrap();
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
    screen.wait_calls("ffmpeg stopped", 1, WAIT);
    assert!(screen.call("-f mjpeg pipe:1", 1).contains("fps=5,scale="), "{}", screen.call("-f mjpeg pipe:1", 1));
    assert_eq!(screen.nth("screen frames", 1), "screen frames window 388 640 360 5");
    view.viewers(2).unwrap();
    view.hint(5, 640).unwrap();
    sleep(Duration::from_millis(300));
    assert_eq!(screen.count("-f mjpeg pipe:1"), 2);

    // paused, then nobody's watching: it stops each time
    view.hint(0, 640).unwrap();
    screen.wait_calls("ffmpeg stopped", 2, WAIT);
    view.hint(10, 1920).unwrap();
    screen.wait_calls("-f mjpeg pipe:1", 3, WAIT);
    view.viewers(0).unwrap();
    screen.wait_calls("ffmpeg stopped", 3, WAIT);
    sleep(Duration::from_millis(300));
    let stopped = view.frame_count();
    sleep(Duration::from_millis(300));
    assert_eq!(view.frame_count(), stopped);
    assert!(view.problems().is_empty(), "{:?}", view.problems());
}

#[test]
fn an_agent_points_the_open_pane_elsewhere_without_closing_it() {
    let screen = mac();
    // nothing to steer yet: it says how to open one
    let e = screen.node.run("screen.show", json!({"target": {"app": "Blender"}})).unwrap_err().message;
    assert!(
        e.starts_with("no live screen pane is open on mac-m4: open one first, with open_pane(extension=\"screen\""),
        "{e}"
    );

    let view = screen.node.open_pane("live", Value::Null).unwrap();
    assert_eq!(view.title().as_deref(), Some("Built-in display"));
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    // the whole display, by its CoreGraphics number: 3024 × 1964 pixels at most 1920 across
    assert_eq!(screen.nth("screen frames", 0), "screen frames display 1 1920 1246 10");

    let done = screen.node.run("screen.show", json!({"target": {"app": "Blender"}})).unwrap();
    assert_eq!(done.result["text"], "The live pane on mac-m4 shows Blender: Untitled.blend now");
    assert_eq!(done.result["panes"], json!([{"pane": view.id(), "title": "Blender: Untitled.blend"}]));
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
    view.wait(WAIT, |v| v.title().as_deref() == Some("Blender: Untitled.blend")).unwrap();
    assert_eq!(screen.nth("screen frames", 1), "screen frames window 388 1920 1080 10");

    // zoomed into part of it, named by the pane's id
    let region = json!({"x": 0, "y": 0, "width": 640, "height": 360});
    let done =
        screen.node.run("screen.show", json!({"target": {"window": "untitled"}, "region": region, "pane": view.id()}));
    assert_eq!(done.unwrap().result["title"], "Blender: Untitled.blend (a region)");
    screen.wait_calls("-f mjpeg pipe:1", 3, WAIT);
    // that part of the window, from its top-left in points, at 2 pixels a point
    assert_eq!(screen.nth("screen frames", 2), "screen frames window 388 1280 720 10 0 0 640 360");
    let before = view.frame_count();
    view.wait(WAIT, |v| v.frame_count() > before + 2).unwrap();
    assert!(view.closed().is_none(), "still open");

    let e = screen.node.run("screen.show", json!({"target": "blender", "pane": "pane_99"})).unwrap_err().message;
    assert_eq!(e, format!("no live pane pane_99 is open on mac-m4: {} is", view.id()));
    let e = screen.node.run("screen.show", json!({"target": {"app": "Resolve"}})).unwrap_err().message;
    assert!(e.starts_with("no app 'Resolve' with a window on mac-m4"), "{e}");
    assert!(view.problems().is_empty(), "{:?}", view.problems());
}

#[test]
fn the_pane_follows_a_window_that_moves_or_closes() {
    let screen = mac();
    let view = screen.node.open_pane("live", json!({"window": 388})).unwrap();
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    sleep(Duration::from_millis(1500));
    assert_eq!(screen.count("-f mjpeg pipe:1"), 1, "it didn't move: no restart");

    // moved, the same size: a window's own frames don't care where it is, so nothing restarts
    let mut list = mac_list();
    list["windows"][1] = json!({"id": 388, "app": "Blender", "title": "Untitled.blend", "x": 1700, "y": 0, "width": 1280, "height": 720});
    screen.write_mac(&list);
    sleep(Duration::from_millis(2500));
    assert_eq!(screen.count("-f mjpeg pipe:1"), 1, "{:?}", screen.calls());

    // dragged to the main display, and another size: its capture starts again at its new size
    list["windows"][1] =
        json!({"id": 388, "app": "Blender", "title": "Untitled.blend", "x": 0, "y": 0, "width": 756, "height": 491});
    screen.write_mac(&list);
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
    assert_eq!(screen.nth("screen frames", 1), "screen frames window 388 1512 982 10");

    // closed: a window by id has nothing to fall back on, so the pane stays where it was
    let mut list = mac_list();
    list["windows"].as_array_mut().unwrap().remove(1);
    screen.write_mac(&list);
    sleep(Duration::from_millis(2500));
    assert_eq!(screen.count("-f mjpeg pipe:1"), 2, "{:?}", screen.calls());
    assert!(screen.node.log().contains("it's gone, so the pane stays where it was"), "{}", screen.node.log());
    let before = view.frame_count();
    view.wait(WAIT, |v| v.frame_count() > before).unwrap();
}

#[test]
fn a_window_capture_that_fails_says_why() {
    let screen = Screen::start(Tools { env: &[("SCREEN_CAPTURE_GONE", "1")], ..fakes(Backend::Mac) }, json!({}));
    let e = screen.node.run("screen.still", json!({"target": {"window": 388}})).unwrap_err().message;
    assert!(e.contains("window 388 isn't there to capture: closed, minimised or on another Space"), "{e}");
    let view = screen.node.open_pane("live", json!({"window": 388})).unwrap();
    view.viewers(1).unwrap();
    screen.wait_calls("screen frames", 3, WAIT);
    let log = screen.node.log();
    assert!(log.contains("window 388 isn't there to capture"), "{log}");
}

#[test]
fn an_apps_pane_moves_to_its_next_window_when_one_closes() {
    let screen = mac();
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    let mut list = mac_list();
    list["windows"].as_array_mut().unwrap().remove(1);
    screen.write_mac(&list);
    view.wait(WAIT, |v| v.title().as_deref() == Some("Blender: Preferences")).unwrap();
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
}

#[test]
fn a_capture_stops_when_the_node_shuts_the_extension_down() {
    let screen = mac();
    let view = screen.node.open_pane("live", Value::Null).unwrap();
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    // still watched, but the node stops the extension (a restart, an upgrade): ffmpeg goes with it
    screen.node.stop();
    screen.wait_calls("ffmpeg stopped", 1, WAIT);
}

#[test]
fn a_capture_that_stops_is_tried_again() {
    // the fake screen goes away after 3 frames each time
    let screen = Screen::start(Tools { env: &[("SCREEN_DIE_AFTER", "3")], ..fakes(Backend::Mac) }, json!({}));
    let view = screen.node.open_pane("live", Value::Null).unwrap();
    view.viewers(1).unwrap();
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
    view.wait(WAIT, |v| v.frame_count() >= 4).unwrap();
    assert!(screen.node.log().contains("it stopped after 3 frames"), "{}", screen.node.log());
}

#[test]
fn closing_the_pane_stops_the_capture_and_it_takes_no_taps() {
    let screen = mac();
    let view = screen.node.open_pane("live", Value::Null).unwrap();
    let e = view.input(json!({"kind": "tap", "block": "x"})).unwrap_err().message;
    assert_eq!(
        e,
        "the screen pane takes hints, and pointer, scroll, key and text while someone controls it: steer it with \
         screen.show"
    );
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    view.close("no_viewer").unwrap();
    screen.wait_calls("ffmpeg stopped", 1, WAIT);
    assert_eq!(screen.count("-f mjpeg pipe:1"), 1);
    let e = screen.node.run("screen.show", json!({"target": "blender"})).unwrap_err().message;
    assert!(e.starts_with("no live screen pane is open"), "{e}");
}

#[test]
fn bad_targets_say_what_a_target_is() {
    let screen = mac();
    let e = screen.node.run("screen.still", json!({"target": {"screen": 1}})).unwrap_err().message;
    assert!(e.starts_with("a target is {display}, {app} or {window}"), "{e}");
    let e = screen.node.open_pane("live", json!({"display": "lg"})).err().unwrap().message;
    assert_eq!(e, "no display 'lg' on mac-m4: it has 0 Built-in display, 1 Display 2");
}

// ------------------------------------------------------------------ X11

#[test]
fn x11_lists_the_window_managers_windows_and_grabs_their_pixels() {
    let screen = Screen::start(fakes(Backend::X11), json!({}));
    let done = screen.node.run("screen.list", json!({})).unwrap();
    assert_eq!(done.result["units"], "pixels");
    assert_eq!(
        done.result["displays"][1],
        json!({"id": "1", "name": "HDMI-1", "width": 1920, "height": 1080, "x": 2560, "y": 0, "main": false})
    );
    let windows: Vec<(&str, &str)> = done.result["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["id"].as_str().unwrap(), w["app"].as_str().unwrap()))
        .collect();
    // front to back, the minimised Gimp left out
    assert_eq!(windows, [("0x2400003", "Blender"), ("0x1e00007", "XTerm")]);
    assert_eq!(done.result["windows"][1]["display"], "1");

    let done = screen.node.run("screen.still", json!({"target": {"app": "blender"}})).unwrap();
    assert_eq!(done.result["name"], "Blender: Blender* [~/loft.blend]");
    let still = screen.call("-frames:v 1", 0);
    assert!(still.contains("-f x11grab -draw_mouse 1 -framerate 30 -video_size 1920x1080 -i :0+602,122"), "{still}");
    assert!(!still.contains("crop="), "x11grab grabs the window's own pixels: {still}");
}

#[test]
fn x11s_pane_follows_its_window_with_xwininfo() {
    let screen = Screen::start(fakes(Backend::X11), json!({}));
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 2).unwrap();
    assert!(screen.call("-f mjpeg pipe:1", 0).contains("-video_size 1920x1080 -i :0+602,122"));
    screen.write_x_window(100, 50, 1280, 720);
    screen.wait_calls("-f mjpeg pipe:1", 2, WAIT);
    assert!(screen.call("-f mjpeg pipe:1", 1).contains("-video_size 1280x720 -i :0+100,50"), "{:?}", screen.calls());
    // found again with one xwininfo a second, not the whole list
    assert!(screen.count("xwininfo -id 0x2400003") >= 1);
    assert_eq!(screen.count("xwininfo -root -tree"), 1, "{:?}", screen.calls());
}

// ------------------------------------------------------------------ Hyprland

#[test]
fn hyprland_lists_whats_shown_and_records_with_wf_recorder() {
    let screen = Screen::start(fakes(Backend::Hyprland), json!({}));
    let done = screen.node.run("screen.list", json!({})).unwrap();
    assert_eq!(done.result["units"], "layout pixels");
    assert_eq!(done.result["displays"][0]["name"], "eDP-1 (AU Optronics 0x97A2)");
    assert_eq!(
        (done.result["displays"][0]["width"].clone(), done.result["displays"][0]["height"].clone()),
        (json!(2400), json!(1350))
    );
    let apps: Vec<&str> =
        done.result["windows"].as_array().unwrap().iter().map(|w| w["app"].as_str().unwrap()).collect();
    assert_eq!(
        apps,
        ["blender", "com.mitchellh.ghostty"],
        "most recently focused first; Firefox's workspace isn't shown"
    );
    // the session found in the runtime folder, as a systemd unit's node has to
    assert!(
        screen.call("hyprctl monitors -j", 0).contains("HYPRLAND_INSTANCE_SIGNATURE=abc_123"),
        "{:?}",
        screen.calls()
    );

    let view = screen.node.open_pane("live", Value::Null).unwrap();
    assert_eq!(view.video(), json!({"codec": "mjpeg", "width": 1920, "height": 1080, "fps": 10}));
    view.viewers(1).unwrap();
    view.wait(WAIT, |v| v.frame_count() >= 3).unwrap();
    let whole = screen.call("wf-recorder", 0);
    assert_eq!(
        whole,
        "wf-recorder -y -D -r 10 -c mjpeg -x yuvj420p -m mjpeg -F fps=10,scale=w=trunc(min(1920\\,iw)/2)*2:h=-2 -o eDP-1 -f pipe:1 (WAYLAND_DISPLAY=wayland-1)"
    );
    screen.node.run("screen.show", json!({"target": {"app": "Blender"}})).unwrap();
    screen.wait_calls("wf-recorder stopped", 1, WAIT);
    screen.wait_calls("wf-recorder -y", 2, WAIT);
    assert!(screen.call("wf-recorder -y", 1).contains("-g 600,300 1200x800 -f pipe:1"), "{:?}", screen.calls());

    // it moves: the pane follows
    screen.write_hypr(&hypr_clients([40, 60]));
    screen.wait_calls("wf-recorder -y", 3, WAIT);
    assert!(screen.call("wf-recorder -y", 2).contains("-g 40,60 1200x800"), "{:?}", screen.calls());
    view.viewers(0).unwrap();
    screen.wait_calls("wf-recorder stopped", 3, WAIT);
}

#[test]
fn a_hyprland_still_is_grims_with_the_pointer() {
    let screen = Screen::start(fakes(Backend::Hyprland), json!({}));
    let done = screen.node.run("screen.still", json!({"target": {"app": "ghostty"}, "width": 800})).unwrap();
    assert_eq!(done.result["name"], "com.mitchellh.ghostty: ~");
    let grim = screen.call("grim", 0);
    assert!(grim.starts_with("grim -c -t ppm -g 1233,24 1142x1302 "), "{grim}");
    let made = screen.call("-frames:v 1", 0);
    assert!(made.contains(".ppm -an -vf scale=w=trunc(min(800\\,iw)/2)*2:h=-2 -frames:v 1"), "{made}");
    let work = done.outputs[0].path.parent().unwrap();
    let left: Vec<_> = std::fs::read_dir(work).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.iter().all(|f| !f.to_string_lossy().ends_with(".ppm")), "the PPM is gone: {left:?}");
}

// ------------------------------------------------------------------ control

/// An hour from now, in unix seconds: a grant's `until`.
fn soon() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600
}

#[test]
fn a_person_with_control_clicks_where_the_picture_says() {
    let screen = mac();
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    let click = json!({"kind": "pointer", "x": 0.5, "y": 0.5, "phase": "down", "button": "left", "clicks": 1});
    // nothing before a grant: the node wouldn't pass it, and the extension refuses it too
    let e = view.control_input(click.clone()).unwrap_err();
    assert!(e.to_string().contains("control is off"), "{e}");
    assert!(screen.controls().is_empty());

    view.control(Some(soon()), "james", "MacBook Pro").unwrap();
    view.control_input(click).unwrap();
    // Blender's window is 1280×720 points at 2152,160: the middle of the picture is 2792,520
    assert_eq!(
        screen.controls(),
        vec![json!({"press": "left", "down": true, "at": [2792.0, 520.0], "clicks": 1, "flags": 0})]
    );
    view.control_input(json!({"kind": "key", "code": 0, "mods": ["cmd"], "phase": "down"})).unwrap();
    view.control_input(json!({"kind": "text", "text": "hi"})).unwrap();

    // ending it lets go of the button and the key still held
    view.control(None, "james", "MacBook Pro").unwrap();
    let posted = screen.controls();
    assert_eq!(posted[1], json!({"key": 0, "down": true, "flags": 0x0010_0000}));
    assert_eq!(posted[2], json!({"type": "hi"}));
    assert_eq!(posted[3], json!({"press": "left", "down": false, "at": [2792.0, 520.0], "clicks": 1, "flags": 0}));
    assert_eq!(posted[4], json!({"key": 0, "down": false, "flags": 0}));
    assert!(view.control_input(json!({"kind": "pointer", "x": 0.1, "y": 0.1})).is_err());
    // hints never needed control
    view.hint(5, 640).unwrap();
}

#[test]
fn a_mac_without_accessibility_turns_control_down_saying_where_to_allow_it() {
    let screen = Screen::start(
        Tools { backend: Backend::Mac, capture: Capture::Fake, env: &[("SCREEN_FAKE_TRUSTED", "0")] },
        json!({}),
    );
    let view = screen.node.open_pane("live", json!({"app": "Blender"})).unwrap();
    let e = view.control(Some(soon()), "james", "MacBook Pro").unwrap_err().to_string();
    assert!(e.starts_with("macOS hasn't allowed the node to control this Mac"), "{e}");
    assert!(e.contains("Privacy & Security, Accessibility"), "{e}");
    assert!(view.control_input(json!({"kind": "pointer", "x": 0.5, "y": 0.5})).is_err());
}

#[test]
fn control_is_only_for_macs_for_now() {
    let screen = Screen::start(fakes(Backend::Hyprland), json!({}));
    let view = screen.node.open_pane("live", Value::Null).unwrap();
    let e = view.control(Some(soon()), "james", "MacBook Pro").unwrap_err().to_string();
    assert_eq!(e, "control works on macOS only for now");
}
