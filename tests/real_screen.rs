//! This machine's real screen, by hand only (`cargo test --test real_screen -- --ignored --nocapture`):
//! the real list and the real ffmpeg, for counts and sizes only. Nothing of what's on the screen is
//! printed (no titles, no names), the still is deleted as soon as its size is read, and the live frames
//! are only ever in memory.

mod common;

use std::time::Duration;

use pointman_extension::testing::StandIn;
use serde_json::{json, Value};

#[test]
#[ignore = "this machine's real screen: run by hand"]
fn the_main_display_for_sizes_only() {
    assert!(common::real_ffmpeg().is_some(), "needs ffmpeg");
    let node = StandIn::new(common::ROOT)
        .binary(env!("CARGO_BIN_EXE_screen"))
        .settings(json!({}))
        .machine("this machine")
        .start()
        .unwrap();
    let list = node.run("screen.list", json!({})).unwrap();
    let sizes: Vec<Value> = list.result["displays"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| json!([d["id"], d["width"], d["height"], d["main"]]))
        .collect();
    let windows = list.result["windows"].as_array().unwrap();
    let titled = windows.iter().filter(|w| w["title"].as_str().is_some_and(|t| !t.is_empty())).count();
    println!("displays [id, width, height, main]: {sizes:?}; {} windows, {titled} with titles", windows.len());
    println!("notes: {}", list.result["notes"].as_array().unwrap().len());

    let still = node.run("screen.still", json!({})).unwrap();
    let path = &still.outputs[0].path;
    let bytes = std::fs::read(path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]));
    println!("still: {}×{}, {} bytes (deleted)", still.result["width"], still.result["height"], still.result["bytes"]);

    if let Some(front) = windows.first() {
        let still = node.run("screen.still", json!({"target": {"window": front["id"]}})).unwrap();
        std::fs::remove_file(&still.outputs[0].path).unwrap();
        println!(
            "the frontmost window's still: {}×{} of {}×{} {} (deleted)",
            still.result["width"], still.result["height"], front["width"], front["height"], list.result["units"]
        );
    }

    let view = node.open_pane("live", Value::Null).unwrap();
    println!("live opened: {}", view.video());
    view.viewers(1).unwrap();
    let started = std::time::Instant::now();
    view.wait(Duration::from_secs(20), |v| v.frame_count() >= 20).unwrap();
    let rate = 20.0 / started.elapsed().as_secs_f64();
    let last = view.video_frames().pop().unwrap();
    view.viewers(0).unwrap();
    view.close("closed").unwrap();
    println!(
        "live: {} frames, the last {}×{} of {} bytes, about {rate:.1} a second",
        view.frame_count(),
        last.width,
        last.height,
        last.jpeg.len()
    );
    assert!(view.problems().is_empty(), "{:?}", view.problems());
}
