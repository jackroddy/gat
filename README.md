# gat

Show a picture in the terminal. PNG, JPEG, PDF, SVG and Markdown.

Requires a terminal that speaks the Kitty graphics protocol: kitty, Ghostty,
WezTerm or Konsole.

## Use

```
gat photo.png          # open the viewer
gat --print notes.md   # draw it where the cursor is and exit
gat --probe            # what detection saw of your terminal
```

`--print` also runs when stdout is not a terminal, so pipes and redirects work.

In the viewer:

```
hjkl or arrows   pan
g G              top, bottom
/ ?              search forward, backward
n N              next, previous match
} {              next, previous heading
tab shift-tab    next, previous file
+ -              zoom (pictures)
0                reset the view (pictures)
q                quit
```

gat draws markdown as a page rather than as styled terminal text: headings at
real sizes, tables, task lists, footnotes, callouts, and code blocks coloured
by language. Search still works, because the page keeps the text it was laid
out from.

## Install

```
cargo install gat
```

## Licence

MIT or Apache-2.0. The bundled Liberation Mono is under SIL OFL 1.1.
