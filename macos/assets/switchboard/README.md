# Switchboard split icon

One centered mark, clipped vertically: Claude's left half and OpenAI's right half.

- `Switchboard-menubar.svg`: monochrome vector outlines traced from the bundled provider favicons. No embedded bitmap images.
- `Switchboard-menubar.pdf`: the same contours in AppKit's native vector format; the app renders it as an 18pt template that adapts to appearance.
- `claude.jpg` and `openai.jpg`: color JPEG sources derived from the existing bundled provider PNGs.
- `Switchboard.png` and `Switchboard.icns`: the colored half-and-half app icon.

Regenerate from the repository root:

```sh
python3 macos/assets/switchboard/render.py
swift macos/assets/switchboard/render.swift "$PWD"
```

The Python composition requires Pillow. Original source favicons are 128px, so the enlarged app composition retains that source detail. Source provenance and ownership notices are in `../provider-icons/README.md`; this composition does not imply endorsement or ownership of those marks.
