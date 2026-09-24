# Changelog

This file records every notable change to gat.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-09-23

### Added

- `-p`, short for `--print`.
- `--cap N`, the most pixels an image gat sends may have: 1048576 unless the
  settings file or the flag says otherwise, written as a count or with K or M,
  as in `2M`. A picture over the cap is shrunk once with its aspect kept, and
  the terminal scales it back up into the cells it would have covered. SVG and
  PDF are rasterized at the capped size directly. A document's width is held to
  the square root of the cap, and its text stays one terminal row per line and
  one cell per character.
- A settings file, `$XDG_CONFIG_HOME/gat/config.toml` or
  `~/.config/gat/config.toml`, with `mode` for whether a run opens the viewer
  or prints, and `cap`. gat writes one with both settings commented out the
  first time it runs without one. A flag wins over the file, and a mistake in
  the file is an error naming its line.
- `-i`, `--interactive`, which opens the viewer when the settings file says
  print. With stdout not a terminal it is an error, where a run without it
  prints.

### Changed

- gat no longer sends an image at the full resolution of a large window. Past
  the cap, a picture, or a page at a large font size, goes out with fewer
  pixels and the terminal enlarges it, which can look softer than before.
- A release build, which is what `cargo install` makes, takes about 17 seconds
  where it took 51, or under a second when only gat's own code has changed, and
  comes out at 14.2M where it was 11.8M. `cargo build --profile dist` builds
  the smaller binary the old way.

## [0.2.0] - 2026-09-16

### Added

- Tables, task lists, footnotes and GitHub callouts in Markdown. A table is
  measured in characters and its widest column shrinks first, so a column of
  prose wraps before a column of short keys is squeezed.
- Code blocks coloured by language through syntect, behind the `syntax`
  feature. A fence naming no language draws in one colour as before.
- Search in the viewer. gat underlines the matches over the page as terminal
  text, which costs no pixels, and steps through headings the same way.
- Vim's keys: `hjkl` and the arrows pan, `g` and `G` are the ends, `/` and `?`
  search, `n` and `N` step matches, `}` and `{` step headings, tab and
  shift-tab change file.
- `--keep`, which leaves the images of earlier runs in the terminal.
- Bare URLs take the link colour, and YAML or TOML front matter is dropped
  instead of drawing as a rule and a paragraph of fields.

### Changed

- gat lays a markdown page out on the terminal's own grid. Every line box is a
  whole number of rows and starts on a whole column, and the type sits on the
  baseline the terminal draws its own text on.
- gat draws a document at 1:1 and scrolls it by whole rows. Only pictures zoom
  now, since magnifying a rasterized page returns the same words larger.
- `--print` draws its ids from one reserved block and clears that block before
  it starts, so a run replaces the images of the run before it. A terminal no
  longer holds every image it has ever been sent.
- The viewer sends a page in bands, each transmitted once. Panning still costs
  one placement, or two across a seam.

### Fixed

- A long document drew as a blank page under herdr, which drops an image whose
  estimated size passes 30 MiB and reports nothing, in its protocol or its log.
  Bands keep every image well under that.
- A page could pass the 10000 pixel side limit that kitty, Ghostty and herdr
  all enforce. No terminal would have drawn it.

### Security

- A filename or a decoder's error can no longer write escape sequences to the
  terminal. gat drops control characters from the status line, C1 included.
- A document cannot name a file. Markdown image syntax keeps its alt text and
  discards the destination, so viewing one file cannot make gat read another or
  reach the network.
- Text from a document reaches the SVG only as character data, escaped, and
  nothing a document contains is written to the terminal as characters.

## [0.1.0] - 2026-09-15

### Added

- Show PNG, JPEG, PDF, SVG and Markdown in a terminal that speaks the Kitty
  graphics protocol. PDF goes through hayro by default, or pdfium behind the
  `pdf-pdfium` feature.
- A viewer on the alternate screen that pans and zooms. It transmits a page
  once and scrolls by asking the terminal for a different part of the pixels it
  already holds, so a keypress costs 69 bytes.
- `--print`, which writes an image where the cursor is and exits. It runs
  whether or not stdout is a terminal, so pipes and redirects work.
- Markdown drawn as a page rather than styled terminal text, at the size of the
  terminal's own text, and written into the scrollback in pieces.
- `--probe`, which reports what terminal detection saw, and `--force-kitty`,
  which skips detection.
- Terminal detection over ssh. It sizes its waits from one measured round trip
  instead of a fixed timeout.
