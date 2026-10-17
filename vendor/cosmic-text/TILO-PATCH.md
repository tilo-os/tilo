# Tilo: variable fonts are shaped at their width

Source: crates.io `cosmic-text` 0.19.0, carried as it ships apart from this
change. Only the library is vendored: the test, bench and example targets
and their files stay upstream.

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
