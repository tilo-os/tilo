# Tilo canonical fonts

The manifest records each file's source (an installed Fedora 44 binary path
and package NEVR, an immutable upstream URL, or a path in this repository and
the commit it was built from), complete SHA-256 values, metadata, and exact
license mappings. Every file was copied without modification except Tilo
Sans, built from its own source repository at a recorded tag, and the Cyrillic fallback
subset, which `tools/font-fallback-subsets.py` derives reproducibly from a
pinned upstream file (below). Scopes are independent of whether a font
happens to be installed on a host. The 35 `product` fonts, the SDK's
standalone catalog, total 54,113,192 bytes; the `os` font New Computer
Modern Regular brings the Tilo OS catalog to 54,830,780 bytes; Adwaita adds
879,796 fixture bytes.

## Tilo Sans (UI face)

Tilo Sans is the system UI face: Instrument Sans (Rodrigo Fuenzalida and
Jordan Egstad, OFL 1.1, no Reserved Font Name) extended with Vietnamese,
Greek, Cyrillic and distinct I and l. Its sources, build and proof tools live
in https://code.tilo.so/tilo/tilo-sans. The catalog ships its default build:
Latin, Vietnamese, Greek, and the distinct I/l (`ss13`, `cv01`, `cv02`) and
slashed zero (`zero`) features, which the toolkit applies by capability for
identifiers and "Always distinguish similar letters". Cyrillic is held back
until review approves it and falls back to Onest (below).

| file | bytes | SHA-256 |
|---|---:|---|
| `TiloSans[wdth,wght].ttf` | 325400 | `903aac545b421e1c050552980762613e9373b4a01b491e45c9737f42525888d6` |
| `TiloSans-Italic[wdth,wght].ttf` | 338224 | `72d98aff60fd0447e5081c48895b08656aa9f2b219c0698866e97f3cd20c5743` |
| `LICENSES/Tilo-Sans-OFL.txt` (tilo/tilo-sans `OFL.txt`) | 4448 | `63a4ce1628d9c43e728a77e40d27529f036a7bca9f50c65be96f4cf0a9aa5f98` |

Both fonts are tilo/tilo-sans' `fonts/default/` at tag `v1.002` (commit
`025d4613d723e0da70d0d2d99e39fbb781f9f45f`), built with

```sh
git clone https://code.tilo.so/tilo/tilo-sans && cd tilo-sans && git checkout v1.002
SOURCE_DATE_EPOCH=1791160331 uv run build.py
```

`SOURCE_DATE_EPOCH` is that commit's time; it sets `head.modified`, the only
part of the output that otherwise follows the clock. The repository pins its
build dependencies (fonttools 4.66.1, ufo2ft 3.9.1, ufoLib2 0.18.1, glyphsLib
6.15.0, skia-pathops 0.9.2, uharfbuzz 0.56.2; `build.py.lock` the rest), so
the rebuild reproduces these bytes; the same files are attached to the
`v1.002` release there. `scripts/shaping_tests.py` passed on the build.

Both fonts embed `Copyright 2022 The Instrument Sans Project Authors
(https://github.com/Instrument/instrument-sans). Copyright 2026 The Tilo Sans
Project Authors.` and an OFL 1.1 notice with no Reserved Font Name; their
name-ID 0, 13 and 14 records are appended to
`LICENSES/FONT-COPYRIGHT-NOTICES.txt`. The family is renamed from upstream's,
so it can never be taken for upstream's files.

Instrument Sans was chosen over Noto Sans, Mona Sans and Atkinson
Hyperlegible Next by rendering the same screen in each at the type roles'
sizes on the vello renderer: it is the most refined at 20-34 px, compact and
clear at 12-14 px, has tabular figures and a 400-700 weight axis covering
every role weight, and carries no Reserved Font Name. Tilo Sans keeps its
Latin outline for outline.

To ship a newer build (a script or feature review has approved): take the
new tilo/tilo-sans release, rebuild it as above to confirm its bytes, copy
`fonts/default/*.ttf` here, and update the manifest (both copies), this file
and `LICENSES/FONT-COPYRIGHT-NOTICES.txt` with the tag, commit and hashes. The
toolkit's held-back-feature test face
(`crates/sdk/ui-core/tests/fixtures/fonts/TiloSansFeaturesFixture-Regular.ttf`)
comes from the same repository's `scripts/toolkit_fixture.py`.

## Reading faces

The `Reading` role's default face is New Computer Modern (Antonis
Tsolomitis); Libertinus Serif, Literata, EB Garamond and Source Serif 4 are
the reading faces a theme or a reader can choose instead
(`docs/typography.md`, Reading faces). They replace Noto Serif, which the
catalog no longer carries. Every file is copied unmodified from its official
release; `tools/font-reading-faces.py` fetches each release, checks the
archive's SHA-256, extracts the files and checks each against the manifest
(`--check` compares the checkout).

### New Computer Modern

Release 8.1.1 (2026-06-22), the author's archive
`https://download.gnu.org.ua/release/newcm/newcm-8.1.1.txz` (SHA-256
`1c10a9e2be6b0978a5d6e851094de72727237c1794ac200bc63604eb49adcba0`, with its
detached signature `newcm-8.1.1.txz.sig` beside it), the same release CTAN
distributes as `newcomputermodern` 8.1.1
(`https://mirrors.ctan.org/fonts/newcomputermodern.zip`, SHA-256
`9cc2e55835ab9adac227ccf1b8ed68bc0f73845489ea3533b88a1f56bd05d79a` when
fetched; CTAN serves only its current release, so a newer one fails the pin).
All files are CFF-outline OpenType from `otf/`:

| file | bytes | SHA-256 | scope | licence |
|---|---:|---|---|---|
| `NewCM10-Book.otf` | 694392 | `005f9b4e13b2a133df36d4ebfbb8f5fe402854bcb5882deb500b9c0f72b1bb49` | `product` | `LPPL-1.3c` |
| `NewCM10-BookItalic.otf` | 699696 | `d01f6c7b5bcdee2545823f1cf0a35698b9ccff1891a2f79a8e9938e5b8ea561e` | `product` | `LPPL-1.3c` |
| `NewCM10-Regular.otf` | 717588 | `328698d764ccdf7acf6bc1088aefd83a237f6a1d8b812de1e77ac5e4483bf3d1` | `os` | `GPL-3.0-or-later WITH Font-exception-2.0` |
| `NewCM10-Italic.otf` | 721176 | `a55d07607eec48889b6e7e7d51d7703c3beb915a39690f34fe01b4e2a744a9d6` | `product` | `LPPL-1.3c` |
| `NewCM10-Bold.otf` | 629288 | `40b0b32b63655fe802679ee4a14adb29c762a4712095d9c84cf7df7852b5152e` | `product` | `LPPL-1.3c` |
| `NewCM10-BoldItalic.otf` | 609000 | `fb53f43500c8ffbcc6dfdea5a519961d475543174ea2a0dcbe16c08510299fdf` | `product` | `LPPL-1.3c` |
| `NewCM08-Book.otf` | 676440 | `dd00447c2bf13d1923dccbc0ee90a0faf7e3a893442463255e170fd8599495fb` | `product` | `LPPL-1.3c` |

Licences, as the package's `License.txt` states them: every cut Tilo ships
is under the GUST Font License (LPPL 1.3c with a request to rename modified
files) except `NewCM10-Regular`, which is GPL-3.0-or-later with the Font
Exception and New Computer Modern's Distribution Exception (both in Section 7
of the GPL text in `License.txt`). The embedded name-ID 0 notices, appended to
`LICENSES/FONT-COPYRIGHT-NOTICES.txt`, say the same per file.

| file | bytes | SHA-256 | source |
|---|---:|---|---|
| `LICENSES/New-Computer-Modern-License.txt` | 33266 | `8641346ee391ff97eb66c70f3d5d695792cf9c5b3c5d748c6158d49cf79646e3` | https://mirrors.ctan.org/fonts/newcomputermodern.zip (newcomputermodern/License.txt, package 8.1.1) |
| `LICENSES/GUST-FONT-LICENSE.txt` | 1377 | `49ea6cb9257bbee0a3979c48a774cd221550ac1c20c95549efe45fc99cc18050` | https://tug.org/fonts/licenses/GUST-FONT-LICENSE.txt |
| `LICENSES/LPPL-1.3c.txt` | 19102 | `3d262cdf34dafa6955f703c634a8c238ec44109bc8dd6ef34fb7aa54809f7e66` | https://www.latex-project.org/lppl/lppl-1-3c.txt |

`LICENSES/New-Computer-Modern-License.txt` is the package's `License.txt`
byte for byte: the licence statement, the full GPL 3 text, the Font
Exception and the Distribution Exception.

**Where each file ships.** The manifest scopes `NewCM10-Regular.otf` `os`:
the Tilo OS image installs it (`tools/font-catalog.py stage --scope os`), and
the SDK's standalone catalog next to third-party Windows, macOS, Android and
web builds (`--scope product`) leaves it out, so that catalog carries only
OFL and GUST fonts. There, Book sets large sizes too. The Tilo OS is
AGPL-3.0, and the GPL'd cut ships in it as a separate font file under its own
licence, with the corresponding source below.

**Corresponding source.** The FontForge sources of every cut are
`newcomputermodern/sfd.txz` inside the CTAN package above (SHA-256
`73f140c8fa850b1dc460518227332c71bb69ed0a60ede072860d60d85be49b68`); the
Regular cut's is `sfd/NewCM10-Regular.sfd`. `tools/font-reading-faces.py`
checks that archive's hash with the rest. Tilo ships the author's OTF files
unchanged, so no Tilo modification needs its own source.

**Families.** Every file names itself `NewComputerModern` at weight 400,
whether it is the Book, Regular or 8 pt cut; the manifest's `tilo_families`
gives each the family the toolkit asks for (`New Computer Modern Book`,
`New Computer Modern 08`, `New Computer Modern` for Regular and Italic; the
shared Bold is in all three). The bytes are never changed. Fontconfig on the
OS sees the files' own names and tells the 8 pt and 10 pt cuts apart by
their declared design sizes.

**Rendering.** Tilo's text stack (cosmic-text shaping, swash and skrifa
outlines, vello) renders every cut's CFF outlines, the Regular cut's
included, so no TrueType conversion is shipped. Headless Chromium's font
sanitiser rejects the Regular cut's CFF CharStrings; that is a browser
limit, not a fault of the file, and the toolkit's own renderer is what Tilo
draws with in every host.

### Libertinus, Literata, EB Garamond, Source Serif 4

All SIL OFL 1.1, scope `product`:

| file | bytes | SHA-256 | scope | licence |
|---|---:|---|---|---|
| `LibertinusSerif-Regular.otf` | 337132 | `fcf06307a77367394fcb0ccb241e59eea70dba3d732be309647611224679c733` | `product` | `OFL-1.1` |
| `LibertinusSerif-Italic.otf` | 332312 | `9a393d63d6e05f620d3dc0190dfd35a8ede58c0808cf0fc9de7fcb9c723e4c24` | `product` | `OFL-1.1` |
| `LibertinusSerif-Bold.otf` | 293908 | `0264914210ed51b3231ebc92ce529e9f2e166ba9eebf0cd4a579558690a27b64` | `product` | `OFL-1.1` |
| `LibertinusSerif-BoldItalic.otf` | 259176 | `47a665259f09f554f5d133d7718cdad43ff462c6a6b2328f38023465e62d57ce` | `product` | `OFL-1.1` |
| `LibertinusSerifDisplay-Regular.otf` | 263688 | `7734b4d4cc5f3b98926f47a1389d775107496c813269cfed238dae5bd9329d44` | `product` | `OFL-1.1` |
| `Literata[opsz,wght].ttf` | 955132 | `b41138c9373112f32abb589cc22e8674b06ed4048b0c513be922bdd26f274440` | `product` | `OFL-1.1` |
| `Literata-Italic[opsz,wght].ttf` | 902728 | `d483dfaeba9cbf4ce71d32a52ee65df82f7e35b15fff8d1011cdb242d1fcd465` | `product` | `OFL-1.1` |
| `EBGaramond[wght].ttf` | 851176 | `ef9512f92f6d579e5dc75af59a5a4b1b8b47d2eda89e00b954d44520e5369027` | `product` | `OFL-1.1` |
| `EBGaramond-Italic[wght].ttf` | 754468 | `bba2c4499c93c9612b90b9825d32b07da52fce2fe57562a1eb6b833553f93c4e` | `product` | `OFL-1.1` |
| `SourceSerif4Variable-Roman.ttf` | 1204208 | `14d360ee1b76655da9276628b229e11671bc1f5d1083636144db6677d452cf55` | `product` | `OFL-1.1` |
| `SourceSerif4Variable-Italic.ttf` | 850036 | `6a059a64838978d54e8fab71ed86b0d82e948c0e12b2664d0c15166326dcff82` | `product` | `OFL-1.1` |

| source | release |
|---|---|
| `https://github.com/alerque/libertinus/releases/download/v7.051/Libertinus-7.051.zip` (`static/OTF/`) | Libertinus 7.051, archive SHA-256 `4d9be29b5cb380c35af8ba967abcc752ad1e07be1f738a9789c33e0dd7478c92` |
| `https://github.com/googlefonts/literata/releases/download/3.103/3.103.zip` (`fonts/variable/`) | Literata 3.103, archive SHA-256 `f7fb973cafb26cf785cbebaeaf51c18f87c15a3bcf4d82a7d4857564db5b056d` |
| `google/fonts` commit `9710da1eacb3be272583c3224dcb70f9da6eadbb`, `ofl/ebgaramond/` | EB Garamond as Google Fonts builds it from `octaviopardo/EBGaramond12` |
| `https://github.com/adobe-fonts/source-serif/releases/download/4.005R/source-serif-4.005_Desktop.zip` (`VAR/`) | Source Serif 4.005, archive SHA-256 `549fdb8f9a682bd06944298621404969f6de77c2e422ff3b8244a1dcd6a0c425` |

| file | bytes | SHA-256 | source |
|---|---:|---|---|
| `LICENSES/Libertinus-OFL.txt` | 4426 | `9aeecc8107c489ec1ec0068b0313e531a760edf3493705b32ab8ab8215a8794e` | https://github.com/alerque/libertinus/releases/download/v7.051/Libertinus-7.051.zip (Libertinus-7.051/OFL.txt) |
| `LICENSES/Literata-OFL.txt` | 4389 | `8742963604cd89dc81437811a850018fc03b2bfad686d7422c8235967c87614e` | https://github.com/googlefonts/literata/releases/download/3.103/3.103.zip (OFL.txt) |
| `LICENSES/EB-Garamond-OFL.txt` | 4398 | `0985066662eb755ed3683ae5482a81a9195b49ce3f7e165cc2388b3dbece7dd7` | https://github.com/google/fonts/blob/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/ebgaramond/OFL.txt |
| `LICENSES/Source-Serif-OFL.txt` | 4492 | `75784a295293a8992f5a8d99210566e0064a012e6dab6731305e3787f15896c7` | https://github.com/adobe-fonts/source-serif/releases/download/4.005R/source-serif-4.005_Desktop.zip (source-serif-4.005_Desktop/LICENSE.md) |

Source Serif carries the Reserved Font Name `Source`; the files ship
unmodified, so it does not apply. Libertinus, Literata and EB Garamond
declare none. Their name-ID 0, 13 and 14 records are appended to
`LICENSES/FONT-COPYRIGHT-NOTICES.txt`.

## Matched script fallback (Onest)

Tilo Sans, as shipped, has no Cyrillic. Set in Noto Sans, Cyrillic would sit
on a taller x-height with different shapes than the Latin around it, so the
toolkit sets it in a face chosen to match Tilo Sans' Latin instead, sized to
its x-height and weight-mapped to its stems (`docs/typography.md`, Script
fallbacks):

| file | bytes | SHA-256 | built from |
|---|---:|---|---|
| `Onest-Cyrillic[wght].ttf` | 80696 | `efc3cf7b09f5bb11880ece503a7df2f5d6c2bf3177f2bd88dca5e709aeb10895` | `ofl/onest/Onest[wght].ttf` (193056 bytes, `966c5c29b4755da84b6854d5c21dd4eaa2420225d0e9874de602de176d4a9f31`) |
| `LICENSES/Onest-OFL.txt` (upstream `OFL.txt`) | 4384 | `071195d8806e226faeee60259c28ca67b458227af5195a73f5cfcab06e3003bc` | |

The upstream file comes from `google/fonts` commit
`9710da1eacb3be272583c3224dcb70f9da6eadbb`. Its `METADATA.pb` names the
source: `https://github.com/simpals/onest` commit
`8739b1910618a15335e4cc48842052d0ee739ade` (release 2.001,
`fonts/variable/Onest[wght].ttf`).

`tools/font-fallback-subsets.py` fetches it, checks its SHA-256, and keeps
only Cyrillic and its supplements and extensions plus the space,
punctuation, digits, combining marks, general punctuation and currency signs
its runs need. It keeps the `wght` axis, all layout features, the name table
and the upstream `head` timestamp, so a rerun reproduces the bytes
(`--check` verifies the checkout).

License review of the modification: Onest is OFL 1.1 and its `OFL.txt` and
embedded copyright notice (`Copyright 2021 The Onest Project Authors`)
declare no Reserved Font Name, so a modified version may keep its name (OFL
condition 3 does not apply). The subset is distributed under the OFL with the
unmodified license text and copyright notice (conditions 2 and 5); its
name-ID 0, 13 and 14 records are appended to
`LICENSES/FONT-COPYRIGHT-NOTICES.txt`. It is not sold by itself (condition 1).

## Atkinson Hyperlegible Next (Hyperlegible UI face)

The built-in "Hyperlegible" UI face a theme or the accessibility layer can
choose, copied unmodified from `google/fonts` commit
`9710da1eacb3be272583c3224dcb70f9da6eadbb`
(`ofl/atkinsonhyperlegiblenext/`), whose `METADATA.pb` names the source
`https://github.com/googlefonts/atkinson-hyperlegible-next` commit
`7925f50f649b3813257faf2f4c0b381011f434f1`:

| file | bytes | SHA-256 |
|---|---:|---|
| `AtkinsonHyperlegibleNext[wght].ttf` | 114552 | `5a455d1cfa099b601ab70751bb9673e8fe1854dc4500c80e1a220d0d75e31745` |
| `AtkinsonHyperlegibleNext-Italic[wght].ttf` | 123916 | `ce9cffed32742ad2d9238c561a93220385e5934cdc02b8eb4097a50efa957dc6` |
| `LICENSES/Atkinson-Hyperlegible-Next-OFL.txt` (upstream `OFL.txt`) | 4431 | `aca6a428580965d2297d1b718042dd427c2a9443ece3b0d02d758e161e0c4030` |

Both embed `Copyright 2020-2024 The Atkinson Hyperlegible Next Project
Authors` and an OFL 1.1 notice with no Reserved Font Name; their name-ID 0, 13
and 14 records are appended to `LICENSES/FONT-COPYRIGHT-NOTICES.txt`. It
covers Latin only; Cyrillic takes the same matched fallback, and Greek and
Vietnamese take Noto Sans, each sized to its own x-height.

The current Fedora rendering does not reproduce all historical Tilo typography
PNGs. This catalog identifies a reviewed product input; it does not claim those
historical PNGs were generated with these bytes. Baseline migration requires a
separate evidence review. The following is the licensing audit; the upstream
paths it names identify acquisition inputs, not runtime dependencies.

# Canonical font catalog: license and provenance bundle

Date: 2026-09-12

This report completes the licensing input for the 16-file catalog in
`manifest.toml`. The shipped notices are in `LICENSES/`; the upstream sources
they were taken from are named below.

## Packaging decision

Every selected font binary is distributed under SIL Open Font License 1.1.
Ship the exact upstream family notice mapped below alongside each unmodified
font. Also ship a short generated copyright index containing the unique SFNT
name-ID 0 notices (`LICENSES/FONT-COPYRIGHT-NOTICES.txt`). This retains notices
which are more specific than, or newer than, the repository-level OFL header.

The minimal full-text bundle is 12 files and 52,680 bytes: eight distinct
notofonts family OFLs, the CJK license, both files which Fedora ships for the
emoji package, and the Adwaita license. The three Latin tag copies and the two
Symbols tag copies are byte-identical and can be deduplicated in the product
bundle; their upstream tags are still recorded separately below because their
provenance is different.

Use a manifest field such as `license_files = [...]` for every font entry. A
small exact product layout is:

```text
LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt
LICENSES/Noto-Sans-Arabic-OFL.txt
LICENSES/Noto-Sans-Hebrew-OFL.txt
LICENSES/Noto-Sans-Devanagari-OFL.txt
LICENSES/Noto-Sans-Bengali-OFL.txt
LICENSES/Noto-Sans-Thai-OFL.txt
LICENSES/Noto-Sans-Math-OFL.txt
LICENSES/Noto-Sans-Symbols-OFL.txt
LICENSES/Noto-Sans-CJK-OFL.txt
LICENSES/Noto-Color-Emoji-LICENSE.txt
LICENSES/Noto-Color-Emoji-OFL.txt
LICENSES/Adwaita-Sans-OFL.txt
LICENSES/FONT-COPYRIGHT-NOTICES.txt
```

The selected files retain their upstream filenames and bytes. The only
explicit reserved font name found is CJK's `Source`; retaining the upstream
file unchanged avoids a rename question. Any later subsetting or other font
modification needs a new license/name review before release.

## Per-font mapping

Paths in this table name the upstream release or package the notice came from.

| catalog font | authoritative full-text notice | additional exact embedded notice |
|---|---|---|
| `NotoSans[wght].ttf` | `notofonts-releases/NotoSans-v2.015/OFL.txt` | Noto Project Authors 2022, latin-greek-cyrillic |
| `NotoSans-Italic[wght].ttf` | `notofonts-releases/NotoSans-v2.015/OFL.txt` | same |
| `NotoSansMono[wght].ttf` | `notofonts-releases/NotoSansMono-v2.014/OFL.txt` | Noto Project Authors 2022, latin-greek-cyrillic |
| `NotoSansMath-Regular.ttf` | `notofonts-releases/NotoSansMath-v3.000/OFL.txt` | `Copyright 2022 Google LLC. All Rights Reserved.` |
| `NotoSansArabic[wght].ttf` | `notofonts-releases/NotoSansArabic-v2.013/OFL.txt` | Noto Project Authors 2022, arabic |
| `NotoSansHebrew[wght].ttf` | `notofonts-releases/NotoSansHebrew-v3.001/OFL.txt` | Noto Project Authors **2024**, hebrew |
| `NotoSansDevanagari[wght].ttf` | `notofonts-releases/NotoSansDevanagari-v2.006/OFL.txt` | Noto Project Authors 2022, devanagari |
| `NotoSansBengali[wght].ttf` | `notofonts-releases/NotoSansBengali-v3.000/OFL.txt` | Noto Project Authors **2025**, bengali |
| `NotoSansThai[wght].ttf` | `notofonts-releases/NotoSansThai-v2.002/OFL.txt` | Noto Project Authors 2022, thai |
| `NotoSansCJK-VF.ttc` | `noto-cjk-Sans2.004/LICENSE` | `© 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'.` on all five faces |
| `NotoSansSymbols[wght].ttf` | `notofonts-releases/NotoSansSymbols-v2.003/OFL.txt` | Noto Project Authors 2022, symbols |
| `NotoSansSymbols2-Regular.ttf` | `notofonts-releases/NotoSansSymbols2-v2.008/OFL.txt` | same |
| `Noto-COLRv1.ttf` | `noto-emoji-b3e3051/LICENSE` and `fedora-installed/google-noto-color-emoji-fonts/OFL.txt` | `Copyright 2022 Google Inc.`; the two package notices say Copyright 2013 Google LLC |
| `AdwaitaSans-Regular.ttf` | `adwaita-fonts-50.0/LICENSE` | embedded Inter 2016 notice; the full file also names Renzhi Li, Jamie Gravendeel, and Florian Müllner |

The name-ID 0, 13 and 14 notices were extracted from the Fedora 44 originals
with a dependency-free SFNT reader that did not modify a font; the unique
copyright notices are shipped as `LICENSES/FONT-COPYRIGHT-NOTICES.txt`.

## Exact notofonts release notices

Each tag was resolved through the official GitHub API.
Each OFL was fetched from the official repository at that tag, then
re-fetched by the resolved immutable commit SHA; the SHA-256 values matched.

| tag / repository | resolved commit | OFL bytes | OFL SHA-256 |
|---|---|---:|---|
| `NotoSans-v2.015`, `notofonts/latin-greek-cyrillic` | `c4a321e123e4d4ff315f57f4e0adf294fe3a95be` | 4396 | `cee9892f9f0cc8fe882c9e9537ee6a89621d86ee7ceaf70b02e2b2b1c25c061a` |
| `NotoSerif-v2.015`, same repository | `c4a321e123e4d4ff315f57f4e0adf294fe3a95be` | 4396 | `cee9892f9f0cc8fe882c9e9537ee6a89621d86ee7ceaf70b02e2b2b1c25c061a` |
| `NotoSansMono-v2.014`, same repository | `9b7310b8f99fcd2583c49606e6aefca13a391350` | 4396 | `cee9892f9f0cc8fe882c9e9537ee6a89621d86ee7ceaf70b02e2b2b1c25c061a` |
| `NotoSansArabic-v2.013`, `notofonts/arabic` | `1b2b7e5c6ce3ab4d50681c854892325530084c35` | 4382 | `a7a5a25eb188bf1cd96982030d53e23c33485c69b1044a562254226857ee13af` |
| `NotoSansHebrew-v3.001`, `notofonts/hebrew` | `036f3206f67caac235cf8546a7751d3440771a7e` | 4382 | `9b9fe028b5ba74d231659a1bbaf0ed09b11e759d1ca6a070999e16d151616b47` |
| `NotoSansDevanagari-v2.006`, `notofonts/devanagari` | `bb8d2566a1708ef2dcc6396ee2eb261a18967f76` | 4386 | `a216f6f8d85c7228093e0ee5e258d9d377e6671f68acb4db1930b29583d0f331` |
| `NotoSansBengali-v3.000`, `notofonts/bengali` | `7ab89a047313e4a2c7ca5d670b28ced031c86542` | 4383 | `754f0e221aa7d5a915489f3bf1f20fe53ddc35ab2834a4d91656d78f9622de70` |
| `NotoSansThai-v2.002`, `notofonts/thai` | `f8b482c158650260bba5d5edba9da3e8bb7185b4` | 4380 | `2e98fd23a52d253db8612cd5942c8f2ff4111b21d2367050fdca91d8ccc374a0` |
| `NotoSansMath-v3.000`, `notofonts/math` | `00e9941d95b2a355399a66f3990ffde6e4985676` | 4380 | `c9c63b8ed6cf76b2ce0c58b241acfc1195c2c928334c989c7047cbdac58773d9` |
| `NotoSansSymbols-v2.003`, `notofonts/symbols` | `b61fa7f86fb7d4163ee11d46d0977393f98d4f7a` | 4383 | `b118dd41337806a5d4797052c77caf3bd096aed783e5eb21b4d11154351e1ac0` |
| `NotoSansSymbols2-v2.008`, same repository | `25b00f0d3f40873a005287514ba2a48558655314` | 4383 | `b118dd41337806a5d4797052c77caf3bd096aed783e5eb21b4d11154351e1ac0` |

All of these OFLs contain the complete OFL 1.1 text and a repository-specific
Noto Project copyright header. The three Latin files are identical; the two
Symbols files are identical.

## CJK, emoji and Adwaita bundles

| source | upstream path | bytes | SHA-256 | byte proof |
|---|---|---:|---|---|
| Noto CJK `Sans2.004`, commit `523d033d6cb47f4a80c58a35753646f5c3608a78` | `licenses/noto-cjk-Sans2.004/LICENSE` | 4301 | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` | identical to Fedora `/usr/share/licenses/google-noto-sans-cjk-vf-fonts/LICENSE` |
| noto-emoji commit `b3e3051a088047d19fd4d49b1c3ac42fb8c3aaf8` | `licenses/noto-emoji-b3e3051/LICENSE` | 4330 | `500bb1ccf43df7bbb522112f9133a52b16e1c35e809632f5d8609b179152de5b` | identical to Fedora package `LICENSE` |
| Fedora emoji source bundle | `licenses/fedora-installed/google-noto-color-emoji-fonts/OFL.txt` | 4423 | `ab2dcc4de7edccb08a7e453bb88dd21bd3035fd5e8c9a2e977368b0ba3992281` | Fedora package file; Fedora spec maps it to `Source4: Noto_Emoji.zip` |
| GNOME Adwaita Fonts 50.0 source archive | `licenses/adwaita-fonts-50.0/LICENSE` | 4554 | `459687971d21c53923c1d1c9c062ec273a7ea03226b36195b79ec6af7d98dc81` | source-archive file is identical to Fedora package `LICENSE` |

The CJK, emoji `LICENSE`, and Adwaita files all
matched their upstream copies byte for byte. The emoji package's second
`OFL.txt` is also shipped because Fedora deliberately declares and installs
both `LICENSE OFL.txt`.

The monthly Noto 2025.12.01 release's `fonts/LICENSE` (4,374 bytes, SHA-256
`f2095b08bed08b23a6fe26112fcd679a2bee3f002eef077eb05d215ed1051bd8`).
It is a valid umbrella OFL with a generic Noto Project header, but the family
tag files above retain more precise repository provenance.

Do not package the
`google-noto-fonts-common` package's `LICENSE` as the license for
the 11 ordinary Noto font files. It is the 11,357-byte Apache-2.0 repository
license, SHA-256
`c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`.
Fedora's `google-noto-fonts` spec declares the font RPMs as OFL-1.1, while its
`%license */LICENSE` glob has installed this website/repository license under
the common package. The upstream family OFLs and embedded SFNT notices provide
the relevant font license evidence.

## Fedora source-package provenance

The `spec` and `sources` files below come from the Fedora 44 dist-git
branches, checked through the immutable dist-git commit URLs.

| Fedora dist-git | f44 commit | spec SHA-256 | sources SHA-256 | declared source digest |
|---|---|---|---|---|
| `google-noto-fonts` | `817ea8f29fb0d2ffb8987e50fb636e00cd702a1f` | `e8beaff1d09c53bbd8a733fb5ff633f80ffc538f5f2a99e3aa21bf07e35badd2` | `9bbddf94624b5e899d68131fc184d07a41f7a196d690718bb18907c37e952cbe` | monthly 2025.12.01 ZIP SHA-512 `b6a33adaae6f87ae5f41820deef63548f2c8e4dbb06182f23693acdf971910b4b870d35c28e025d83d8a166d8416683e016283590a4323e778b1f61deaf7046d` |
| `google-noto-sans-cjk-vf-fonts` | `3ed6b918569dbd0fd65e0c9543f97cc32b5b1f07` | `9d12ae52fea529521d6c5bb64323f7ed25d329fe6dc44345763a23b95e11310e` | `98ae771b4099af298f248bde2324d3aa9f848bf5ff3228e15db5f12c7da91d6f` | `01_NotoSansCJK-OTF-VF.zip` SHA-512 `e8756ec4897589f740adf59bc16554e61cec45e031fa7a41d2acfc6251a47e7db79f2489e1569388e51f12905e06b03c397faf3db366bf8c2549221fbd1980cc` |
| `google-noto-emoji-fonts` | `0651041e0ccc81cef309dffeced0683b34d75f33` | `0c9be0ce432b615fa5e8faee243a9af4551d9aee4f507e6ad974e08461934ebd` | `c99e16c5ec2ba7c6b8461e6a16ee22f0b215b2687624ed0d0def64a93ccf7a6b` | git archive SHA-512 `0e46a9836d581d10596009924c6b2ed43603a5ddd1c3bdc11c3d23e428ba2318074abfe71cf7f0b87973c612d52bfd86f4d9f54a2ca85a33c6a5cb3690895f8c`; `Noto_Emoji.zip` SHA-512 `23522e68cb0f103252aa3606aa3c3b31857093159edbed976de4d0d2425fc92918ca266c8afccffff0b1c8eff95754cf264648291eb9f3ea8b57f4e384ba5df2` |
| `adwaita-fonts` | `84ca97c76d4a4951be9911fd941478482c8023c3` | `40f75c3c3af979745cc67b4b57b0c1838e06ad5170b87bb002d672d2a27157eb` | `4147f0f6eb26966c23403c5847bda0635a229ef89979233277836c91e1b16149` | 50.0 tar.xz SHA-512 `546f03c89461c9cc998d54b0df003e6cb13daea2149ecb8e414e27934e7e29227e8886af276964cacef23dec48c5be32b59963abc61370bebba0d2bda96d178e` |

The first row covers the Sans, Mono, Math, five script and two Symbols
RPMs. The other rows cover the CJK, emoji and Adwaita files respectively.

## Binary hash correction and verification

All 16 SHA-256 values in `manifest.toml` were recomputed directly from the
Fedora 44 originals on 2026-09-12. Each is exactly 64 lowercase hexadecimal
characters.

The original Symbols row had accidentally omitted the `df8` after byte 36.
Its corrected, verified digest is:

```text
63724daeff6755a45933b9a5e3a3b41df5df8fe925c0f4c991f2f68c08a4182d
```

That is the only factual correction made to the catalog in this licensing
pass.

## Version uncertainty retained for implementation review

The installed bytes and RPM NEVRs are exact. The ordinary Noto source RPM is a
monthly aggregate release, while the most precise family copyright files live
in separate upstream repositories. The family versions in the font name
tables match the release tags used above, but the monthly aggregate metadata
does not expose a per-family source commit mapping. The report therefore keeps
both layers: immutable family-tag OFLs and the exact notices embedded in the
actual Fedora bytes.

Three embedded copyright lines differ from their repository OFL header:
Hebrew says 2024, Bengali says 2025, and Math names Google LLC rather than the
Noto Project Authors. CJK's OFL text has no copyright header, and emoji's font
name table says 2022 while its package notice says 2013. Preserve the generated
copyright index with the full-text licenses so these differences are not lost.
