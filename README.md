# better-video

Lightweight screen recorder for developers. Record a clip, get a **small GIF or
MP4 you can drop straight into a PR body**, optionally with a camera that
**zooms in on your cursor** so reviewers can actually see what you clicked.

```sh
bv record                 # record until Enter / Ctrl+C → ./bv-2026-10-11_15-48-12.gif
bv record demo.mp4 -z     # MP4, zooms in around each click
bv record --zoom-on hover # zoom where the cursor rests (also click, follow)
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

**Zoom:** the camera eases in and out on springs (no snapping) and only pans
when the cursor nears the edge of the zoomed view. It zooms in on each click
(`click`), where the cursor comes to rest (`hover`), either
(`click-or-hover`), or stays zoomed and follows the cursor (`follow`).
`--zoom-smooth` sets roughly how long it takes to settle (default 0.8 s) and
`--zoom-hold` how long it stays in. The `--select` bar offers Off / Click /
Hover / Follow.

Useful flags: `-z/--zoom`, `--zoom-on click|hover|click-or-hover|follow`,
`--zoom-level 2.5`, `--zoom-smooth 1.2`, `--zoom-hold 2`, `-w/--width 960`,
`--fps 10`, `-d/--duration 15`, `-m/--monitor 2`, `--region WxH+X+Y`,
`--trim-start 0.5`, `--trim-end 1`.

**Size tips:** GitHub caps PR attachments at 10 MB. MP4 is usually 3–10×
smaller than GIF for the same clip and GitHub plays it inline; use GIF where
autoplay without a player matters. For GIFs, lower `--fps` and `--width` first.

## How it works

Recording and exporting are separate so recording stays cheap. There are two
engines; Windows uses the native one by default (`--engine ffmpeg` switches).

**Native engine** (`bv-engine`, Windows, no ffmpeg needed for GIF):

1. **Record** — the region is grabbed at the export frame rate, cursor
   included. On one monitor that's DXGI desktop duplication: the GPU copies
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
   in it are transparent, and identical frames merge into one. MP4/WebM
   frames are piped to ffmpeg.

`BV_CAPTURE=gdi` forces GDI capture; `BV_PROFILE=1` prints where recording
and export time went.

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
- [x] Smooth, spring-based zoom with click / hover / follow triggers
- [ ] Keystroke overlay
