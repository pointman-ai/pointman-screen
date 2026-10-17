//! The crop maths: a window's or region's rectangle, in the screen's own coordinates (points on macOS,
//! as CGWindowList and CGDisplayBounds give them; pixels on X11; layout pixels on Hyprland), cut to the
//! display it's on, and what that is in the capture's own pixels, which may be more (a Retina display
//! has two a point, a Hyprland monitor at scale 1.6 has 1.6).
//!
//! A crop is kept as fractions of its display, so ffmpeg can cut it from frames of any size (`iw`,
//! `ih`): nothing depends on knowing the display's pixels exactly.

use serde_json::{json, Value};

/// A rectangle in a screen's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn area(&self) -> f64 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    /// Where it overlaps `other`, if it does by more than a line.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let (x, y) = (self.x.max(other.x), self.y.max(other.y));
        let (right, bottom) = (self.right().min(other.right()), self.bottom().min(other.bottom()));
        (right - x >= 1.0 && bottom - y >= 1.0).then(|| Rect::new(x, y, right - x, bottom - y))
    }

    /// `region`, given from this rectangle's top-left, in the screen's coordinates and cut to this.
    pub fn within(&self, region: &Rect) -> Option<Rect> {
        Rect::new(self.x + region.x, self.y + region.y, region.width, region.height).intersect(self)
    }

    /// `{x, y, width, height}`, rounded to whole units.
    pub fn to_json(self) -> Value {
        json!({"x": self.x.round() as i64, "y": self.y.round() as i64,
               "width": self.width.round() as i64, "height": self.height.round() as i64})
    }

    /// One from `{x, y, width, height}`, each a number, the size above 0.
    pub fn from_json(v: &Value) -> Option<Rect> {
        let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite());
        let r = Rect::new(n("x").unwrap_or(0.0), n("y").unwrap_or(0.0), n("width")?, n("height")?);
        (r.width > 0.0 && r.height > 0.0).then_some(r)
    }

    /// Whole pixels for a tool that takes them (x11grab, wf-recorder's and grim's geometry): its corner
    /// rounded in, its size down to even, at least 2 by 2.
    pub fn pixels(&self) -> (i64, i64, u32, u32) {
        let (x, y) = (self.x.ceil(), self.y.ceil());
        let even = |n: f64| ((n.floor() as i64 / 2) * 2).max(2) as u32;
        (x as i64, y as i64, even(self.right().floor() - x), even(self.bottom().floor() - y))
    }
}

/// What the capture cuts from a display's frames, as fractions of it: 0 to 1 each.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Crop {
    pub const WHOLE: Crop = Crop { x: 0.0, y: 0.0, width: 1.0, height: 1.0 };

    /// `area` (on `display`, in the same coordinates) as fractions of it. What's off the display is
    /// cut away first.
    pub fn of(display: &Rect, area: &Rect) -> Crop {
        let Some(a) = area.intersect(display) else { return Crop::WHOLE };
        // to the 6 places the filter writes, so its size here is the size ffmpeg cuts
        let f = |v: f64, of: f64| format!("{:.6}", (v / of).clamp(0.0, 1.0)).parse().unwrap_or(0.0);
        Crop {
            x: f(a.x - display.x, display.width),
            y: f(a.y - display.y, display.height),
            width: f(a.width, display.width),
            height: f(a.height, display.height),
        }
    }

    pub fn is_whole(&self) -> bool {
        self.x <= 1e-6 && self.y <= 1e-6 && self.width >= 1.0 - 1e-6 && self.height >= 1.0 - 1e-6
    }

    /// ffmpeg's crop filter for it, in the frames' own pixels and even: none for the whole display.
    pub fn filter(&self) -> Option<String> {
        if self.is_whole() {
            return None;
        }
        Some(format!(
            "crop=w=trunc(iw*{:.6}/2)*2:h=trunc(ih*{:.6}/2)*2:x=trunc(iw*{:.6}):y=trunc(ih*{:.6})",
            self.width, self.height, self.x, self.y
        ))
    }

    /// Its size in frames `pixels` big, as ffmpeg's [`filter`](Self::filter) cuts it.
    pub fn size_in(&self, pixels: (u32, u32)) -> (u32, u32) {
        if self.is_whole() {
            return pixels;
        }
        let cut = |of: u32, f: f64| (((f64::from(of) * f) / 2.0).trunc() as u32 * 2).max(2);
        (cut(pixels.0, self.width), cut(pixels.1, self.height))
    }
}

/// The size a frame `size` big comes out at most `width` across: what ffmpeg's
/// `scale=w=trunc(min(W\,iw)/2)*2:h=-2` makes of it (never wider, both even, the height rounded as
/// ffmpeg rounds it).
pub fn scaled(size: (u32, u32), width: u32) -> (u32, u32) {
    let (iw, ih) = (u64::from(size.0.max(1)), u64::from(size.1.max(1)));
    let w = (u64::from(width).min(iw) / 2 * 2).max(2);
    // av_rescale(w, ih, iw * 2) * 2: to the nearest, halves away from zero
    let h = ((w * ih + iw) / (iw * 2)).max(1) * 2;
    (w as u32, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect::new(0.0, 0.0, 1512.0, 982.0);
    const SIDE: Rect = Rect::new(1512.0, -200.0, 2560.0, 1440.0);

    #[test]
    fn a_window_is_cut_to_its_display() {
        let window = Rect::new(1400.0, 100.0, 400.0, 300.0);
        assert_eq!(window.intersect(&MAIN), Some(Rect::new(1400.0, 100.0, 112.0, 300.0)));
        assert_eq!(window.intersect(&SIDE), Some(Rect::new(1512.0, 100.0, 288.0, 300.0)));
        assert_eq!(Rect::new(-50.0, -50.0, 40.0, 40.0).intersect(&MAIN), None);
        assert_eq!(Rect::new(0.0, 0.0, 0.5, 10.0).intersect(&MAIN), None, "less than a line");
    }

    #[test]
    fn a_region_is_from_its_targets_top_left() {
        let window = Rect::new(100.0, 50.0, 800.0, 600.0);
        assert_eq!(window.within(&Rect::new(10.0, 20.0, 300.0, 200.0)), Some(Rect::new(110.0, 70.0, 300.0, 200.0)));
        // past its edge: cut to it
        assert_eq!(window.within(&Rect::new(700.0, 500.0, 300.0, 300.0)), Some(Rect::new(800.0, 550.0, 100.0, 100.0)));
        assert_eq!(window.within(&Rect::new(900.0, 0.0, 10.0, 10.0)), None);
    }

    #[test]
    fn a_crop_is_fractions_of_its_display_and_cuts_even_pixels() {
        // a window on the side display, which sits 200 points higher than the main one
        let c = Crop::of(&SIDE, &Rect::new(2152.0, 160.0, 1280.0, 720.0));
        assert_eq!(c, Crop { x: 0.25, y: 0.25, width: 0.5, height: 0.5 });
        assert_eq!(
            c.filter().unwrap(),
            "crop=w=trunc(iw*0.500000/2)*2:h=trunc(ih*0.500000/2)*2:x=trunc(iw*0.250000):y=trunc(ih*0.250000)"
        );
        // its pixels, on a display at 1 and at 2 a point
        assert_eq!(c.size_in((2560, 1440)), (1280, 720));
        assert_eq!(c.size_in((5120, 2880)), (2560, 1440));
        // an odd size comes out even, as ffmpeg's trunc(…/2)*2 does with the fractions it's given
        // (501 / 1512 is 0.331349, a hair under: 1000, not 1002)
        let odd = Crop::of(&MAIN, &Rect::new(0.0, 0.0, 501.0, 333.0));
        assert_eq!((odd.width, odd.height), (0.331349, 0.339104));
        assert_eq!(odd.size_in((3024, 1964)), (1000, 666));
        assert_eq!(odd.size_in((1512, 982)), (500, 332));
        // the whole display, or more: no crop
        assert_eq!(Crop::of(&MAIN, &Rect::new(-10.0, -10.0, 2000.0, 2000.0)), Crop::WHOLE);
        assert_eq!(Crop::WHOLE.filter(), None);
        assert_eq!(Crop::WHOLE.size_in((3024, 1964)), (3024, 1964));
    }

    #[test]
    fn it_scales_as_ffmpeg_does() {
        assert_eq!(scaled((3024, 1964), 1920), (1920, 1246));
        assert_eq!(scaled((1280, 720), 1920), (1280, 720), "never wider than it is");
        assert_eq!(scaled((1001, 667), 1920), (1000, 666));
        assert_eq!(scaled((3840, 2160), 1280), (1280, 720));
        assert_eq!(scaled((6720, 1892), 1920), (1920, 540));
    }

    #[test]
    fn whole_pixels_round_in_and_stay_even() {
        assert_eq!(Rect::new(10.4, 20.0, 801.0, 600.6).pixels(), (11, 20, 800, 600));
        assert_eq!(Rect::new(0.0, 0.0, 1.0, 1.0).pixels(), (0, 0, 2, 2));
    }

    #[test]
    fn a_rect_from_json_needs_a_size() {
        assert_eq!(
            Rect::from_json(&json!({"x": 10, "y": 20.5, "width": 300, "height": 200})),
            Some(Rect::new(10.0, 20.5, 300.0, 200.0))
        );
        assert_eq!(Rect::from_json(&json!({"width": 300, "height": 200})), Some(Rect::new(0.0, 0.0, 300.0, 200.0)));
        assert_eq!(Rect::from_json(&json!({"x": 1, "y": 1, "width": 0, "height": 2})), None);
        assert_eq!(Rect::from_json(&json!({"x": 1, "y": 1})), None);
        assert_eq!(Rect::new(1.4, 2.6, 300.2, 200.0).to_json(), json!({"x": 1, "y": 3, "width": 300, "height": 200}));
    }
}
