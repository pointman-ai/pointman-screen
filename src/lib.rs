//! screen: a machine's display, app or window as a live pane that agents can open and steer, and stills
//! of it (Pointman's docs/dev/video.md). `bin/screen` is the extension its node runs ([`serve`]); the
//! rest is how it finds what's on the screen ([`screens`], [`mac`], [`x11`], [`hypr`]) and captures it
//! ([`backend`], [`ffmpeg`], [`sck`], [`live`]).

pub mod backend;
pub mod ffmpeg;
pub mod geom;
pub mod hypr;
pub mod jpeg;
pub mod live;
pub mod mac;
pub mod proc;
pub mod sck;
pub mod screens;
pub mod serve;
pub mod x11;
