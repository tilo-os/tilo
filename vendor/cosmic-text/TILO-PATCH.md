# Tilo: variable fonts are shaped at their width

Source: crates.io `cosmic-text` 0.19.0, carried as it ships apart from the
changes below. Only the library is vendored: the test, bench and example
targets and their files stay upstream.

cosmic-text builds a face's HarfRust shaper instance (and its metrics) at a
`wght` location only, so a variable font drawn at another `wdth` was shaped,
measured and kerned at its default width. Tilo draws glyphs with the text's
own variation coordinates, so condensed text came out with the normal width's
advances: each letter narrow, the spacing loose (Tilo Sans at `wdth` 75).

- `src/font/mod.rs`: `Font::new_stretched` builds the instance at `wght` and
  at `wdth` = the requested [`fontdb::Stretch`]'s percentage (Condensed is
  75); `Font::new` is it at `Stretch::Normal`. A face without a `wdth` axis
  ignores the coordinate.
- `src/font/system.rs`: the font cache is keyed by face, weight and width;
  `FontSystem::get_font_stretched` beside `get_font` (the normal width).
- `src/font/fallback/mod.rs`, `src/shape.rs`: the fallback iterator carries
  the run's `Attrs::stretch` and shapes every face at it.

Glyphs that later look their font up again by weight alone (decoration
metrics, swash rasterisation) use the normal width, as before.

Remove this patch once upstream shapes variable fonts at the width a run asks
for (cosmic-text 0.19 applies `wght` only).

## Font stack on the versions the rest of Tilo uses

The manifest asks for `skrifa` 0.44 and `harfrust` 0.12 instead of 0.40 and
0.5, the versions `swash` and `vello_cpu` already pull in, so one copy of the
font parser ships instead of two (the web build included). `src/shape.rs`
passes its cached plan and features through `harfrust::ShapeOptions`, the
form `Shaper::shape` takes in harfrust 0.12.

Drop this once an upstream release depends on these versions.

## Seeking the layout runs of one line

`LayoutRunIter::skip_to_line` (`src/buffer.rs`) moves the iterator on to a
logical line's first run, leaving it exactly as calling `next` past every
earlier run would: the same tops, and the same end at an unshaped line or
below the buffer's height. Tilo's caret, arrow key and selection queries read
one paragraph of a long text; skipping the runs before it built a run for
every row above that paragraph on each query.

Drop this once upstream can start the runs at a line.

## Matching a run reads only the faces it considers

Upstream's `FontMatchKey::new` (`src/font/system.rs`) read the `wght` axis of
every face whose weight differs from the run's, for every new set of
attributes, so the first text set read a page of every font the process knew
about: on a cold disk, every font file before the first frame. The key no
longer carries that answer; `FontSystem::weight_fits` reads a face's axis when
the fallback iterator (`src/font/fallback/mod.rs`) has a face of the family it
wants, and keeps it per face. Which face a run gets is unchanged.

Drop this once upstream reads a face's axes only for a face it may use.

## A face's characters without its lookup tables

`Font::new_stretched` (`src/font/mod.rs`) gave up the face's monospace
fallback record, its character list included, when the face had no `GPOS` or
no `GSUB` table. The fallback iterator then found none of a word's characters
in the default monospace face and read every monospace face the system has
to look for them. The record now names the scripts of whichever tables the
face has, and always lists its characters.

Without the `monospace_fallback` feature, which Tilo leaves off, no face
keeps that list, and the same search read every system monospace face for
words the default monospace face holds whole.
`FontSystem::get_font_supported_codepoints_in_word` (`src/font/system.rs`)
now asks the face's character map (`Font::chars_in`) when it keeps no list.

Drop this once upstream builds the record without both tables and answers a
face's characters without the feature.

## Rows narrowed beside a floating picture

`RowBands` (`src/row_bands.rs`, new) narrows the rows whose top stands in a
band: a band carries the distance down to its bottom and the insets a row
takes off the left and right edges of the shaped width. `Buffer::set_row_bands`
takes them, and `BufferLine::layout` hands each `ShapeLine` its bands and the
height of the lines above it (`BufferInner::flow_start_y`), so a row is found
by where it stands in the buffer rather than by its index. `ShapeLine` reads a
row's width when the row starts and lays the row out between its insets, so the
glyphs carry their own offset: painting, hit-testing and caret geometry read
the same positions they always have. Empty bands lay every row across the whole
width, to the bit.

Tilo's rich-text editor sets these bands when a picture marked `wrap:left` or
`wrap:right` floats at an edge, and its following paragraphs run down beside
the picture until they pass its bottom.

Upstream lays a whole paragraph out at one width; drop this once it can narrow
some rows.
