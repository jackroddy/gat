# rimg

A small terminal viewer in Rust, modeled on
[timg](https://github.com/hzeller/timg). It handles the Kitty graphics protocol,
PNG, JPEG, PDF, SVG and Markdown. Nothing else.

`timg/` is a checkout of the C++ original, kept for reference. It is a separate
git repo and is excluded from this one. Read it; never edit it.

## Build and run

```
cargo run -- img/roc.pdf            # interactive viewer
cargo run -- --print img/roc.pdf    # one-shot render, then exit
cargo run -- x.svg
cargo run -- README.md
cargo build --release
cargo build --no-default-features --features png   # a smaller binary
```

The viewer takes over the screen; `--print` writes the image where the cursor
is and exits. A run whose stdout is not a terminal prints regardless, so piping
and redirecting keep working.

Requires a terminal that speaks the Kitty graphics protocol: kitty, Ghostty,
WezTerm, or Konsole. Elsewhere the escape sequences print as garbage, so check
`$TERM` before pasting output anywhere. `--force-kitty` skips detection, which
is how you capture the escape sequences to a file and inspect them, and
`--probe` prints what detection saw.

Markdown is not styled terminal text; it is rasterized to pixels like a PDF
page: headings at real sizes, tinted code blocks, and a place to put syntax
colour later. Text is monospace throughout, so line breaking is arithmetic over
one advance ratio rather than a shaping and measurement pass. The font is
compiled in, because usvg's default Options carries an empty fontdb and
`load_system_fonts` is gated out of this build. See
`src/source/markdown/font.rs`.

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
- `src/source/markdown/` is the other exception, being a pipeline rather than a
  decoder: `parse.rs` turns events into a block tree, `layout.rs` turns that
  into a flat display list of positioned runs, and `to_svg.rs` turns that into
  an SVG document for resvg. The middle step is deliberately free of SVG, so
  replacing the backend means rewriting only the last one.
- `src/term/` interrogates the terminal for its size in cells and in pixels, and
  for which graphics protocol it supports.
- `src/render/kitty.rs` serializes a framebuffer into escape sequences, both
  the one-shot `a=T` and the viewer's `a=t` transmit plus `a=p` placements.
  `encode` (compress) is split from `emit` (write) so the expensive half can
  run off the main thread; only `emit` touches the terminal.
- `src/tui.rs` is the viewer: the alternate screen, the key loop, and the map
  from zoom and pan to a source rectangle.

## Design constraints

Be stingy with dependencies, but not at the cost of the thing working out of
the box. Every format ships in the default build, which costs 106 crates and a
9.6M binary: hayro is most of the crates, resvg most of the rest, and markdown
adds only two beyond what resvg already dragged in, plus 1.2M of compiled-in
font. For comparison, `--no-default-features` is 18 crates.

Each format is still a named feature, so the cost is refusable. But those names
exist to be switched *off* with `--no-default-features`, not to be switched on:
a format nobody can use without rebuilding might as well not be supported. Be
stingy about what goes in the list at all, not about what a normal build gets.

SVG and PDF carry no pixel size of their own, so rasterize them straight at
display size. Scale a bitmap afterwards instead and you lose resolution the
source still had.

Markdown has no size in either direction: its height is a function of the width
it is given, so its font size is derived from the width asked for rather than
fixed. Scaling a long page down is never the answer: `geometry::fit` shrinks by
the tighter axis, which would reduce a document until the text was unreadable.

A document is therefore drawn a band at a time, which is what `Hints.from_y`
is for. The one-shot render asks for the top and nothing else. The viewer asks
for the band around what the reader can see, fits it on width alone, and asks
again with a new `from_y` when scrolling nears the edge of it.

The reason is not memory here but what a terminal will accept. It stores the
image, and a whole document is tens of megabytes as one texture — 63M for this
file in a large window — which a terminal may simply refuse. So keep a band
close to the size the one-shot path produces, since that is the size known to
work. Laying the document out to find the band costs nothing worth counting:
parse, layout and SVG emit together are about 270µs, against 61ms to rasterize
and 23ms to compress. Only the drawing scales with what is on screen, which is
the whole point.

For the same reason the viewer gives a document no `ZOOM_HEADROOM`. Headroom
buys real pixels to zoom into, which a photo has and a document does not: ask
markdown for double the width and it returns the same words at double the size,
to be drawn at half. That is four times the pixels for an identical-looking
page.

No terminal advertises what it will hold, so do not guess it. Transmit with
`q=1` so refusals come back instead of vanishing, and halve the band when one
does. Starting generous and backing off beats picking a number that suits the
stingiest terminal.

The viewer never re-encodes pixels. It transmits once, then pans and zooms by
sending a new source rectangle for the stored image, which the terminal scales.
A frame costs 69 bytes. Re-encoding instead measured 40-90ms and several
megabytes per frame, so think hard before moving redrawing back into Rust.

Documents are the one exception, and only because the alternative does not
work: a whole document is too large for a terminal to store. Even there most
frames are still the 69-byte kind, because a band holds more than the screen
shows and only running off the end of one costs a re-render.

Reuse one placement id and let the new placement replace the old one. Delete
the old one first and the background shows through the gap, which reads as a
flash on every keypress.

Decoding happens on a worker thread, and so does compressing, because the two
are comparable: on a large markdown page the zlib pass can cost more than the
render. Leaving compression on the main thread would freeze the spinner for
exactly the stretch it exists to cover. The viewer keeps only the write, since
only the write touches the terminal.

Say something during a wait, but not during a short one. A load under
`PATIENCE` finishes before the eye notices and flashing a spinner at it reads
as a stutter; past it, a blank alternate screen with the cursor hidden is
indistinguishable from a hang. Show elapsed seconds too: that is what tells
someone "slow" from "wedged". Because the work is off-thread, `q` answers
during a load rather than after it.

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

Over ssh only `TERM` crosses the hop, so detection falls to the probe, and
every question it asks now costs a network round trip. Measure one of those
first, with a device status report that every terminal answers, and size the
waits that follow from it. A terminal that leaves even that unanswered is
treated as unable to display images, which is also the right answer for a
multiplexer swallowing the sequences. `SSH_TTY` decides how long a reply may
take before detection gives up. The same latency splits a keypress across two
reads, so the viewer holds back a trailing escape instead of reading it as the
escape key and quitting on the first arrow press.

## Testing

Geometry and protocol serialization are pure functions, so unit-test them.
Decoders get small fixtures under `tests/data/`.

No test can tell you whether an image looks right; that needs a real terminal
and a human. `cargo run -- timg/img/sunflower-term.png` is the eyeball test, and
`cargo run -- CLAUDE.md` is the one for markdown.

Markdown has one test that is not about looks and still matters: a font that
fails to load renders a perfectly valid, perfectly empty page, so something has
to assert the framebuffer contains lit pixels at all.
