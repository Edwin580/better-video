---
name: better-video
description: Make a short, polished screen recording (MP4 or GIF) of an app with the local `bv` CLI (better-video) and attach it to a pull request, from storyboard to recording, frame check and upload. Use when asked to record, capture or demo something on screen, to show a UI change or bug fix to reviewers, or to add a video/GIF to a PR. Works on Windows, macOS and Linux (X11, including headless Xvfb); macOS/Linux need ffmpeg.
---

# PR demo videos with better-video (`bv`)

Goal: a 10–30 s MP4 that shows one change, framed, with a drawn pointer,
click rings and the camera zooming in on each click, attached to the PR.

`bv` is local-only (no network). Progress goes to stderr; with `--json`,
stdout is exactly one JSON line:
`{"path","bytes","format","width","height","seconds"}`.

## 1. Check the setup

```sh
bv --version
bv doctor        # needs: ffmpeg (macOS/Linux) and "cursor tracking available"
```

Missing? Build it from the better-video repo: `cargo install --path
crates/bv-cli` (needs Rust; on Linux also `libx11-dev`, plus `ffmpeg` on
macOS/Linux). If `bv doctor` says cursor tracking is unavailable there is no
zoom, click rings or drawn pointer; on Linux that means `DISPLAY` isn't set
or isn't an X11 display.

## 2. Plan before recording

Write a storyboard of 1–3 beats, one action each, 10–30 s in all. Show the
change, not the product: the new behaviour, or the bug no longer happening.

```text
1. Type "Buy milk", click Add      -> it appears at the top     (~5 s)
2. Tick it                         -> "0 tasks left"            (~3 s)
```

Then set the stage:

- **What to record.** One app window, not the whole desktop.
  - Windows: `--window "Title"` (substring; `bv windows` lists them).
  - macOS / Linux: `--region WxH+X+Y`. On X11:
    `xdotool search --name "Title" getwindowgeometry --shell` gives X, Y,
    WIDTH, HEIGHT (check it: some window managers report it off by the
    title bar).
  - A web app: a window without browser chrome,
    `chromium --app=http://localhost:3000 --window-size=1280,800 --user-data-dir=$(mktemp -d)`.
- **App state.** Fresh, believable data; the first beat's starting screen
  open; nothing private (tokens, emails, real customer data,
  notifications) anywhere in the area. Close other windows that could pop
  over it.
- **Probe the area** with a 1 s take and look at it (section 5) before the
  real one: `bv record probe.mp4 --region 560x420+680+110 -d 1`.

## 3. Record

Recommended for a PR:

```sh
bv record demo.mp4 --region 560x420+680+110 \
  --style gradient --zoom-on click --clicks \
  --trim-start 0.5 --trim-end 0.5 --keep-raw --json > demo.json &
# ... drive the app: the storyboard beats (section 4) ...
bv stop          # stops it, waits for the export, prints the JSON result
```

- **MP4** (`.mp4` path): small and plays inline on GitHub. Export a GIF
  as well only where a GIF is needed (an inline preview in Markdown).
- **`--style gradient`** (or `clean`, `dark`): background, padding, rounded
  corners, shadow. `-w` is then the whole frame's width.
- **`--zoom-on click`** zooms on each click (`hover` for where the pointer
  rests); **`--clicks`** draws a ring at each click. The pointer is drawn
  at export by default (`--cursor drawn`); `--cursor-size 2` makes it
  bigger.
- **Zoom picks its own depth** (1.25x to 1.8x) to frame the clicks it
  covers, opens and ends on the whole screen, and skips clicks it can't
  show well (spread out, too early or too late). Leave `--zoom-level`
  alone; it only caps the depth.
- **Size:** `-w` defaults to 1280 (small recordings are never scaled up).
  MP4 defaults to 30 fps; keep it. For a GIF use `-w 800 --fps 12`.
- **`--trim-start/--trim-end SECS`** cut the setup before the first beat
  and the stop after the last.
- **Fixed length** instead of `bv stop`: `-d 15`. Start the driving script
  in the background first, then run `bv record ... -d 15 --json`.
- **Background runs:** start `bv record` with `&` (in Claude Code, a
  background Bash command); `bv stop` finds it through the temp folder, so
  run both with the same `TMPDIR`. `bv stop --timeout 120` bounds the wait.
- **Exit codes:** 0 saved (result on stdout), 1 error (stderr says why),
  2 bad arguments, 3 cancelled or discarded by a person.
- Never use `-s/--select`: it waits for a person.

**Restyle without re-recording.** `--keep-raw` leaves the raw take
(`demo.raw.mkv`, or `.bvraw` on Windows) and its pointer track
(`demo.raw.cursor.json`) next to the output. Export again with any style,
zoom, trim or pointer settings:

```sh
bv export demo.raw.mkv -o demo.mp4 --style dark --zoom-on click --clicks --zoom-level 1.25  # gentler zoom
bv export demo.raw.mkv -o demo.gif -w 800 --fps 12 --style gradient --zoom-on click --clicks
```

## 4. Drive the app

Do it from a script so every take has the same timing. Move deliberately
and pause about 1 s after each result appears. Zoom is planned from the
clicks: start the first beat ~2.5 s in (the clip opens on the whole
screen), keep the clicks of one beat close together (within ~3 s and a
part of the screen) so they share one zoom, and leave ~2 s after the last
click before stopping.

bv samples the **real OS pointer**, so:

- **Clicks must hold the button ~100–150 ms.** An instant synthetic click
  can fall between samples and is missed (no ring, no zoom).
- **Move along a path** with intermediate points, not a jump: the drawn
  pointer and the camera follow the path.
- **Playwright / Puppeteer / CDP clicks don't move the OS pointer**, so the
  take has no pointer, rings or zoom. Use them to prepare state, but click
  with a tool that moves the real pointer (`xdotool` on X11, `cliclick` on
  macOS, SendInput / AutoHotkey on Windows).

X11 helpers:

```sh
glide() {  # move to $1,$2 in ~0.4 s
  eval "$(xdotool getmouselocation --shell)"
  for i in $(seq 1 12); do
    xdotool mousemove $((X + ($1 - X) * i / 12)) $((Y + ($2 - Y) * i / 12)); sleep 0.03
  done
}
click() { glide "$1" "$2"; sleep 0.25; xdotool mousedown 1; sleep 0.12; xdotool mouseup 1; sleep 0.6; }
type_text() { xdotool type --delay 60 "$1"; sleep 0.4; }
```

**Headless Linux:** run an X server, a window manager and a *headed*
browser on it, and record that display:

```sh
Xvfb :99 -screen 0 1280x720x24 -nolisten tcp &
DISPLAY=:99 openbox &
DISPLAY=:99 chromium --app=http://localhost:3000 --window-position=100,80 --window-size=1000,600 \
  --user-data-dir=$(mktemp -d) --no-first-run &
export DISPLAY=:99   # for bv, xdotool and the driving script
```

## 5. Check before posting

Look at the frames, don't assume:

```sh
DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 demo.mp4)
ffmpeg -v error -y -i demo.mp4 -vf "fps=6/$DUR,scale=640:-1,tile=3x2" -frames:v 1 check.png
ffmpeg -v error -y -ss 4.5 -i demo.mp4 -frames:v 1 at-4.5s.png   # one moment, full size
ls -l demo.mp4                                                   # bytes
```

Open the images and check:

- every beat happens, and the result of each is on screen for ~1 s;
- the zoom lands on the action, and the pointer and click rings show;
- the right window, nothing cut off, no setup screen or stray dialog;
- nothing private; file under 10 MB.

Fix and take again (or `bv export` the raw take with new settings) until it
holds. A new take is cheap.

## 6. Attach it to the PR

Only to a PR the user asked for a video on, or one you opened for them.
Never commit videos to the code branches.

**With `gh` 2.99 or later** (`gh --version`), which uploads the file like a
drag-and-drop. Write the text in a file, the video alone on its own line so
GitHub shows a player:

```markdown
**Demo:** ticking the last task now shows "All done".

![demo](demo.mp4)
```

```sh
gh pr comment 123 --body-file pr-video.md --attach demo.mp4
gh pr create --title "..." --body-file pr-body.md --attach demo.mp4   # new PR
```

**Older `gh`, or no `gh`:**

- An assets branch: commit the file to a branch that holds only media, and
  link it from the PR body. Name files after the PR.

  ```sh
  git fetch origin pr-assets; git worktree add --detach ../assets && cd ../assets
  git switch pr-assets 2>/dev/null || git switch --orphan pr-assets   # new: ask first
  cp ../repo/demo.mp4 pr123-demo.mp4 && cp ../repo/demo.gif pr123-demo.gif
  git add pr123-* && git commit -m "Add demo for #123" && git push origin pr-assets
  ```

  `![demo](https://github.com/OWNER/REPO/blob/pr-assets/pr123-demo.gif?raw=true)`
  shows a GIF inline; an MP4 from a branch doesn't play inline, so link it:
  `[demo.mp4](https://github.com/OWNER/REPO/blob/pr-assets/pr123-demo.mp4?raw=true)`.
- Or give the user the path (`path` in the JSON) to drag into the PR.

Report the PR or comment URL.

## Troubleshooting

| Symptom | Fix |
|---|---|
| No pointer, rings or zoom | `bv doctor` must say cursor tracking is available (Linux: `DISPLAY` is X11). Drive with real pointer events (section 4), not Playwright/CDP. |
| A click has no ring | Hold the button 100–150 ms. |
| A click has no zoom | Expected for a click in the first 2.5 s or last second, one far from the clicks around it, or one a zoom couldn't hold for ~2 s without hiding the next. Group the beat's clicks, leave room before and after. |
| Zoom crops the result | The region is wrong (probe it), or the result appears far from the clicks; `--zoom-level 1.25` keeps zooms gentle; re-export the raw take. |
| Empty / black recording | Wrong region or display; on Linux check `DISPLAY` and that the window is on that screen. Probe with `-d 1`. |
| `bv stop`: "No recordings running." | It already ended (`-d`), it failed (read its stderr), or it ran with a different `TMPDIR`. |
| `bv stop` takes minutes | Long takes export slowly; keep it under 30 s. `bv stop --timeout 120` bounds the wait. |
| File too big | MP4 instead of GIF; shorter take, tighter `--trim-start/--trim-end`; `-w 960`; GIF: `--fps 10`. |
| Text unreadable | Record a smaller area or zoom in (`--zoom-level 2.5`); don't shrink below `-w 800`. |
| Exit code 2 | A flag is wrong: `bv record --help`, `bv export --help`. |
