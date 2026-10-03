# Tilo Sans

Tilo's UI typeface: [Instrument Sans](https://github.com/Instrument/instrument-sans)
by Rodrigo Fuenzalida and Jordan Egstad, extended with the scripts and forms the
Tilo desktop needs (Vietnamese, distinct I/l for identifiers, Greek, Cyrillic).

Licensed under the [SIL Open Font License 1.1](OFL.txt). Instrument Sans declares no
Reserved Font Name; the extended family is renamed anyway so it can never be mistaken
for upstream's files. The name table credits Instrument Sans and its authors in the
copyright (ID 0), designer (ID 9) and description (ID 10) strings.

## Provenance

| | |
|---|---|
| Upstream | https://github.com/Instrument/instrument-sans |
| Pinned commit | `7fa22308a3d0c94ee2b3cd537a1196b65db34a3e` (2023-06-14, "feat: updated example gif") |
| Upstream sources | `sources/InstrumentSans.glyphs`, `sources/InstrumentSans-Italic.glyphs` (Glyphs 3) |
| Converted with | glyphsLib 6.15 via `scripts/import_upstream.py` (one-time; see below) |

The upright variable font rebuilt here matches upstream's
`fonts/variable/InstrumentSans[wdth,wght].ttf` (byte-identical to the
`InstrumentSans.ttf` Tilo used before) glyph for glyph: same glyph set, cmap, advances,
outlines (point by point at all 12 named locations) and shaping (every character,
every letter pair, mark stacks, each GSUB feature and `locl` language). The italic
matches upstream's italic the same way. The one difference: `bullet.ss01` and
`bullet.case.ss01` (scaled `period.ss01` components) are now decomposed before the
cubic-to-quadratic conversion, so their circles carry a few more points; the shape is
the same within one unit. `scripts/verify_upstream.py` checks all of this.

## Layout

```
vendor/fonts/tilo-sans/
  build.py                 compile sources/ -> fonts/ (variable TTFs)
  proof.py                 render proofs with headless Chromium -> proofs/
  scripts/measure.py       metrics, stems, overshoots, sidebearings per master
  scripts/verify_upstream.py   every upstream glyph unchanged?
  scripts/import_upstream.py   how sources/ was made from upstream (do not rerun)
  scripts/greek/           the Greek: outline generator, kerning, checks (see Greek)
  sources/
    TiloSans.designspace           upright: 4 masters
    TiloSans-Italic.designspace    italic: 4 masters
    TiloSans-{CondensedRegular,CondensedBold,Regular,Bold}.ufo
    TiloSans-{CondensedItalic,CondensedBoldItalic,Italic,BoldItalic}.ufo
    features/*.fea                 OpenType code, one file per feature, shared by all 8 masters
  fonts/        (ignored)  TiloSans[wdth,wght].ttf, TiloSans-Italic[wdth,wght].ttf
  proofs/work/  (ignored)  every proof run
  proofs/sheets/           proofs written with --final; the only tracked proofs
  .upstream/    (ignored)  upstream checkout for import/verify
```

Every glyph is one `.glif` file per master, so parallel work on different glyphs
merges cleanly. Each master's `features.fea` only `include`s `features/*.fea`.

## Commands

All tools are [uv](https://docs.astral.sh/uv/) scripts that declare their own
dependencies (PEP 723); run them from the repository root.

```sh
uv run vendor/fonts/tilo-sans/build.py              # lint + interpolation check + compile both (~10 s)
uv run vendor/fonts/tilo-sans/build.py --upright    # or --italic
uv run vendor/fonts/tilo-sans/build.py --fast       # skip the interpolation check while iterating
uv run vendor/fonts/tilo-sans/build.py --check      # lint only, no compile

uv run vendor/fonts/tilo-sans/proof.py --set vietnamese
uv run vendor/fonts/tilo-sans/proof.py --set greek --fallback geologica
uv run vendor/fonts/tilo-sans/proof.py --set cyrillic --fallback onest
uv run vendor/fonts/tilo-sans/proof.py --set cyrillic --lang bg --name cyrillic-bg   # locl forms
uv run vendor/fonts/tilo-sans/proof.py --chars "Il1|" --features ss13 --name il
uv run vendor/fonts/tilo-sans/proof.py --set vietnamese --italic
uv run vendor/fonts/tilo-sans/proof.py --set latin --fallback instrument           # regression vs upstream

uv run vendor/fonts/tilo-sans/scripts/measure.py [--glyphs n o A-cy]
uv run vendor/fonts/tilo-sans/scripts/verify_upstream.py
```

`verify_upstream.py` and `proof.py --fallback instrument` need the upstream
checkout; `uv run vendor/fonts/tilo-sans/scripts/import_upstream.py` clones it
(and refuses to touch `sources/` without `--force`).

### Proofs

`proof.py` writes 2x PNGs to `proofs/work/<name>/` (`--final` writes to
`proofs/sheets/<name>/`, which git tracks):

| page | what |
|---|---|
| `grid-<instance>[-n].png` | every glyph at 160 px in its advance box (tinted) with baseline, x-height, cap height, ascender, descender; glyph name and code point; 36 per page; one set per instance: Regular, Medium, SemiBold, Bold (wdth 100), Condensed Regular, Condensed Bold (wdth 75). A code point the font lacks is a red cell labelled MISSING. |
| `text-light.png`, `text-dark.png` | sample text at 13, 14, 16, 20, 26 px, body at 400 and a 600 heading, in Tilo's light and dark surface colours |
| `compare-<fallback>.png` | the glyphs (96 px, 400 and 700; 48 px) and the samples (13–26 px) in Tilo Sans and the fallback, the fallback scaled to Tilo Sans' x-height (510/1000) |
| `context-<style>.png` | each glyph between its script's control letters (`HH?HOH?OHO?OO`, `nn?non?ono?oo`; Cyrillic Н О н о; Greek Η Ο π ο) and the samples, at Regular, Bold and Condensed Regular 32 px and Regular 14 px |

Fallbacks: `onest` (Onest with `ss02`, same weight), `geologica` (Geologica at
0.85x the weight: 340 for Regular), `noto` (Noto Sans from the UI toolkit's
assets), `instrument` (upstream), or a path to any font. Default per set:
Greek → geologica, Cyrillic → onest, Vietnamese → noto, else instrument.
Characters Tilo Sans lacks are struck through in red on every page; Chromium would
otherwise draw them from a system font. The proof refuses to run on a font older
than its sources.

Look at proofs with an image viewer at full size, at both 160 px and the UI sizes,
light and dark, and across weights and widths.

## Design space

Axes as upstream; user and design coordinates coincide except one width point.

| axis | tag | min | default | max | map (user → design) |
|---|---|---|---|---|---|
| Width | `wdth` | 75 | 100 | 100 | 75→75, 88→87.5, 100→100 |
| Weight | `wght` | 400 | 400 | 700 | linear |

Masters (one UFO each; every glyph must exist in all of them, compatible):

| UFO | wdth | wght |
|---|---|---|
| `TiloSans-CondensedRegular.ufo` / `TiloSans-CondensedItalic.ufo` | 75 | 400 |
| `TiloSans-CondensedBold.ufo` / `TiloSans-CondensedBoldItalic.ufo` | 75 | 700 |
| `TiloSans-Regular.ufo` / `TiloSans-Italic.ufo` (default) | 100 | 400 |
| `TiloSans-Bold.ufo` / `TiloSans-BoldItalic.ufo` | 100 | 700 |

Named instances in `fvar`: Regular 400, Medium 500, SemiBold 600, Bold 700 at
wdth 100 (italic: Italic, Medium Italic, SemiBold Italic, Bold Italic). STAT also
labels Condensed (75) and SemiCondensed (87.5), and `ital` 0/1 (Roman/Italic).
There is no weight below 400 and no width above 100: there are no Thin or Expanded
masters.

## Metrics a new glyph must match

UPM 1000. Identical in every master: x-height 510, cap height 720, ascender 740
(`b d l` tops sit at 720, `d`'s top anchor at 740), descender −205, overshoot 10 above
and below for round shapes (o, O), italic angle 13° (UFO `italicAngle` −13).
Line metrics: typo/hhea ascender 970, descender −250, line gap 0, win 986/350,
`USE_TYPO_METRICS` set.

Stems (font units; italic stems measured across the stroke). From
`scripts/measure.py`:

| master | n stem | o side | o top/bottom | H stem | H bar | O side | O top/bottom |
|---|---|---|---|---|---|---|---|
| Condensed Regular | 68 | 70 | 60 | 70 | 62 | 72 | 64 |
| Condensed Bold | 138 | 142 | 108 | 142 | 110 | 144 | 112 |
| Regular | 80 | 82 | 68 | 82 | 72 | 84 | 74 |
| Bold | 150 | 156 | 118 | 154 | 118 | 156 | 122 |
| Condensed Italic | 66 | 69 | 62 | 68 | 62 | 73 | 67/66 |
| Condensed Bold Italic | 134 | 138 | 120 | 138 | 110 | 139 | 122/121 |
| Italic | 78 | 80 | 69/70 | 80 | 72 | 83 | 76 |
| Bold Italic | 146 | 152 | 123/124 | 150 | 118 | 152 | 126 |

Spacing references (advance, LSB, RSB; italic sidebearings measured on the outline
slanted back upright about half the x-height, as Glyphs shows them; the italic
spacing equals the upright's):

| master | n | o | H | O |
|---|---|---|---|---|
| Condensed Regular | 442, 64, 60 | 432, 36, 36 | 500, 70, 70 | 500, 40, 40 |
| Condensed Bold | 462, 44, 40 | 460, 21, 21 | 514, 50, 50 | 536, 30, 30 |
| Regular | 599, 80, 74 | 584, 40, 40 | 736, 86, 86 | 786, 50, 50 |
| Bold | 619, 60, 54 | 604, 20, 20 | 720, 50, 50 | 798, 30, 30 |

## Glyph naming

Glyphs.app "nice names", as upstream: `A`, `aacute`, `idotless`, `Ohorn`,
`Ecircumflexacute`, `A-cy`, `be-cy`, `Alpha`, `alphatonos`, `iotadieresistonos`,
`numero`. Combining marks end in `comb` (`acutecomb`, `dotbelowcomb`,
`hookabovecomb`); ligatures join components with `_` (`f_i`). Suffixes:
`.ss01`–`.ssNN` (stylistic set), `.case` (capital forms), `.tf` (tabular), `.alt`,
`.liga`, `.loclBGR` / `.loclSRB` / `.loclMKD` / `.loclCAT` (language forms); stack them
in feature order (`Gcommaaccent.ss01.ss07`). The italic uses the same names; there
are no italic-only glyph names.

Production (post table) names are derived at build time: `A-cy` ships as `uni0410`,
`Ecircumflexacute` as `uni1EBE`, `Be-cy.loclBGR` as `uni0411.loclBGR`. Do not add
`public.postscriptNames` entries by hand. File names inside the UFO follow the UFO
convention (an underscore after each capital: `Aacute` → `A_acute.glif`,
`A-cy` → `A_-cy.glif`); let the UFO library pick them.

Stylistic sets in use: ss01 Round dots, ss02 Single Story a, ss03 Alternate y,
ss04 Alternate K, ss05 Alternate R, ss06 Alternate M, ss07 Alternate G, ss08
Alternate J, ss09 Alternate Q, ss10 Alternate 2, ss11 Alternate Ampersand, ss12
Alternate Currency. New sets start at ss13.

## Anchors

Bases: `top`, `bottom`, `ogonek`, `center` (for strokes: Đ, Ł, Ħ), `topleft`,
`topright` (caron-as-apostrophe: ď ľ ť). Marks: `_top` + `top` (stackable),
`_bottom` + `bottom`, `_ogonek`, `_topright` (`caroncomb.alt`). Ligatures:
`top_1`…, `bottom_1`…, `caret_1`… . Marks attach at `_top` y = 510 (x-height) and
carry their own `top` for the next mark (`acutecomb` `top` at 740); a capital's
`top` is at 720, so the same `acutecomb` serves lower and upper case (there are no
`.case` marks yet).

The build propagates anchors into composites (Glyphs' algorithm): `Aacute` = `A` +
`acutecomb` gets `A`'s `bottom`/`ogonek` and a `top` on the acute, so a further mark
stacks on it. Draw anchors on simple glyphs and marks; put anchors on a composite
only to override. Spacing accents (`acute`, `tonos`, …) get none. `mark`, `mkmk`,
`kern` and GDEF are generated from anchors, categories and kerning; never write
them by hand.

## Adding a glyph

1. Draw it in **all four masters of its designspace** (upright and italic are
   separate designspaces: a new letter needs eight drawings). Same contours, same
   point counts and order, same start points and directions, same components and
   anchors in every master; only coordinates differ.
2. Set the advance, the code point (`<unicode hex="0410"/>`; none for alternates)
   and anchors. Combining marks have advance 0.
3. Make sure each `glyphs/contents.plist` lists it (any UFO library does this;
   `build.py` fails on a `.glif` that is not listed). Two branches adding
   different glyphs merge `contents.plist` cleanly (`merge=union` in
   `.gitattributes`).
4. Kerning: join the `public.kern1.*` (left side of a pair, i.e. the glyph's right
   side) and `public.kern2.*` (its left side) groups whose shapes it shares —
   Cyrillic А, В, Е, К, М, Н, О, Р, С, Т, Х and Greek Α, Β, Ε, Ζ, Η, Ι, Κ, Μ, Ν, Ο,
   Ρ, Τ, Υ, Χ belong in the Latin groups (`KO_A`, `KO_H`, `KO_O`, …); the kern
   writer splits pairs per script. Groups and kerning live in each master's
   `groups.plist` (same in all masters) and `kerning.plist` (per master).
5. Features: edit `sources/features/<feature>.fea` (one file, all 8 masters). A new
   script needs its `languagesystem` lines in `languagesystems.fea`
   (`languagesystem cyrl dflt;`, `languagesystem cyrl BGR;`, `languagesystem cyrl SRB;`,
   `languagesystem grek dflt;`), otherwise kerning and `locl` never reach it. Add new
   combining top marks to `@CombiningTopAccents` in `ccmp.fea` so `i`/`j` lose their
   dot under them.
6. `build.py`, then `proof.py`, then `scripts/verify_upstream.py` (must stay clean:
   a Latin change shows up by glyph name).

Glyph order, production names and GDEF classes for new glyphs are filled in by
`build.py`; `lib.plist` does not need editing. A glyph whose Glyphs category is
wrong can be overridden with `com.schriftgestaltung.Glyphs.category` /
`.subCategory` in the glyph's `lib`.

## Greek

Modern monotonic Greek, upright and italic, all masters: Α–Ω, α–ω with ς, the
tonos and dialytika letters (Ά Έ Ή Ί Ό Ύ Ώ Ϊ Ϋ, ά έ ή ί ό ύ ώ ϊ ϋ ΐ ΰ), tonos
΄, dialytika-tonos ΅, ano teleia ·, the Greek question mark ; and the numeral
signs ʹ ͵ (`numeral-greek`, `lowernumeral-greek`); beside them MICRO SIGN µ (`micro`,
the Greek μ) and HORIZONTAL BAR ―, the Greek dialogue dash (`horizontalbar`,
the em dash, with a `.case` form).

**The Greek glyphs are generated.** `scripts/greek/gen.py` draws every one of
them into all eight masters from that master's own Latin parts and measured
stems, and overwrites their `.glif` files. Change a Greek glyph in the
generator, never in the `.glif`, or the next run loses the edit:

```sh
cd vendor/fonts/tilo-sans/scripts/greek
uv run --with ufoLib2 --with fonttools python gen.py [glyph ...]   # all, or the named glyphs
uv run kern.py [--report]          # Greek kerning groups and pairs, all masters
uv run view.py out.png --masters Bold,CondensedBold --glyphs "beta xi"   # sources, no build
uv run check.py                    # after build.py: pair kerning, substitutions, accent overhang
```

* Shared shapes: Α Β Ε Ζ Η Ι Κ Μ Ν Ο Ρ Τ Υ Χ and ο are components of the Latin
  glyphs; η comes from n, μ from u, κ from k, ι and τ from t, α from the
  single-storey a with t's tail, ρ σ from o, ν γ from v, λ's long stroke from
  y's turned over, Γ Δ Λ Π Ξ from F, A, H and E's stems and bars (Δ's base
  ends flush with the diagonals). Bowls (δ θ φ Φ) are the master's o or O
  stretched, so they carry Instrument's curve tension, and in the italic its
  corrected italic o.
* Drawn shapes (β γ ε ζ ξ ς υ ψ ω Ψ Ω) are skeleton strokes outlined with an
  elliptical pen using o's vertical and horizontal weights; terminals are cut
  across the stroke like c's and j's, and the descender hooks of ζ ξ ς taper.
  Joins are thinned where strokes meet (β's waist and stem joins, δ's neck,
  ψ and ω's bottoms, φ's bowl where the stem crosses it), more in Bold. Heavy
  masters lighten small bowls (β's and ε's upper bowls, φ, ψ) and widen the
  letters whose counters would otherwise close (φ Φ ψ Ψ ω θ in Condensed Bold).
* ζ and ξ leave their top bar from its right end, the bar ending flush with the
  stroke; ζ's spine stays diagonal well down and turns late into the bowl, with
  a small round under the bar in the heavy masters. ε's and ξ's tongue is
  where the two bowls part, run on a little and cut leaning back, not a bar.
  β is one stroke: the stem rolls over into the upper bowl, pinches at the
  waist and the lower bowl runs back into the stem. γ's right arm runs on
  without a break into a tail that leans a little left, the left arm joining
  it above the baseline. λ's short leg joins the long stroke a little under
  half way up, its foot well in, so the base is narrower than Λ's. υ and ω
  lean their arms in over the top quarter (by a share of the counter, less in
  Condensed Bold); ω's middle stroke is short, light and set high. Ω's sides
  turn in below the middle and run, still curving and a little lighter, into
  the feet.
* Italic masters are drawn upright and sheared 13°. Strokes are pre-compensated
  so the shear keeps the upright's weight distribution (a plain shear thickens
  `\` strokes and thins `/` strokes by about 11%).
* The tonos is the acute stood up. Over a capital it sits in front of the letter
  at cap height; Ε Η Ι Ο set the white beside it, Υ and Α (whose sides run away
  from it) match Ε's mean white, and Ω's arch comes a little closer than Ο.
  Υ's tonos drops beside the arm, its top under the arm's tip, so it holds the
  same white as Έ. In ΐ ΰ ΅ the tonos stands up between the dots, narrower and
  steeper in the heavy masters, with its foot between the dots' tops; the dots
  keep the dieresis spacing where they can (drawn in, and 8% smaller in
  Condensed Bold). No accent hangs more than about 36 units
  past the advance: ϊ ϋ Ϊ Ϋ ΐ ΰ take extra sidebearing where needed.
* Kerning (`scripts/greek/kern.py`): each Greek glyph joins the Latin
  kerning groups of the Latin glyph its sides match (Γ's arm kerns like T's,
  Δ Λ like A, ρ like p then o, υ ω like o on the right, ι like t, λ like A on
  the right and l on the left, accented letters like the Latin letter with
  the same accent). The kern writer splits pairs per script, so Greek and Latin
  never kern against each other. Where Instrument kerns the Latin analogue as
  a single glyph (T-u is a glyph pair), the Greek pair gets the same value as
  a glyph pair, so Τυ Τω Τμ Γω Υω kern like Tu. Where a Greek side is only
  partly like its analogue the pair is set from the outlines against the
  analogue's white: ζ ξ before lowercase take c's white plus about 40 units
  (their body sits well left of the bar and hook); υ ω before ν γ χ take u's
  white before v and x; Γ's long arm closes over x-height letters to the white
  T's arm keeps over the same letter; an x-height bar (π σ on the right, π τ on
  the left) keeps at least the white between two t's bars, so πτ never reads
  as one bar. A collision guard then opens any Greek letter pair whose
  outlines come closer than nearly all Latin pairs do in that master, and a
  punctuation guard keeps a Greek letter at least 70% as far from brackets,
  quotes and figures as its Latin analogue ((ζ no longer gets (c's kern).
  Deliberate exceptions: φχ φλ λρ 0, T-like arms before a capital with tonos
  +20 to +40, opening brackets, quotes and guillemets before a capital with
  tonos −45 (the tonos hangs into the space: «Έκθεση, (Άδεια).
* Features (`features/greek.fea`): `languagesystem grek dflt`; the Greek
  question mark follows the capitals under `case`; `ss01` (round dots) covers
  ϊ ϋ Ϊ Ϋ ΐ ΰ ΅, the ano teleia and the question mark; and `locl` for Greek
  gives U+00B7 MIDDLE DOT the ano teleia's form when it follows a Greek letter
  directly, since NFC turns U+0387 into U+00B7 and Greek keyboards type it. A
  middle dot with a space before it is a separator (`Αρχεία · Ρυθμίσεις`,
  `12 MB · 3 λεπτά`) and stays centred. `case` also covers ―. Dropping the
  tonos in all-caps text (and adding the dialytika it can call for) is left
  to the text layer's Greek uppercasing.

## Editing

UFO is a plain-text format: any UFO editor works (Fontra, RoboFont, FontForge,
Glyphs.app's UFO import), as do scripts with
[ufoLib2](https://github.com/fonttools/ufoLib2) and hand-edited `.glif` XML. Keep
coordinates integral. `glyphsLib.filters.eraseOpenCorners` runs at build time, so
Glyphs-style open corners are fine.

## Rebasing on a newer upstream

`scripts/import_upstream.py --force` regenerates `sources/` from
`UPSTREAM_COMMIT`; it overwrites every master. Bump the commit, run it on a
branch, then re-apply the Tilo Sans glyphs from git history. It also resets two
artefacts of today's glyphsLib to what upstream shipped: the 0.13° slant on the
italic `i`/`j` components, and base classes on spacing accents.

## CI

Changes under `vendor/fonts/` do not count as Cargo inputs
(`tools/ci/affected_packages.py`, `tools/apps/app_release.py`, the `app-sdk`,
`release-apps` and `build-image` workflows) and are excluded from the image build
context (`.containerignore`). No crate reads these files; shipping a build of Tilo
Sans means copying `fonts/*.ttf` into the toolkit's font assets in a separate
change.
