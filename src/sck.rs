//! macOS capture through ScreenCaptureKit: a display, or one window on its own (the desktop-independent
//! window filter: its own pixels, with the pointer, whether other windows cover it or not, wherever it
//! is). Cutting a window out of its display would show whatever is in front of it (Pointman's own
//! window, say), and ffmpeg's avfoundation screens won't open a second capture while one runs on macOS
//! 26, so both go this way.
//!
//! A capture runs as screen itself, `screen frames window|display <id> <width> <height> <fps> [x y w h]`:
//! raw BGRA frames, `width × height` each, on stdout for ffmpeg to make MJPEG of, as any capture tool's.
//! ScreenCaptureKit scales each frame to that size itself. A process of its own keeps ScreenCaptureKit's
//! threads out of the extension, and it stops as any tool does. `SCREEN_FRAMES` names another program to
//! run instead: for tests.

use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::geom::{scaled, Rect};
use crate::screens::Resolved;

/// The subcommand screen runs itself with.
pub const SUBCOMMAND: &str = "frames";
pub const PROGRAM_ENV: &str = "SCREEN_FRAMES";

/// What's captured: a window by its CGWindowID, or a display by its CGDirectDisplayID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Window(u32),
    Display(u32),
}

/// What a capture is asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub source: Source,
    /// Each frame's size, in pixels.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Part of it, from its top-left in points; none, all of it.
    pub part: Option<Rect>,
}

impl Ask {
    /// A target's capture, at most `width` across: its window's own (or its display's), the region of it
    /// (or all of it), at its display's pixels a point, made even. None when it names neither a window
    /// nor a display ScreenCaptureKit knows by number.
    pub fn of(shown: &Resolved, width: u32, fps: u32) -> Option<Ask> {
        let (source, whole) = match &shown.window {
            Some(w) => (Source::Window(w.id.parse().ok()?), shown.whole),
            None => (Source::Display(shown.display.output.parse().ok()?), shown.display.bounds),
        };
        let local = local(shown);
        let part = (local != Rect::new(0.0, 0.0, whole.width, whole.height)).then_some(local);
        let s = shown.display.scale();
        let pixels = ((local.width * s).round().max(2.0) as u32, (local.height * s).round().max(2.0) as u32);
        let (width, height) = scaled(pixels, width);
        Some(Ask { source, width, height, fps: fps.max(1), part })
    }

    pub fn args(&self) -> Vec<String> {
        let (kind, id) = match self.source {
            Source::Window(id) => ("window", id),
            Source::Display(id) => ("display", id),
        };
        let mut args = [SUBCOMMAND, kind].map(String::from).to_vec();
        args.extend([id, self.width, self.height, self.fps].iter().map(u32::to_string));
        if let Some(r) = self.part {
            args.extend([r.x, r.y, r.width, r.height].iter().map(|n| format!("{n}")));
        }
        args
    }

    /// The subcommand's arguments, after its name.
    pub fn parse(args: &[String]) -> Result<Ask> {
        let usage =
            || format!("usage: screen {SUBCOMMAND} window|display <id> <width> <height> <fps> [x y width height]");
        let n = |i: usize, what: &str| -> Result<u32> {
            args.get(i).with_context(|| format!("no {what}"))?.parse().with_context(|| format!("{what} isn't a number"))
        };
        let id = n(1, "id")?;
        let source = match args.first().map(String::as_str) {
            Some("window") => Source::Window(id),
            Some("display") => Source::Display(id),
            _ => bail!("{}", usage()),
        };
        let (width, height, fps) = (n(2, "width")?, n(3, "height")?, n(4, "fps")?);
        if width < 2 || height < 2 || fps == 0 {
            bail!("a capture is at least 2×2 at 1 fps");
        }
        let part = match args.len() {
            5 => None,
            9 => {
                let f = |i: usize| -> Result<f64> {
                    args[i].parse().with_context(|| format!("{} isn't a number", args[i]))
                };
                Some(Rect::new(f(5)?, f(6)?, f(7)?, f(8)?))
            }
            _ => bail!("{}", usage()),
        };
        Ok(Ask { source, width, height, fps, part })
    }

    /// The capture's process: raw frames on stdout.
    pub fn command(&self) -> Command {
        let program = std::env::var_os(PROGRAM_ENV)
            .or_else(|| std::env::current_exe().ok().map(Into::into))
            .unwrap_or_else(|| "screen".into());
        let mut cmd = Command::new(program);
        cmd.args(self.args());
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }
}

/// What's shown, from its window's top-left (or its display's): its region, or all of it.
pub fn local(shown: &Resolved) -> Rect {
    let origin = if shown.window.is_some() { shown.whole } else { shown.display.bounds };
    let a = shown.area;
    Rect::new(a.x - origin.x, a.y - origin.y, a.width, a.height)
}

/// One frame of `width × height` from a pixel buffer's rows (`row` bytes apart, perhaps padded, perhaps
/// another size): packed, cut or filled with black to exactly that size.
pub fn pack(pixels: &[u8], row: usize, have: (usize, usize), width: usize, height: usize) -> Vec<u8> {
    let mut frame = vec![0u8; width * height * 4];
    let (cols, rows) = (have.0.min(width), have.1.min(height));
    for y in 0..rows {
        let from = y * row;
        let Some(line) = pixels.get(from..from + cols * 4) else { break };
        frame[y * width * 4..y * width * 4 + cols * 4].copy_from_slice(line);
    }
    frame
}

#[cfg(target_os = "macos")]
pub use kit::run;

/// The subcommand: only macOS has ScreenCaptureKit.
#[cfg(not(target_os = "macos"))]
pub fn run(_: &[String]) -> Result<()> {
    bail!("{SUBCOMMAND} is macOS's (ScreenCaptureKit)")
}

#[cfg(target_os = "macos")]
mod kit {
    //! ScreenCaptureKit through objc2: the window or display found among what may be shared, a stream of
    //! it on a queue of its own, and each frame copied out of its pixel buffer and written to stdout. A
    //! frame is written again each second while nothing new comes, so a viewer who comes later sees one.

    use std::io::Write;
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    use anyhow::{anyhow, bail, Result};
    use block2::RcBlock;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_media::{CMSampleBuffer, CMTime};
    use objc2_core_video::{
        kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
        CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSArray, NSError, NSObjectProtocol};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType,
    };

    use super::{pack, Ask, Source};

    struct Ivars {
        size: (usize, usize),
        frames: Mutex<mpsc::SyncSender<Vec<u8>>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "PointmanScreenFrames"]
        #[ivars = Ivars]
        struct Output;

        unsafe impl NSObjectProtocol for Output {}

        unsafe impl SCStreamOutput for Output {
            #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
            unsafe fn did_output(&self, _stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
                if kind != SCStreamOutputType::Screen {
                    return;
                }
                // SAFETY: a sample buffer ScreenCaptureKit handed us, alive for this call
                let Some(image) = (unsafe { buffer.image_buffer() }) else { return }; // nothing new
                                                                                      // SAFETY: locked for reading while its rows are copied, unlocked after
                unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) };
                let (w, h, row) = (
                    CVPixelBufferGetWidth(&image),
                    CVPixelBufferGetHeight(&image),
                    CVPixelBufferGetBytesPerRow(&image),
                );
                let base = CVPixelBufferGetBaseAddress(&image) as *const u8;
                let (width, height) = self.ivars().size;
                let frame = if base.is_null() {
                    None
                } else {
                    // SAFETY: `row × h` bytes from the locked base address are the buffer's
                    let pixels = unsafe { std::slice::from_raw_parts(base, row * h) };
                    Some(pack(pixels, row, (w, h), width, height))
                };
                // SAFETY: as above
                unsafe { CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) };
                if let Some(frame) = frame {
                    // newest first: a frame the writer has no room for is dropped, not queued
                    let _ = self.ivars().frames.lock().map(|f| f.try_send(frame));
                }
            }
        }
    );

    impl Output {
        fn new(size: (usize, usize), frames: mpsc::SyncSender<Vec<u8>>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { size, frames: Mutex::new(frames) });
            // SAFETY: NSObject's init, on a freshly allocated instance
            unsafe { msg_send![super(this), init] }
        }
    }

    /// An Objective-C error's words.
    fn words(e: *mut NSError) -> Option<String> {
        // SAFETY: an NSError handed to a completion handler, or null
        unsafe { Retained::retain(e) }.map(|e| e.localizedDescription().to_string())
    }

    /// The windows, displays and apps that may be shared now.
    fn shareable() -> Result<Retained<SCShareableContent>> {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: what the completion handler was given, or null
            let _ = tx.send((unsafe { Retained::retain(content) }, words(error)));
        });
        // SAFETY: the block lives until it's called (ScreenCaptureKit copies it)
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok((Some(content), _)) => Ok(content),
            Ok((None, why)) => bail!("ScreenCaptureKit lists nothing to capture: {}", why.unwrap_or_default()),
            Err(_) => bail!("ScreenCaptureKit didn't list what may be captured in 10 s"),
        }
    }

    /// The filter for what's asked: one window on its own, or a display with every window on it.
    fn filter(content: &SCShareableContent, source: Source) -> Result<Retained<SCContentFilter>> {
        // SAFETY: plain getters on what ScreenCaptureKit gave, and its own initialisers
        unsafe {
            match source {
                Source::Window(id) => {
                    let window = content.windows().iter().find(|w| w.windowID() == id).ok_or_else(|| {
                        anyhow!("window {id} isn't there to capture: closed, minimised or on another Space")
                    })?;
                    Ok(SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window))
                }
                Source::Display(id) => {
                    let display = content
                        .displays()
                        .iter()
                        .find(|d| d.displayID() == id)
                        .ok_or_else(|| anyhow!("display {id} isn't there to capture: unplugged, or asleep"))?;
                    let none = NSArray::new();
                    Ok(SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &none))
                }
            }
        }
    }

    /// Raw frames of a window or display on stdout until nothing reads them.
    pub fn run(args: &[String]) -> Result<()> {
        let ask = Ask::parse(args)?;
        // a process of its own has no connection to the window server until CoreGraphics makes one
        let _ = core_graphics::display::CGDisplay::main();
        let content = shareable()?;
        let filter = filter(&content, ask.source)?;
        let (tx, frames) = mpsc::sync_channel(1);
        let output = Output::new((ask.width as usize, ask.height as usize), tx);
        let queue = DispatchQueue::new("ai.pointman.screen.frames", None);
        // SAFETY: ScreenCaptureKit's own initialisers and setters, on objects made here
        let stream = unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(ask.width as usize);
            config.setHeight(ask.height as usize);
            config.setMinimumFrameInterval(CMTime::new(1, ask.fps as i32));
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setShowsCursor(true);
            config.setQueueDepth(3);
            if let Some(r) = ask.part {
                config.setSourceRect(CGRect::new(CGPoint::new(r.x, r.y), CGSize::new(r.width, r.height)));
            }
            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("ScreenCaptureKit took no output: {}", e.localizedDescription()))?;
            stream
        };
        let (tx, started) = mpsc::channel();
        let block = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(words(error));
        });
        // SAFETY: as above; the block lives until it's called
        unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
        match started.recv_timeout(Duration::from_secs(10)) {
            Ok(None) => {}
            Ok(Some(why)) => bail!("ScreenCaptureKit wouldn't capture it: {why}"),
            Err(_) => bail!("ScreenCaptureKit didn't start in 10 s"),
        }
        let mut out = std::io::stdout().lock();
        let mut last: Option<Vec<u8>> = None;
        loop {
            let frame = match frames.recv_timeout(Duration::from_secs(1)) {
                Ok(frame) => frame,
                Err(mpsc::RecvTimeoutError::Timeout) => match last.take() {
                    Some(again) => again,
                    None => continue,
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if out.write_all(&frame).and_then(|()| out.flush()).is_err() {
                break; // nothing reads it now: done
            }
            last = Some(frame);
        }
        // SAFETY: stopping the stream made here
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
        drop((output, queue));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::{Display, Window};

    fn display() -> Display {
        Display {
            id: "1".into(),
            name: "Display 2".into(),
            bounds: Rect::new(1512.0, -200.0, 2560.0, 1440.0),
            pixels: (5120, 2880),
            main: false,
            output: "69733632".into(),
        }
    }

    fn shown(region: Option<Rect>) -> Resolved {
        let whole = Rect::new(2152.0, 160.0, 1280.0, 720.0);
        let window =
            Window { id: "388".into(), app: "Blender".into(), title: "t".into(), display: "1".into(), bounds: whole };
        let area = region.map_or(whole, |r| whole.within(&r).unwrap());
        Resolved { display: display(), whole, area, title: "Blender".into(), window: Some(window) }
    }

    #[test]
    fn a_window_is_asked_for_at_its_displays_pixels_and_at_most_the_width() {
        let ask = Ask::of(&shown(None), 1920, 10).unwrap();
        assert_eq!(ask, Ask { source: Source::Window(388), width: 1920, height: 1080, fps: 10, part: None });
        assert_eq!(ask.args(), ["frames", "window", "388", "1920", "1080", "10"]);
        // a region: that part of it, from its top-left, in points
        let zoomed = Ask::of(&shown(Some(Rect::new(320.0, 180.0, 640.0, 360.0))), 1920, 10).unwrap();
        assert_eq!(zoomed.part, Some(Rect::new(320.0, 180.0, 640.0, 360.0)));
        assert_eq!((zoomed.width, zoomed.height), (1280, 720), "2 pixels a point, under 1920");
        assert_eq!(zoomed.args()[6..], ["320", "180", "640", "360"]);
    }

    #[test]
    fn a_display_is_asked_for_by_its_core_graphics_number() {
        let d = display();
        let whole =
            Resolved { whole: d.bounds, area: d.bounds, title: "Display 2".into(), window: None, display: d.clone() };
        let ask = Ask::of(&whole, 1920, 10).unwrap();
        assert_eq!(ask, Ask { source: Source::Display(69733632), width: 1920, height: 1080, fps: 10, part: None });
        // a region of it, from the display's top-left (it sits 200 points above the main one)
        let part = Resolved { area: Rect::new(1512.0 + 100.0, -200.0 + 50.0, 800.0, 450.0), ..whole.clone() };
        let ask = Ask::of(&part, 1920, 10).unwrap();
        assert_eq!(ask.part, Some(Rect::new(100.0, 50.0, 800.0, 450.0)));
        assert_eq!((ask.width, ask.height), (1600, 900));
        // a display with no number ScreenCaptureKit knows (another platform's): none
        let other = Resolved { display: Display { output: "eDP-1".into(), ..d }, ..whole };
        assert_eq!(Ask::of(&other, 1920, 10), None);
    }

    #[test]
    fn its_arguments_go_both_ways() {
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ask = Ask {
            source: Source::Window(7),
            width: 640,
            height: 360,
            fps: 5,
            part: Some(Rect::new(0.0, 10.5, 320.0, 180.0)),
        };
        assert_eq!(Ask::parse(&ask.args()[1..]).unwrap(), ask);
        let display = Ask { source: Source::Display(1), part: None, ..ask.clone() };
        assert_eq!(Ask::parse(&display.args()[1..]).unwrap(), display);
        let bad = |a: &[&str]| Ask::parse(&s(a)).unwrap_err().to_string();
        assert_eq!(bad(&["window", "7", "640"]), "no height");
        assert_eq!(bad(&["window", "7", "x", "360", "5"]), "width isn't a number");
        assert_eq!(bad(&["window", "7", "1", "360", "5"]), "a capture is at least 2×2 at 1 fps");
        assert!(bad(&["screen", "7", "640", "360", "5"]).starts_with("usage: screen frames window|display"));
        assert!(bad(&["window", "7", "640", "360", "5", "1"]).starts_with("usage: screen frames"));
    }

    #[test]
    fn a_frame_is_packed_to_exactly_its_size() {
        // 2×2 pixels, rows padded to 12 bytes
        let mut buffer = Vec::new();
        for y in 0..2u8 {
            for x in 0..2u8 {
                buffer.extend([x, y, 0, 255]);
            }
            buffer.extend([9; 4]); // padding
        }
        assert_eq!(pack(&buffer, 12, (2, 2), 2, 2), [0, 0, 0, 255, 1, 0, 0, 255, 0, 1, 0, 255, 1, 1, 0, 255]);
        // bigger than asked: cut; smaller: the rest black
        assert_eq!(pack(&buffer, 12, (2, 2), 1, 1), [0, 0, 0, 255]);
        assert_eq!(pack(&buffer, 12, (2, 2), 3, 1), [0, 0, 0, 255, 1, 0, 0, 255, 0, 0, 0, 0]);
    }
}
