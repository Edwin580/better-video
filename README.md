# better-video

Lightweight screen recorder for developers. Record a clip, get a **small GIF or
MP4 you can drop straight into a PR body**, optionally with a camera that
**zooms in on your cursor** so reviewers can actually see what you clicked.

```sh
bv record                 # record until Enter / Ctrl+C → ./bv-2026-10-11_15-48-12.gif
bv record demo.mp4 -z     # MP4, zooms in around each click
bv record --zoom-follow   # stay zoomed and follow the cursor
bv record --clicks        # draw a ring wherever you click
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
   taskbar tray (click **^** if it's hidden).
2. **Click the icon** or press **Ctrl+Alt+R**. The screens dim: click a screen
   to record all of it, or drag around the part you want.
3. In the bar under your selection, pick **GIF** or **MP4** and a size
   (**Small** is 640 px at 10 fps, the right size for a PR), then **Record**.
   A 3-2-1 countdown starts.
4. A red frame shows what's being recorded (it doesn't appear in the video).
   Click **Stop** under it, click the tray icon, or press Ctrl+Alt+R again.
5. A notification shows the file name and size. The file is saved in
   **Videos\better-video** and copied to the clipboard, ready to paste into a
   PR. Click the notification to show it in Explorer.

Right-click the tray icon to record the same area again, open the last
recording, see or change the save folder, or turn off clipboard copy. Your
choices are remembered in `%APPDATA%\better-video\settings.json`.

## Install (from source)

Requirements: Rust (stable) and [`ffmpeg`](https://ffmpeg.org/download.html) on
your `PATH` (or point `BV_FFMPEG` / `BV_FFPROBE` at it). On Windows, ffmpeg is
only needed for MP4/WebM: GIFs use the built-in engine. On Linux, cursor zoom
needs X11 headers (`libx11-dev`), and `--copy` needs `xclip` (X11) or
`wl-clipboard` (Wayland). The menu bar app on Linux needs `libgtk-3-dev`,
`libayatana-appindicator3-dev` and `libxdo-dev`.

```sh
cargo install --path crates/bv-cli                  # CLI
cargo install --path crates/bv-cli --features tray  # CLI + menu bar app
bv doctor                 # checks ffmpeg + cursor tracking
```

## Usage

| Command | What it does |
| --- | --- |
| `bv record [OUT]` | Record the screen, export when you stop. Format comes from `--format`, `--gif`, or the file extension (default GIF). |
| `bv hotkey` | Run in the background. `--key` (default `ctrl+alt+r`) starts and stops a recording; each one is exported to `--dir` and copied to the clipboard. Takes the same capture/export flags as `record`. macOS needs Input Monitoring permission. |
| `bv tray` | Tray (Windows, Linux) / menu bar (macOS) icon. On Windows, click it (or press the hotkey) to pick an area and options, then record; right-click for the menu: record the same area again, open or show the last clip, the save folder (shown in the menu, changeable), clipboard copy. Elsewhere the menu starts/stops recording the primary screen. Saves to `~/Videos/better-video` (`~/Movies` on macOS) unless `--dir` is set; a notification shows each saved file. Build with `--features tray`. |
| `bv export RAW` | Re-export a raw recording (`bv record --keep-raw`) with different settings, e.g. try zoom on/off without re-recording. |
| `bv monitors` | List connected monitors (Windows) with the number to pass to `--monitor`. |
| `bv doctor` | Check dependencies. |

**Choosing what to record:** `-s/--select` dims every screen before recording:
click a screen to record all of it, or drag out an area (Esc cancels). A bar under the
selection sets the format (GIF/MP4), size (Small 640px @ 10 fps, Medium 960px @
12 fps, Large 1280px @ 15 fps), click rings and zoom, and shows where the file
will be saved; Record starts a 3-second countdown (`--countdown 0` skips it).
While recording, a red frame marks the area with a Stop button under it; both
are hidden from the recording itself (Windows 10 2004 or later).

Without `--select`, `record`, `hotkey` and `tray` capture the primary monitor
by default. Pick another with `-m 2` (numbers from
`bv monitors`, left to right) or record everything with `-m all`. `--region`
offsets may be negative for screens left of or above the primary one, e.g.
`--region 1920x1080-1920+0`.

Useful flags: `-z/--zoom`, `--zoom-follow`, `--zoom-level 2.5`, `-w/--width 960`,
`--fps 10`, `-d/--duration 15`, `-m/--monitor 2`, `--region WxH+X+Y`,
`--trim-start 0.5`, `--trim-end 1`.

**Size tips:** GitHub caps PR attachments at 10 MB. MP4 is usually 3–10×
smaller than GIF for the same clip and GitHub plays it inline; use GIF where
autoplay without a player matters. For GIFs, lower `--fps` and `--width` first.

## How it works

Recording and exporting are separate so recording stays cheap. There are two
engines; Windows uses the native one by default (`--engine ffmpeg` switches).

**Native engine** (`bv-engine`, Windows, no ffmpeg needed for GIF):

1. **Record** — GDI grabs the region (cursor included) at the export frame
   rate. Each frame is split into 32×32 tiles and only the tiles that changed
   are written, LZ4-compressed, to a `.bvraw` file, so an idle screen costs
   almost nothing.
2. **Export** — frames are replayed at the output frame rate; unchanged ones
   aren't re-rendered. Downscaling box-averages so text stays readable. For
   GIF, a first pass collects colours (an exact palette when the clip uses at
   most 255, median cut otherwise) and a second pass encodes with a built-in
   LZW coder: each frame stores only the rectangle that changed, unchanged
   pixels in it are transparent, and identical frames merge into one.
   MP4/WebM frames are piped to ffmpeg.

**ffmpeg engine** (Linux, macOS, or `--engine ffmpeg`):

1. **Record** — ffmpeg grabs the screen (`x11grab` / `avfoundation` / `gdigrab`)
   into a fast, visually lossless `.mkv`.
2. **Export** — without zoom, a single ffmpeg pass scales and encodes (GIF uses
   a one-pass `palettegen`/`paletteuse` tuned for screen content). With zoom,
   ffmpeg decodes to raw RGBA, frames are rendered as below, and a second
   ffmpeg encodes.

Either way a background thread samples the cursor at ~120 Hz into a JSON
track, `ZoomPlanner` picks a viewport per frame (eased, frame-rate
independent, clamped to the screen), and `render` crops + scales in one pass.

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
- [ ] Keystroke overlay
