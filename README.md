# screen

A Pointman extension: a node's display, one app or one window, with the pointer, as a live pane that
agents can open and steer, and stills of it for agents to look at. "Take me through your render" works:
the agent drives Blender with its own tools while the pane follows Blender's window. The spec for the pane
is Pointman's docs/dev/video.md; it's built like [cam](https://github.com/pointman-ai/pointman-cam).

One Rust binary, `bin/screen`, built for each platform and written with the
[pointman-extension](https://github.com/pointman-ai/pointman-extension) crate.

## Where it works

| Desktop | Lists | Captures | Needs |
|---|---|---|---|
| macOS | CGGetActiveDisplayList, CGWindowList (front to back, in points) | ScreenCaptureKit: a window on its own (its own pixels, even when covered), or a display, with the pointer; ffmpeg makes the MJPEG | Screen Recording for the node, once |
| Hyprland (Wayland) | `hyprctl monitors -j`, `hyprctl clients -j` (in layout pixels) | wf-recorder (live) and grim (stills), by wlroots' screencopy | nothing: no prompt |
| X11 | xrandr's monitors, the window manager's clients (xprop), their places (xwininfo) | ffmpeg's x11grab with `-draw_mouse 1`, the window's own pixels | nothing |
| GNOME, KDE on Wayland | | not yet | the desktop portal, which asks someone at the screen |

- **macOS, how:** a capture is screen itself, `screen frames window|display <id> <width> <height> <fps>
  [x y w h]`, which streams ScreenCaptureKit's raw BGRA frames (already scaled to that size) to ffmpeg's
  stdin. A window's target uses the desktop-independent window filter, so it shows that window's own
  pixels wherever it is and whatever covers it, Pointman's own window included; a display's target is
  the display with every window on it. A region is ScreenCaptureKit's source rectangle, in points. A
  window that only moves keeps its capture; one that changes size starts it again at the new size.
  (ffmpeg's avfoundation screens aren't used: on macOS 26 a second capture hangs while one runs.)
- **macOS, permission:** the node needs Screen Recording (System Settings, Privacy & Security, Screen & System Audio
  Recording). Without it, CGWindowList leaves out other apps' titles and a capture shows only the desktop,
  so screen checks first (CGPreflightScreenCaptureAccess), says where to allow it and for which program
  (the node, by its path), and asks macOS once a run (CGRequestScreenCaptureAccess), which shows macOS's
  prompt the first time. After allowing it, restart the node.
- **Hyprland:** the node may run as a systemd user unit without `WAYLAND_DISPLAY` or
  `HYPRLAND_INSTANCE_SIGNATURE`; screen finds both in the runtime folder (`/run/user/<uid>/wayland-N`,
  `hypr/<instance>`). Install `wf-recorder` and `grim` (nix's packages of those names). A window on a
  workspace no monitor shows isn't listed: its place on the screen shows something else.
- **Other Wayland desktops** (GNOME, KDE): a program may record the screen only through
  xdg-desktop-portal's ScreenCast over PipeWire, which asks someone at the screen to allow it: once, if
  the desktop keeps a restore token (portal version 4 and up), else every time. screen says so rather than
  half-working. XWayland's x11grab sees only X11 windows, so it isn't used there.

## Targets

What to show, for a still, the pane and `screen.show`:

| Target | Shows |
|---|---|
| none, `{}` | the main display |
| `{"display": "1"}` | a display: its id, `main`, or any part of its name |
| `{"app": "Blender"}` | an app's frontmost window: its whole name, else the start of one, else any part, any case |
| `{"window": "388"}` | a window: its id, or its title or any part of it, the frontmost first |
| `"blender"` | a bare name (a slash command's): an app, else a window's title, else a display |

Any of them takes `region: {x, y, width, height}`, from the target's top-left in `screen.list`'s units
(points on macOS, pixels on X11, layout pixels on Hyprland): a zoom into part of it. A still's answer says
how many of its pixels a unit is (`scale`), so a region can be read off a still.

On macOS a window's target is that window's own pixels, even when another window covers it. On X11 and
Hyprland it's cut out of its display, so whatever covers it shows too, as it does on the screen
(Hyprland's tiling rarely overlaps windows).

## Commands

Run on a machine like any extension's: `/screen list`, `/screen still blender`, `/screen show blender` in a
thread, or `run_on_machine` for agents. What they show answers only whoever asked: a list or a still is
the request's result, and a still is posted only where its `thread_id` says. Nothing about the screen is
ever an event.

| Kind | Params | What |
|---|---|---|
| `screen.list` | | `{displays: [{id, name, width, height, x, y, main}], windows: [{id, app, title, display, x, y, width, height}], units, notes}`, windows front to back |
| `screen.still` | `target?`, `region?`, `width?`, `thread_id?` | One JPEG, with the pointer, and `{name, width, height, area, units, scale}` |
| `screen.show` | `target`, `region?`, `pane?` | Points the open live pane (that one, or every one on the machine) at `target`, without closing it |

For agents: `run_on_machine` lists only each command's title and params (a summary is the "/" menu's),
so `screen.list`'s and `screen.still`'s answers end with how to show it live and steer it:

```
open_pane(extension="screen", pane="live", target={"app": "Blender"})        # on their device
run_on_machine("screen.show", {"target": {"app": "Blender", "region": {"x": 0, "y": 0, "width": 800, "height": 600}}})
run_on_machine("screen.still", {"target": {"app": "Blender"}})                # to look first
```

## The live pane

`live` is a `video` pane (`opens = ["menu", "command"]`): the frames as JPEGs, sealed by the node and sent
straight to the app that opened it.

- It's captured only while a path is attached (`pane.viewers` above 0), never otherwise.
- It opens 1920 across (the `width` setting) in its target's shape, so the app's hints may ask for that
  much whatever it's pointed at later. The frames carry their own size.
- The app's hints set its rate and width (`max_fps: 0` pauses it), within the settings; a width is rounded
  up to 320, 480, 640, 960, 1280, 1920, 2560 or 3840.
- **Following:** while an app's or a window's pane is captured, the window is found again once a second
  (CGWindowList, hyprctl, or one `xwininfo -id`). Resized, the capture starts again at its new size; moved,
  it does too on X11 and Hyprland, and on macOS it needn't (a window's own pixels are where it is). Gone,
  the target is looked for afresh (an app's next window); with nothing there, the pane keeps the place it
  had and the log says so.
- `screen.show` changes the target and the pane's title, and starts the capture again at once if someone's
  watching.
- If the capture stops on its own, it's tried again 1 s later, doubling to 30 s.

## Settings

Per machine, in `~/.config/pointman/extensions/screen/config.toml`:

| Setting | Default | |
|---|---|---|
| `fps` | 10 | Frames a second, for the live pane |
| `width` | 1920 | The most pixels across, for stills and the live pane |
| `quality` | 5 | ffmpeg's `-q:v` for stills and the live pane (lower is better and bigger); Hyprland's live pane is wf-recorder's own MJPEG |

## Development

`cargo test`. The tests run screen under the SDK's stand-in node with fakes on a PATH of their own
(tests/common): the Mac's displays and windows from a file (`SCREEN_FAKE_LIST`, as CGWindowList would
give them), fake `xrandr`, `xprop` and `xwininfo`, a fake `hyprctl`, fake `ffmpeg`, `wf-recorder` and
`grim` that send a fixture frame, and a fake of screen's own ScreenCaptureKit capture (`SCREEN_FRAMES`,
black frames at the size and rate asked, with perl); `SCREEN_BACKEND` picks macOS, X11 or Hyprland on any
machine. The test pattern tests run the real ffmpeg on those raw frames, and on X11's screen swapped for
its testsrc2, so the sizes are real (skipped without ffmpeg). No test captures a real screen, except
`tests/real_screen.rs`, by hand only, which prints sizes, counts and whether two stills are the same, and
deletes them. CI checks `cargo fmt` and `cargo clippy` too,
on macOS and Ubuntu.

A release is a tag that's extension.toml's version: .github/workflows/release.yml builds the four
platforms and attaches them, as cam's does.
