# better-video

Lightweight screen recorder for developers. Record a clip, get a **small GIF or
MP4 you can drop straight into a PR body**, optionally with a camera that
**zooms in on your cursor** so reviewers can actually see what you clicked.

```sh
bv record                 # record until Enter / Ctrl+C → ./bv-<ts>.gif
bv record demo.mp4 -z     # MP4, zooms in around each click
bv record --zoom-follow   # stay zoomed and follow the cursor
bv record --clicks        # draw a ring wherever you click
bv record -d 10 --region 1280x720+0+0 --width 960
```

## Install (from source)

Requirements: Rust (stable) and [`ffmpeg`](https://ffmpeg.org/download.html) on
your `PATH` (or point `BV_FFMPEG` / `BV_FFPROBE` at it). On Linux, cursor zoom
needs X11 headers (`libx11-dev`).

```sh
cargo install --path crates/bv-cli
bv doctor                 # checks ffmpeg + cursor tracking
```

## Usage

| Command | What it does |
| --- | --- |
| `bv record [OUT]` | Record the screen, export when you stop. Format comes from `--format`, `--gif`, or the file extension (default GIF). |
| `bv export RAW` | Re-export a raw recording (`bv record --keep-raw`) with different settings, e.g. try zoom on/off without re-recording. |
| `bv doctor` | Check dependencies. |

Useful flags: `-z/--zoom`, `--zoom-follow`, `--zoom-level 2.5`, `-w/--width 960`,
`--fps 10`, `-d/--duration 15`, `--region WxH+X+Y`,
`--trim-start 0.5`, `--trim-end 1`.

**Size tips:** GitHub caps PR attachments at 10 MB. MP4 is usually 3–10×
smaller than GIF for the same clip and GitHub plays it inline; use GIF where
autoplay without a player matters. For GIFs, lower `--fps` and `--width` first.

## How it works

Recording and exporting are separate so recording stays cheap:

1. **Record** — ffmpeg grabs the screen (`x11grab` / `avfoundation` / `gdigrab`)
   into a fast, visually lossless `.mkv`. A background thread samples the
   cursor at ~120 Hz into a JSON track.
2. **Export** — without zoom, a single ffmpeg pass scales and encodes (GIF uses
   a one-pass `palettegen`/`paletteuse` tuned for screen content). With zoom,
   ffmpeg decodes to raw RGBA, `ZoomPlanner` picks a viewport per frame (eased,
   frame-rate independent, clamped to the screen), `render::crop_scale_rgba`
   crops + scales in one pass, and a second ffmpeg encodes.

```
crates/
  bv-core/   # library: record, cursor track, zoom planner, renderer, export
  bv-cli/    # `bv` binary + platform cursor tracker
```

## Roadmap

Ease of access is the goal: one hotkey to start, one to stop, link on the
clipboard.

- [ ] Desktop shell (tray/menu-bar app, e.g. Tauri) with a global hotkey and
      drag-to-select region
- [ ] Copy the output file/path to the clipboard after export
- [x] Trim start/end before export
- [x] Align cursor timestamps with ffmpeg's first frame
- [ ] HiDPI: map cursor points → capture pixels on macOS/Windows scaling
- [ ] Wayland capture (xdg-desktop-portal + PipeWire)
- [ ] Native capture backends (ScreenCaptureKit, DXGI) to drop the ffmpeg
      runtime dependency, or ship ffmpeg as a sidecar
- [ ] Optional [gifski](https://gif.ski) encoder for higher-quality GIFs
- [x] Click highlight rings
- [ ] Keystroke overlay
