# rimg

A small terminal image viewer in Rust, modeled on
[timg](https://github.com/hzeller/timg). It handles the Kitty graphics protocol,
PNG, JPEG and PDF, plus SVG behind a feature flag. Nothing else.

`timg/` is a checkout of the C++ original, kept for reference. It is a separate
git repo and is excluded from this one. Read it; never edit it.

## Build and run

```
cargo run -- img/roc.pdf            # interactive viewer
cargo run -- --print img/roc.pdf    # one-shot render, then exit
cargo run --features svg -- x.svg
cargo build --release
```

The viewer takes over the screen; `--print` writes the image where the cursor
is and exits. A run whose stdout is not a terminal prints regardless, so piping
and redirecting keep working.

Requires a terminal that speaks the Kitty graphics protocol: kitty, Ghostty,
WezTerm, or Konsole. Elsewhere the escape sequences print as garbage, so check
`$TERM` before pasting output anywhere. `--force-kitty` skips detection, which
is how you capture the escape sequences to a file and inspect them, and
`--probe` prints what detection saw.

PDF goes through hayro, which is pure Rust, so the default build needs nothing
installed. Build with `--features pdf-pdfium` to use pdfium instead. It covers
what hayro's README lists as unsupported, including encrypted files, blend
modes and non-embedded CID fonts, but it loads libpdfium at runtime, so that
has to be on the loader path first, and Homebrew has no formula for it. Cargo
features are additive, so a build can carry both backends; pdfium takes
precedence.

## Layout

- `src/main.rs` parses arguments and loops over the input files.
- `src/geometry.rs` does the fit math: image pixels, terminal grid, and
  requested geometry in; final pixel size out.
- `src/framebuffer.rs` holds an RGBA8 buffer and the resize step. Every decoder
  produces one of these and the renderer consumes one.
- `src/source/` holds the decoders, one module per format, each exposing
  `load(bytes, hints) -> Framebuffer`. `source/mod.rs` sniffs the magic bytes
  and matches on what it finds. `source/pdf/` is the exception with two
  modules, one per backend, behind that same signature.
- `src/term/` interrogates the terminal for its size in cells and in pixels, and
  for which graphics protocol it supports.
- `src/render/kitty.rs` serializes a framebuffer into escape sequences, both
  the one-shot `a=T` and the viewer's `a=t` transmit plus `a=p` placements.
- `src/tui.rs` is the viewer: the alternate screen, the key loop, and the map
  from zoom and pan to a source rectangle.

## Design constraints

Be stingy with dependencies, with one deliberate exception: PDF ships in the
default build, though adding hayro grew the tree from 26 crates to 93 and the
release binary from 700K to 4.5M. Anything further goes behind a feature that
is off by default.

SVG and PDF carry no pixel size of their own, so rasterize them straight at
display size. Scale a bitmap afterwards instead and you lose resolution the
source still had.

The viewer never re-encodes pixels. It transmits once, then pans and zooms by
sending a new source rectangle for the stored image, which the terminal scales.
A frame costs 69 bytes. Re-encoding instead measured 40-90ms and several
megabytes per frame, so think hard before moving redrawing back into Rust.

Reuse one placement id and let the new placement replace the old one. Delete
the old one first and the background shows through the gap, which reads as a
flash on every keypress.

No traits with one implementor. Formats vary along one axis, bytes in and
framebuffer out, so they are an enum and a `match` rather than a
`dyn ImageSource`. Before a new format talks you into a trait, count the
implementors; see the `concrete-first` skill.

Terminal queries mutate global state (termios). Keep that code in
`src/term/query.rs` and restore what it changed on every path out, panics
included. A reply that arrives after the restore goes to the next reader of the
tty, which is the user's shell, and shows up at their prompt as though they had
typed the escape sequence. So: answer from environment variables where you can,
and where you cannot, drain the tty before restoring it. `TERM` alone does not
settle which protocol a terminal speaks, because kitty and Ghostty are often run
with `TERM=xterm-256color` to keep ssh working.

## Testing

Geometry and protocol serialization are pure functions, so unit-test them.
Decoders get small fixtures under `tests/data/`.

No test can tell you whether an image looks right; that needs a real terminal
and a human. `cargo run -- timg/img/sunflower-term.png` is the eyeball test.
