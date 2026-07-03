# docs/assets — images for the README

Visuals referenced by **both** READMEs (`README.md` and `README.ru.md`) live here,
linked by relative paths. One shared set of images for both languages — no
per-language duplicates.

## What to capture (by priority)

1. **`hero.png`** or **`hero.gif`** — the main above-the-fold visual. Best: a
   short GIF, 5–15 s, 10–15 fps, ≈720–960 px wide, showing:
   - the panel opening on **⌘J** (centered, Raycast-style);
   - the menu-bar counters changing live (**⏸ waiting · ⚙ working**);
   - a toast notification appearing **over a fullscreen** app.
   A static screenshot of the panel is an acceptable minimum; a GIF is better.
2. `panel.png` — the panel with the session list, model badges and the reply field.
3. `toast.png` — a toast over fullscreen.
4. `taskboard.png` — the "Tasks" slide-over (the TodoWrite board).

## How to wire it in

Both READMEs already contain a commented-out tag for the hero visual:

```html
<!-- <p align="center"><img src="docs/assets/hero.png" alt="…" width="760"></p> -->
```

Capture the image, drop it here as `hero.png` (or `hero.gif`), uncomment the tag
in **both** files and remove the "Screenshot and demo coming soon" placeholder line.

## How to capture (macOS)

- Area screenshot: **⌘⇧4** (or a window: **⌘⇧4**, then Space).
- Screen/area recording: **⌘⇧5**. Video → GIF: `ffmpeg` or
  [Gifski](https://gif.ski/). Example: `ffmpeg -i screen.mov -vf "fps=12,scale=720:-1" hero.gif`.
- Record on a clean desktop, with no personal data in paths or window titles.
