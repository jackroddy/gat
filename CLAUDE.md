# gat

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
cargo build --profile dist                         # smaller, with a 50s link
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
the box. Every format ships in the default build, which costs 115 crates and a
14.2M binary, or 11.8M built with `--profile dist`: hayro is most of the
crates, resvg most of the rest, and markdown adds only two beyond what resvg
already dragged in, plus 1.2M of compiled-in font. For comparison,
`--no-default-features` is 18 crates.

Syntax colouring is 14 of those crates and 2.1M of that binary, which is why it
carries a feature of its own. The alternative was a hand-written highlighter
reading a table of keywords per language, which would have covered four
languages and been wrong on the rest. syntect's syntax and theme dumps
deserialize once behind a `OnceLock`, because each costs tens of milliseconds
and a page can hold many blocks. A fence naming no language, or one syntect has
no definition for, draws in the plain code colour.

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

gat never opens a path that a document asks for. Markdown image syntax keeps
its alt text and drops the destination, so viewing one file cannot make gat
read another, or reach the network.

This is what keeps `to_svg.rs` simple. Every attribute it writes is a number or
a colour gat picked; text from the document reaches the SVG only as character
data inside a `<text>` element, where `escape` handles it. Nothing from the
input is pasted into an attribute, so there is no second place to get the
escaping right.

Images can be added later, behind a Cargo feature, but two things go first.
`escape` has to cover `'`, because attributes here are single-quoted and a
filename carrying an apostrophe would close one early and write its own markup.
And the path rules have to be settled: files beside the document, no `..`
climbing out, no network, and a cap on decoded size.

The whole document is rasterized once, into memory, and the viewer transmits
it to the terminal a single time, in bands it never cuts again. Scrolling then
sends a new source rectangle for the pixels the terminal already holds, which
costs 69 bytes, or twice that for a window lying over a seam. Thirty scrolls
send no transmits at all and about 4.7KB.

Do not go back to drawing a screenful at a time. It was tried, and cutting a
new image whenever the view left the last one is what made scrolling drag: each
cut makes the terminal load an image and upload a texture, which is expensive
however cheap our side of the cut becomes. Measured against a terminal doing
real work, twenty-five scrolls that way asked it to load over 200MB.

Measure what the terminal is asked to do, not what gat spends. A harness that
reads the escapes and discards them will report a keypress answered in zero
milliseconds while the terminal behind it is loading megabytes per frame. That
mistake cost a day.

The viewer gives a document no `ZOOM_HEADROOM`. Headroom buys real pixels to
zoom into, which a photo has and a document does not: ask markdown for double
the width and it returns the same words at double the size, to be drawn at
half. That is four times the pixels for an identical-looking page.

Transmit with `q=1` so a refusal comes back instead of vanishing, and put what
comes back in the status line. That is how `EINVAL: unsupported medium` was
found. `q=2` suppresses failures along with successes, which turns a refused
image into a blank screen with nothing to explain it.

The viewer never re-encodes pixels. It transmits once, then pans and zooms by
sending a new source rectangle for the stored image, which the terminal scales.
A frame costs 69 bytes. Re-encoding instead measured 40-90ms and several
megabytes per frame, so think hard before moving redrawing back into Rust.

Search scrolls, and does not highlight. The page is one image the terminal
already holds, so drawing a box around a match means re-encoding and
re-transmitting it, which is what the rest of the viewer is built to avoid. A
hit moves the view and the status line carries the line it was found on, for
the same 69 bytes a pan costs.

What makes that possible is layout's own output, kept rather than recomputed.
`lines_of` reads the text back out of the positioned runs in the display list,
so there is no second copy of the page's words to keep in step with the first.

`--print` takes its image ids from one block and deletes that whole block
before it draws, so a run's images replace the last run's instead of joining
them. Before that, every run transmitted under a fresh id and nothing was ever
deleted, and a day of printing stayed in the terminal's memory. `--keep` skips
the delete and takes fresh ids, for a scrollback you want to keep.

The delete is `a=d,d=R`, and the capital is what frees the pixels: the
lowercase form drops the placements and keeps the data so it can be shown
again. A terminal will only free an image the scrollback no longer refers to,
which is why deleting the block is the whole mechanism rather than half of it.
The placements go with it.

A run has to fit in the block. That is a million ids against one per picture
and at most five per document, since a page is capped at `MAX_PIXELS` and cut
into `CHUNK_PIXELS` pieces, so a terminal runs out of memory for the pixels
long before a run runs out of ids. Pick the base high: a low id is what a
client that has not thought about ids will choose, and the block is deleted
wholesale.

Two runs at once in the same terminal share the block, so the second wipes the
first. That is the price of the default. Whether the terminal hands the memory
back is then its own business, so measure it rather than assuming.

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

## Multiplexers

A document drew as a blank page under herdr while a picture in the same viewer
drew fine and `--print` worked everywhere. The cause was size, and the
mechanism is worth keeping because nothing anywhere reports it.

herdr estimates an image's base64-encoded size and drops any asset over
`MAX_GRAPHICS_FRAME_SIZE - MAX_FRAME_SIZE`, which is 32 MiB less 2 MiB. The sum
runs on decoded RGBA, so compressing better does not help: a page of 169 KiB on
the wire is dropped exactly as one of 1.4 MB when both decode to the same
pixels. The drop is a bare `continue` in `kitty_graphics/surface.rs` with no
error reply and no line in `herdr-server.log`, and the placement survives it,
so the pane reserves the space and paints nothing. At 1267 pixels across, the
ceiling works out at 4637 rows; measured, 4576 drew and 4800 did not.

So the viewer sends a page in bands of `BAND_PIXELS`, each transmitted once
under an id of its own, the way `--print` already cut its pieces. A band is cut
on whole terminal rows, so the page stays on the row grid across a seam, and a
window lying over one places both sides of it for a second placement and no
pixels. This is not the screenful-at-a-time design that made scrolling drag:
bands are cut once when the page loads, never again as it scrolls.

A band is capped at 10000 pixels a side as well. That is kitty's
`max_dimension`, inherited by Ghostty and by the Ghostty core herdr is built
on, and a narrow page can sit under the byte budget and still run past it.

Do not trust what a multiplexer says about itself. herdr answers a kitty
file-transfer query with `OK` deliberately, so that applications detect the
capability, while refusing the transfer that follows. gat sent `t=f` once and
built a feature on that `OK`; the transport has since gone and the direct
medium was never the problem. A query is not evidence of anything; only a
transmission is.

tmux was blank too and has not been retried since any of this.

`gat --probe` carries the ladder. It stores and places an image at a range of
heights the way the viewer does and prints what came back, reading the replies
through a `Probe`. An earlier version left them on the tty, the shell took most
of them, and a size limit was read out of the one answer that survived.

## Testing

Geometry and protocol serialization are pure functions, so unit-test them.
Decoders get small fixtures under `tests/data/`.

No test can tell you whether an image looks right; that needs a real terminal
and a human. `cargo run -- timg/img/sunflower-term.png` is the eyeball test, and
`cargo run -- CLAUDE.md` is the one for markdown.

Markdown has one test that is not about looks and still matters: a font that
fails to load renders a perfectly valid, perfectly empty page, so something has
to assert the framebuffer contains lit pixels at all.
