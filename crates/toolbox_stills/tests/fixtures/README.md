# toolbox_stills' test fixtures

Small pictures the crate's tests decode, copied from slide's `tests/fixtures/media`, where
slide's `script/make-fixtures` makes them with ffmpeg (`testsrc2`), libwebp and macOS's `sips`.
Remake them there and copy them here; they are committed, and tests depend on their sizes.

| File | What it is |
|---|---|
| `still-1920x1080.jpg` | a JPEG still (ffmpeg, `-q:v 5`) |
| `still-1080x1350.png` | a portrait PNG |
| `still.heic` | `still-1080x1350.png` as HEIC (`sips`) |
| `p3-64.heic` | a 64 px Display P3 HEIC with an ICC `colr` box (`sips -m`) |
| `still.avif` | a 1280x720 AVIF (SVT-AV1) |
| `still-160x90.gif` | a one-frame GIF |
| `still-160x90.webp` | a still WebP (`cwebp`) |
| `orient6-160x90.jpg` | stored landscape, red left and blue right, with EXIF orientation 6 spliced in: shown portrait, red on top |
| `anim-320x180.gif` | an animated GIF, 1 s at 10 fps |
| `anim-160x90.png` | an APNG, 5 frames at 10 fps |
| `anim-160x90.webp` | an animated WebP, 5 frames (`gif2webp`) |
| `head-*.{mp4,mov,webm,mkv}` | the first 64 bytes of slide's video fixtures: all sniffing reads of a video |

The codec probe's samples, `../../assets/probe.{heic,avif}` (64 px, `sips` and SVT-AV1), come
from the same script.
