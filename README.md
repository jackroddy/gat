# gat

Show a picture in the terminal. PNG, JPEG, PDF, SVG and Markdown.

Requires a terminal that speaks the Kitty graphics protocol: kitty, Ghostty,
WezTerm or Konsole.

## Use

```
gat photo.png          # viewer: hjkl or arrows pan, +/- zoom, n/p file, q quit
gat --print notes.md   # draw it where the cursor is and exit
gat --probe            # what detection saw of your terminal
```

`--print` also runs when stdout is not a terminal, so pipes and redirects work.

## Install

```
cargo install gat
```

## Licence

MIT or Apache-2.0. The bundled Liberation Mono is under SIL OFL 1.1.
