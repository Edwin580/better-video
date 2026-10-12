# better-video

Lightweight screen recorder for developers. Record a clip, get a **small GIF or
MP4 you can drop straight into a PR body**, optionally with a camera that
**zooms in on your cursor** so reviewers can actually see what you clicked.

```sh
bv record                 # record until Enter / Ctrl+C → ./bv-2026-10-11_15-48-12.gif
bv record demo.mp4 -z     # MP4, zooms in around each click
bv record --zoom-on hover # zoom where the cursor rests (also click, follow)
bv record --clicks        # draw a ring wherever you click
bv record --style gradient # framed: background, rounded corners, shadow
bv record --cursor-size 2  # bigger pointer (drawn at export, smoothed)
bv record -c              # copy the result to the clipboard
bv record -s              # pick a screen or drag out an area first (Windows)
bv record -d 10 --region 1280x720+0+0 --width 960
bv hotkey --dir ~/Videos  # Ctrl+Alt+R starts/stops; each clip is copied
bv tray                   # same, from a menu bar / tray icon
```

## Quick start (Windows)

```sh
cargo install --path crates/bv-cli --features tray
bv tray
```

1. A notification confirms better-video is running. Its icon is in the
   taskbar tray, but Windows 11 hides new tray icons: if you don't see it
   (or a **^** leading to it), turn it on in **Settings › Personalization ›
   Taskbar › Other system tray icons**. The hotkey works either way.
2. **Click the icon** or press **Ctrl+Alt+R**. The screens dim: click a screen
   to record all of it, or drag around the part you want. Drag the handles to
   resize, or inside the selection to move it.
3. In the bar under your selection, pick **GIF** or **MP4**, a size
   (**Small** is 640 px at 10 fps, the right size for a PR), and optionally
   **Show clicks** and a **Zoom** mode, then **Record** (or press Enter).
   A 3-2-1 countdown starts.
4. A red frame shows what's being recorded (it doesn't appear in the video).
   Click **Stop** in the pill under it, click the tray icon, or press
   Ctrl+Alt+R again. **Discard** throws the take away.
5. A notification shows the file name and size. The file is saved in
   **Videos\better-video** and copied to the clipboard, ready to paste into a
   PR. Click the notification to show it in Explorer.

Right-click the tray icon to record the same area again, open the last
recording, see or change the save folder, or turn off clipboard copy. Your
choices are remembered in `%APPDATA%\better-video\settings.json`.

## Install (from source)

Requirements: Rust (stable).

- **Windows:** nothing else. Capture, GIF and MP4 are built in (MP4 uses
  Windows' own H.264 encoder). [`ffmpeg`](https://ffmpeg.org/download.html)
  is only needed for WebM, or for MP4 on Windows "N" editions without the
  Media Feature Pack. No admin rights, no PowerShell, no services.
- **macOS / Linux:** `ffmpeg` on your `PATH` (or point `BV_FFMPEG` /
  `BV_FFPROBE` at it). On Linux, cursor zoom needs X11 headers
  (`libx11-dev`), and `--copy` needs `xclip` (X11) or `wl-clipboard`
  (Wayland). The menu bar app on Linux needs `libgtk-3-dev`,
  `libayatana-appindicator3-dev` and `libxdo-dev`.

```sh
cargo install --path crates/bv-cli                  # CLI
cargo install --path crates/bv-cli --features tray  # CLI + menu bar app
bv doctor                 # what's available on this machine
```

## For scripts and AI agents

`bv` is built to be driven by tools as well as people:

```sh
bv record demo.mp4 --region 1000x600+100+80 --style gradient --zoom-on click --clicks --json &
# ...drive the app (real pointer events: xdotool, cliclick, SendInput)...
bv stop                    # stops it, waits for the export, prints the JSON result
bv record demo.mp4 --window "My App" -d 15 --json   # or a fixed length (Windows: by title)
```

- **One JSON line.** With `--json`, stdout is exactly one line:
  `{"path", "bytes", "format", "width", "height", "seconds"}`. Progress and
  notes always go to stderr.
- **Exit codes.** 0 saved, 1 error, 2 bad arguments, 3 cancelled or
  discarded by a person.
- **Real pointer only.** bv samples the OS pointer for the drawn cursor,
  click rings and zoom, so hold scripted clicks ~100–150 ms and move along
  a path; Playwright/CDP clicks don't move it. Headless Linux works with
  Xvfb, a window manager and a headed browser.
- **Claude skill.** `bv skill install` puts a skill in
  `~/.claude/skills/better-video/` (`bv skill show` prints it) that takes
  Claude Code from a storyboard through recording, a frame-by-frame check
  and attaching the MP4 to the pull request.

## Privacy and corporate use

better-video is local-only:

- **No network.** It never connects anywhere: no telemetry, no update
  checks, no uploads. Nothing in its dependencies does networking either.
  Sharing a clip is up to you (paste it, attach it, `gh` it).
- **What's recorded:** only the screen or area you choose, and only between
  start and stop. The mouse pointer is included unless you pass
  `--no-cursor`. The selection overlay, the red frame, the Stop pill and
  notifications are excluded from capture, so they never end up in a clip.
  Nothing records audio, keystrokes or the camera. The pointer's position
  and button state are sampled while recording, only to draw the pointer,
  click rings and zoom (not with `--cursor os` or `--no-cursor` unless
  zoom or click rings are on).
- **Where files go:** the output file you name, or `Videos\better-video`
  for the tray. While recording, a raw intermediate is kept in
  `%TEMP%\better-video` and deleted after export; leftovers from a crash
  are removed after a day. Tray settings live in
  `%APPDATA%\better-video\settings.json`.
- **What it runs:** nothing except Explorer (for "Show in folder"), and
  ffmpeg only if you use the formats that need it. The clipboard and
  folder picker use Windows APIs directly, so it works where PowerShell is
  restricted.
- **Install:** a single per-user `bv.exe`; no installer, admin rights or
  background service. Quit the tray and delete the file to remove it.

## Usage

| Command | What it does |
| --- | --- |
| `bv record [OUT]` | Record the screen, export when you stop. Format comes from `--format`, `--gif`, or the file extension (default GIF). |
| `bv hotkey` | Run in the background. `--key` (default `ctrl+alt+r`) starts and stops a recording; each one is exported to `--dir` and copied to the clipboard. Takes the same capture/export flags as `record`. macOS needs Input Monitoring permission. |
| `bv tray` | Tray (Windows, Linux) / menu bar (macOS) icon. On Windows, click it (or press the hotkey) to pick an area and options, then record; right-click for the menu: record the same area again, open or show the last clip, the save folder (shown in the menu, changeable), clipboard copy. Elsewhere the menu starts/stops recording the primary screen. Saves to `~/Videos/better-video` (`~/Movies` on macOS) unless `--dir` is set; a notification shows each saved file. Build with `--features tray`. |
| `bv export RAW` | Re-export a raw recording (`bv record --keep-raw`) with different settings, e.g. try zoom on/off without re-recording. |
| `bv monitors` | List connected monitors (Windows) with the number to pass to `--monitor`. |
| `bv windows` | List visible windows (Windows), for `--window "Title"`. |
| `bv stop` | Stop recordings running in the background, wait for their export, and print the results. |
| `bv skill install` | Install the Claude skill for recording demos (`~/.claude/skills/better-video/SKILL.md`). `bv skill show` prints it. |
| `bv doctor` | Check dependencies. |

**Choosing what to record:** `-s/--select` dims every screen before recording:
click a screen to record all of it, or drag out an area (Esc cancels), then
resize it with the handles or drag it around. A bar under the selection sets
the format (GIF/MP4), size (Small 640px @ 10 fps, Medium 960px @ 12 fps,
Large 1280px @ 15 fps), click rings and zoom (Off / Click / Hover / Follow),
and shows where the file will be saved; Record starts a 3-second countdown
(`--countdown 0` skips it). While recording, a red frame marks the area and a
pill under it shows the time with Stop and Discard; all of it is hidden from
the recording itself (Windows 10 2004 or later).

Without `--select`, `record`, `hotkey` and `tray` capture the primary monitor
by default. Pick another with `-m 2` (numbers from
`bv monitors`, left to right) or record everything with `-m all`. `--region`
offsets may be negative for screens left of or above the primary one, e.g.
`--region 1920x1080-1920+0`.

### Zoom

Zoom is applied when the clip is exported. Because the whole recording is
known by then, it is planned like an editor would cut it, and the camera
is already in place when the action happens. Pick a mode with `--zoom-on`
(or in the `--select` bar):

| Mode | Zooms on… | Aims at |
|---|---|---|
| `click` (`-z`, bar: **Click**) | mouse presses | the clicks it frames |
| `hover` (bar: **Hover**) | the cursor coming to rest: it stays within about 1% of the screen width (19 px on a 1920 px screen) for at least 0.6 s (`--zoom-dwell`). Where the cursor sits when recording starts doesn't count. | where it rests |
| `click-or-hover` | either of the above | the click or rest point |
| `follow` (`--zoom-follow`, bar: **Follow**) | the whole clip, at `--zoom-level` | the cursor, panning only when it leaves the middle half of the view |
| off (bar: **Off**) | never | n/a |

How click and hover zooms are planned:

- **Clicks share a zoom.** Clicks less than 3 s apart that fit in one view
  together get one zoom, centred on them and just deep enough to show them
  all with a 12% margin: the deepest of 1.25x, 1.5x and 1.8x that fits
  (`--zoom-level` caps it; a deeper level is used only where it fits).
- **The camera holds still.** While zoomed it doesn't pan between clicks,
  so still frames are identical (GIFs stay small).
- **No zoom hides a click.** A zoom is back out before a click outside its
  view, and comes in only after one.
- **No yo-yo.** Two zooms have at least 1.5 s of whole screen between
  them; closer ones merge if they fit together, otherwise the one with
  fewer clicks is skipped. A zoom that couldn't hold for 1.8 s is skipped.
- **Wide ends.** The clip opens on the whole screen (no zoom moves in
  before 2.5 s) and is back out 1 s before the end; a click in the last
  second is ignored.
- **Calm motion.** A zoom is fully in 0.5 s before its first click and
  holds 1.5 s after its last (`--zoom-hold`). Each move takes 0.7 s (to
  1.25x) to 0.9 s (to 1.8x) on a critically damped spring curve: it
  starts at rest, never overshoots, lands exactly, and scales in log space
  so the pace feels even.

Tuning: `--zoom-level 1.8` (deepest zoom), `--zoom-hold 1.5`,
`--zoom-dwell 0.6` and `--zoom-smooth 0.8` (pace of camera moves; larger
is slower).

The planning, easing and follow are ported from
[OpenScreen](https://github.com/getopenscreen/openscreen) (MIT); see
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

### Styling

`--style` frames the recording for a polished look: it sits on a background
with padding around it, rounded corners and a soft drop shadow. The frame is
added at export, so a `--keep-raw` take can be restyled with `bv export`, and
zoom and click rings happen inside the frame (the frame itself stays still).

| Style | Background |
|---|---|
| `none` (default) | no frame: the recording edge to edge, as before |
| `clean` | light grey |
| `gradient` | blue to violet |
| `dark` | dark slate |

`-w/--width` is the width of the whole frame, padding included; a small
recording is never scaled up, the padding is added around it. Fine-tune with
`--background '#rrggbb'` (or `'#rrggbb,#rrggbb'` for a diagonal gradient),
`--padding PX` (default 8% of the recording's width) and `--radius PX`
(default about 2%); any of them alone implies `--style clean`. The background
never changes, so a styled GIF only stores it once and costs little more
than an unstyled one.

### Pointer

By default the pointer isn't captured with the screen: bv records where it
is (the same ~120 Hz track zoom uses) and draws a crisp arrow at export,
white with a dark outline and a soft shadow. That means it can be:

- **bigger** — `--cursor-size SCALE`, relative to a standard system arrow at
  the output's scale (default 1.5). It grows a little when zoomed in (by the
  square root of the zoom).
- **smooth** — `--cursor-smooth SECS` (default 0.15, about how long it
  takes to catch up; 0 follows the recorded path exactly). Jitter and
  low-frame-rate steps go away, yet at each click it's exactly where you
  clicked, and once you stop it settles exactly, so a still pointer costs
  nothing in a GIF.
- **pressed** — it squashes a little while a button is held
  (`--no-cursor-bounce` turns that off).
- **hidden when idle** — `--cursor-hide SECS` fades it out after that long
  without moving and back in just before it moves (off by default: in a
  short demo a pointer that vanishes reads as a glitch).

It's always an arrow: the I-beam, hand and resize shapes aren't drawn.
`--cursor os` records the system pointer in the frames instead (the old
behaviour, and the fallback when cursor tracking isn't available, e.g. a
build without the `input` feature or no X11), and `--no-cursor` (or
`--cursor none`) leaves it out. A `--keep-raw` take made with the drawn
pointer can be re-exported with any `--cursor-*` settings, or with
`--no-cursor`; `bv export` draws it again by default. `bv export --track
PATH` names the cursor track if it isn't next to the recording.

Useful flags: `-z/--zoom`, `--zoom-on click|hover|click-or-hover|follow`,
`--zoom-level 1.25`, `--zoom-smooth 1.2`, `--zoom-hold 2`, `-w/--width 960`,
`--fps 10`, `-d/--duration 15`, `-m/--monitor 2`, `--region WxH+X+Y`,
`--trim-start 0.5`, `--trim-end 1`, `--style clean|gradient|dark`,
`--cursor drawn|os|none`, `--cursor-size 2`, `--cursor-hide 3`.

**Size tips:** GitHub caps PR attachments at 10 MB. MP4 is usually 3–10×
smaller than GIF for the same clip and GitHub plays it inline; use GIF where
autoplay without a player matters. For GIFs, lower `--fps` and `--width` first.

## How it works

Recording and exporting are separate so recording stays cheap. There are two
engines; Windows uses the native one by default (`--engine ffmpeg` switches).

**Native engine** (`bv-engine`, Windows, no ffmpeg needed for GIF):

1. **Record** — the region is grabbed at the export frame rate (with the
   cursor drawn in by `DrawIconEx` only for `--cursor os`). On one monitor that's DXGI desktop duplication: the GPU copies
   the screen and Windows says when nothing changed, so an idle screen costs
   almost nothing. Areas spanning monitors (or machines where duplication
   isn't available) use GDI `BitBlt`. Frames are compared in the capture
   buffer's own BGRA, 32×32 tiles at a time, and only changed tiles are
   copied and written, LZ4-compressed, to a `.bvraw` file.
2. **Export** — frames are replayed at the output frame rate, and only the
   output rows under changed pixels are re-rendered, re-counted and
   re-mapped. Downscaling box-averages so text stays readable. For GIF, a
   first pass collects colours (an exact palette when the clip uses at most
   255, median cut otherwise) and a second pass encodes with a built-in LZW
   coder: each frame stores only the rectangle that changed, unchanged pixels
   in it are transparent, and identical frames merge into one. MP4 goes to
   Windows' Media Foundation H.264 encoder; WebM frames are piped to ffmpeg.

`BV_CAPTURE=gdi` forces GDI capture; `BV_PROFILE=1` prints where recording
and export time went.

**ffmpeg engine** (Linux, macOS, or `--engine ffmpeg`):

1. **Record** — ffmpeg grabs the screen (`x11grab` / `avfoundation` / `gdigrab`)
   into a fast, visually lossless `.mkv`, at the export frame rate (15 fps
   for a GIF, which halves the grabber's CPU compared with 30). With
   `--keep-raw` it captures at least 30 fps so the take can be re-exported
   at any rate; `--capture-fps` overrides both.
2. **Export** — GIFs: ffmpeg decodes the `.mkv` at the output frame rate into
   a temporary `.bvraw` (only changed tiles are kept, so it's small), and the
   native exporter above takes it from there: partial redraws, the built-in
   palette and LZW coder, and two passes over the file instead of holding the
   clip in memory. MP4/WebM: without zoom, pointer or style, a single ffmpeg
   pass scales and encodes; otherwise ffmpeg decodes to raw RGBA, frames are
   rendered as below, and a second ffmpeg encodes.

Either way a background thread samples the cursor at ~120 Hz into a JSON
track, `ZoomPlanner` plans the zooms from its clicks and picks a viewport
per frame (a pure function of time, clamped to the screen), and `render` crops + scales in one pass.
The pointer is then drawn from the track: a spring-smoothed path (pulled back
onto the recorded one around each click, snapping once settled) and a
signed-distance arrow, anti-aliased, mapped through the zoom viewport like
click rings. The native exporter only redraws the rows where the pointer and
click rings were and are, so a still pointer leaves frames identical.
With `--style`, the background and shadow are drawn once and each frame's
changed rows are copied into the frame with anti-aliased corners, so
everything outside the recording is byte-identical from frame to frame (the
native GIF path adds a fixed ordered dither to the background so gradients
don't band).

```
crates/
  bv-engine/ # native engine: GDI capture, .bvraw, palette, LZW, GIF writer
  bv-core/   # library: record, cursor track, zoom planner, renderer, export
  bv-cli/    # `bv` binary + platform cursor tracker
```

## Roadmap

Ease of access is the goal: one hotkey to start, one to stop, link on the
clipboard.

- [x] Global hotkey to start/stop (`bv hotkey`)
- [x] Menu bar / tray app (`bv tray`)
- [x] Drag-to-select capture region (`--select`, Windows)
- [x] Copy the output file to the clipboard after export
- [x] Trim start/end before export
- [x] Align cursor timestamps with ffmpeg's first frame
- [ ] HiDPI: map cursor points → capture pixels on macOS scaling (Windows
      runs per-monitor DPI aware, so everything is in physical pixels)
- [x] Record one monitor at a time on Windows (`--monitor`)
- [ ] Wayland capture (xdg-desktop-portal + PipeWire)
- [x] Native capture + GIF encoder on Windows (`bv-engine`), no ffmpeg needed
- [ ] Native capture on macOS (ScreenCaptureKit) and Linux; DXGI desktop
      duplication on Windows for high frame rates
- [ ] Optional [gifski](https://gif.ski) encoder for higher-quality GIFs
- [x] Click highlight rings
- [x] Smooth, spring-based zoom with click / hover / follow triggers
- [x] Zoom planned like an editor: grouped clicks, fitted depth, no yo-yo
- [ ] Keystroke overlay
