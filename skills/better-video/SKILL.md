---
name: better-video
description: Record a short GIF or MP4 of the screen or an app window with the local `bv` CLI (better-video), e.g. to show a UI change in a pull request. Use when asked to record, capture or demo something on screen, or to attach a recording/GIF to a PR. Windows has the most complete support; macOS/Linux need ffmpeg.
---

# Recording demos with better-video (`bv`)

`bv` records the screen locally and writes a small GIF or MP4. It makes no
network connections. Progress goes to stderr; with `--json`, stdout is
exactly one JSON line describing the result.

Check it's installed: `bv --version` (and `bv doctor` for what works on this
machine). If it's missing, build it from the better-video repo:
`cargo install --path crates/bv-cli`.

## Choose what to record

- A window, by title (Windows): `--window "Title text"`. The match is a
  case-insensitive substring; the front-most match wins. `bv windows` lists
  candidates. The window's position when recording starts is used, so don't
  move it mid-recording.
- A monitor: `-m 1`, `-m 2` (see `bv monitors`) or `-m all`. Default on
  Windows: the primary monitor.
- An exact area: `--region WIDTHxHEIGHT+X+Y` (offsets may be negative).

Never use `-s/--select`. It waits for a person to pick an area.

## Record for a fixed time

```sh
bv record demo.gif --window "My App" -d 8 --json
```

Exits after about 8 seconds plus the export, and prints one line like:

```json
{"path":"C:\\work\\demo.gif","bytes":412345,"format":"gif","width":960,"height":540,"seconds":8.0}
```

## Record while you drive the app

Start it in the background, run your interactions, then stop it:

```sh
bv record demo.gif --window "My App" --json &
# ... click / type / run the steps you want to show ...
bv stop        # waits for the export, prints each recording's JSON result
```

On PowerShell use `Start-Process bv -ArgumentList ... -NoNewWindow` for the
background step. `bv stop --timeout 120` bounds the wait.

## Make it small and readable

- GitHub caps attachments at 10 MB. Small clips: `-w 640 --fps 10`;
  readable text: `-w 960 --fps 12`.
- `--format mp4` (or an `.mp4` path) is usually 3–10× smaller than GIF and
  plays inline on GitHub. GIF autoplays everywhere.
- `--trim-start 0.5 --trim-end 0.5` cuts the setup and the stop.
- `--clicks` draws a ring at each click. `--zoom-on click|hover|follow`
  zooms the camera in on the action (see the README's Zoom section).
- `--no-cursor` leaves the pointer out.

## Exit codes

| code | meaning |
|---|---|
| 0 | saved; the result is on stdout |
| 1 | error, described on stderr |
| 2 | bad arguments |
| 3 | a person cancelled or discarded it |

## Put it in a pull request

GitHub has no API for uploading PR attachments. Either:

- commit the file to a branch that holds assets and link it from the PR
  body: `![demo](https://github.com/OWNER/REPO/blob/BRANCH/demo.gif?raw=true)`, or
- tell the person where the file is (`path` in the JSON) so they can drag it
  into the PR description.

Record only what the change needs. Close or avoid windows with personal or
confidential information first: the recording contains whatever is on
screen in that area.
