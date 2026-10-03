# Tilo canonical fonts

The manifest records each file's source (an installed Fedora 44 binary path
and package NEVR, or an immutable upstream URL), complete SHA-256 values,
metadata, and exact license mappings. These files were copied without
modification. Product and fixture scope are independent of whether a font
happens to be installed on a host. The 17 product fonts total 44,214,180
bytes; Adwaita adds 879,796 fixture bytes.

## Instrument Sans (UI face)

Instrument Sans is the system UI face; Noto Sans stays in the catalog as the
fallback for the Greek, Cyrillic and Vietnamese text Instrument Sans does not
cover, and as the face of the PDF helper. Fedora 44 does not package
Instrument Sans, so its files come from the Google Fonts repository, which
publishes the upstream release unmodified:

| file | bytes | SHA-256 |
|---|---:|---|
| `InstrumentSans[wdth,wght].ttf` | 194336 | `b24f1812584816958afcf22e22d08e44318c5e51651e25d2438efdde389b33b1` |
| `InstrumentSans-Italic[wdth,wght].ttf` | 202128 | `a74203cc5066a3b2f8de1a7b0887ef897773c2319dc86c911f8e85350cde0d07` |
| `LICENSES/Instrument-Sans-OFL.txt` (upstream `OFL.txt`) | 4403 | `9e27a72ed30eb49a08678f6a5d6ed98ec7ba5368f541637ee0683ec9134ef966` |

All three were fetched from `google/fonts` commit
`2fc9491da70cf3e1bafd3c3da49244c5a496e9d5` (path `ofl/instrumentsans/`,
"Instrument Sans: Version 1.000", 2023-04-13), the last commit touching them.
That family's `METADATA.pb` names the upstream source: repository
`https://github.com/Instrument/instrument-sans`, commit
`7fa22308a3d0c94ee2b3cd537a1196b65db34a3e`, files
`fonts/variable/InstrumentSans[wdth,wght].ttf` and
`fonts/variable/InstrumentSans-Italic[wdth,wght].ttf`. Both fonts embed
`Copyright 2022 The Instrument Sans Project Authors` and an OFL 1.1 notice
with no Reserved Font Name; their name-ID 0, 13 and 14 records are appended
to `LICENSES/FONT-COPYRIGHT-NOTICES.txt`.

The face was chosen over Noto Sans, Mona Sans and Atkinson Hyperlegible Next
by rendering the same screen in each at the type roles' sizes on the vello
renderer: it is the most refined at 20-34 px, compact and clear at 12-14 px,
has tabular figures and a 400-700 weight axis covering every role weight,
carries no Reserved Font Name, and is 396 KB for both styles.

The current Fedora rendering does not reproduce all historical Tilo typography
PNGs. This catalog identifies a reviewed product input; it does not claim those
historical PNGs were generated with these bytes. Baseline migration requires a
separate evidence review. The following is the preserved licensing audit; its
scratch paths identify acquisition inputs, not runtime dependencies.

# Canonical font catalog: license and provenance bundle

Date: 2026-09-12

This report completes the licensing input for the 16-file catalog in
`font-catalog-sources.md`. It does not add product assets or implementation.
All preserved files are below the ignored scratch root:

```text
/Users/user/dev/tilo-macos-shell/.superpowers/sdd/2026-09-12-macos-shell-build-foundation/font-license-scratch
```

## Packaging decision

Every selected font binary is distributed under SIL Open Font License 1.1.
Ship the exact upstream family notice mapped below alongside each unmodified
font. Also ship a short generated copyright index containing the unique SFNT
name-ID 0 notices preserved in `font-name-notices.txt`. This retains notices
which are more specific than, or newer than, the repository-level OFL header.

The minimal full-text bundle is 12 files and 52,680 bytes: eight distinct
notofonts family OFLs, the CJK license, both files which Fedora ships for the
emoji package, and the Adwaita license. The three Latin tag copies and the two
Symbols tag copies are byte-identical and can be deduplicated in the product
bundle. Keep their separate tag copies in scratch because their provenance is
different.

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

Paths in this table are relative to `font-license-scratch/licenses/`.

| catalog font | authoritative full-text notice in scratch | additional exact embedded notice |
|---|---|---|
| `NotoSans[wght].ttf` | `notofonts-releases/NotoSans-v2.015/OFL.txt` | Noto Project Authors 2022, latin-greek-cyrillic |
| `NotoSans-Italic[wght].ttf` | `notofonts-releases/NotoSans-v2.015/OFL.txt` | same |
| `NotoSerif[wght].ttf` | `notofonts-releases/NotoSerif-v2.015/OFL.txt` | Noto Project Authors 2022, latin-greek-cyrillic |
| `NotoSerif-Italic[wght].ttf` | `notofonts-releases/NotoSerif-v2.015/OFL.txt` | same |
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

`font-name-notices.txt` is the exact extraction of name IDs 0, 13 and 14 from
the VM originals. It is 10,367 bytes with SHA-256
`f3cbba1d449827c0a7c7d47d9cabb9e87a81a03136fb0bb1bf20df3744548374`.
`extract_sfnt_names.py` is the dependency-free reader used in the VM through
the Parallels shared folder; it did not modify a font.

## Exact notofonts release notices

Each `commit.json` was fetched from the official GitHub API for the named tag.
Each preserved OFL was fetched from the official repository at that tag, then
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

| source | preserved path | bytes | SHA-256 | byte proof |
|---|---|---:|---|---|
| Noto CJK `Sans2.004`, commit `523d033d6cb47f4a80c58a35753646f5c3608a78` | `licenses/noto-cjk-Sans2.004/LICENSE` | 4301 | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` | identical to VM `/usr/share/licenses/google-noto-sans-cjk-vf-fonts/LICENSE` |
| noto-emoji commit `b3e3051a088047d19fd4d49b1c3ac42fb8c3aaf8` | `licenses/noto-emoji-b3e3051/LICENSE` | 4330 | `500bb1ccf43df7bbb522112f9133a52b16e1c35e809632f5d8609b179152de5b` | identical to VM package `LICENSE` |
| Fedora emoji source bundle | `licenses/fedora-installed/google-noto-color-emoji-fonts/OFL.txt` | 4423 | `ab2dcc4de7edccb08a7e453bb88dd21bd3035fd5e8c9a2e977368b0ba3992281` | copied from VM package; Fedora spec maps it to `Source4: Noto_Emoji.zip` |
| GNOME Adwaita Fonts 50.0 source archive | `licenses/adwaita-fonts-50.0/LICENSE` | 4554 | `459687971d21c53923c1d1c9c062ec273a7ea03226b36195b79ec6af7d98dc81` | source-archive file is identical to VM package `LICENSE` |

The Fedora-installed copies are retained separately under
`licenses/fedora-installed/`. The CJK, emoji `LICENSE`, and Adwaita files all
matched their upstream copies byte for byte. The emoji package's second
`OFL.txt` is also retained because Fedora deliberately declares and installs
both `LICENSE OFL.txt`.

The monthly Noto release's `fonts/LICENSE` is preserved at
`licenses/notofonts-monthly-2025.12.01/fonts-LICENSE` (4,374 bytes, SHA-256
`f2095b08bed08b23a6fe26112fcd679a2bee3f002eef077eb05d215ed1051bd8`).
It is a valid umbrella OFL with a generic Noto Project header, but the family
tag files above retain more precise repository provenance.

Do not package
`licenses/fedora-installed/google-noto-fonts-common/LICENSE` as the license for
the 11 ordinary Noto font files. It is the 11,357-byte Apache-2.0 repository
license, SHA-256
`c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`.
Fedora's `google-noto-fonts` spec declares the font RPMs as OFL-1.1, while its
`%license */LICENSE` glob has installed this website/repository license under
the common package. The upstream family OFLs and embedded SFNT notices provide
the relevant font license evidence.

## Fedora source-package provenance

The preserved `spec` and `sources` files came from the Fedora 44 dist-git
branches. They were checked again through the immutable dist-git commit URLs;
the local SHA-256 values matched.

| Fedora dist-git | f44 commit | spec SHA-256 | sources SHA-256 | declared source digest |
|---|---|---|---|---|
| `google-noto-fonts` | `817ea8f29fb0d2ffb8987e50fb636e00cd702a1f` | `e8beaff1d09c53bbd8a733fb5ff633f80ffc538f5f2a99e3aa21bf07e35badd2` | `9bbddf94624b5e899d68131fc184d07a41f7a196d690718bb18907c37e952cbe` | monthly 2025.12.01 ZIP SHA-512 `b6a33adaae6f87ae5f41820deef63548f2c8e4dbb06182f23693acdf971910b4b870d35c28e025d83d8a166d8416683e016283590a4323e778b1f61deaf7046d` |
| `google-noto-sans-cjk-vf-fonts` | `3ed6b918569dbd0fd65e0c9543f97cc32b5b1f07` | `9d12ae52fea529521d6c5bb64323f7ed25d329fe6dc44345763a23b95e11310e` | `98ae771b4099af298f248bde2324d3aa9f848bf5ff3228e15db5f12c7da91d6f` | `01_NotoSansCJK-OTF-VF.zip` SHA-512 `e8756ec4897589f740adf59bc16554e61cec45e031fa7a41d2acfc6251a47e7db79f2489e1569388e51f12905e06b03c397faf3db366bf8c2549221fbd1980cc` |
| `google-noto-emoji-fonts` | `0651041e0ccc81cef309dffeced0683b34d75f33` | `0c9be0ce432b615fa5e8faee243a9af4551d9aee4f507e6ad974e08461934ebd` | `c99e16c5ec2ba7c6b8461e6a16ee22f0b215b2687624ed0d0def64a93ccf7a6b` | git archive SHA-512 `0e46a9836d581d10596009924c6b2ed43603a5ddd1c3bdc11c3d23e428ba2318074abfe71cf7f0b87973c612d52bfd86f4d9f54a2ca85a33c6a5cb3690895f8c`; `Noto_Emoji.zip` SHA-512 `23522e68cb0f103252aa3606aa3c3b31857093159edbed976de4d0d2425fc92918ca266c8afccffff0b1c8eff95754cf264648291eb9f3ea8b57f4e384ba5df2` |
| `adwaita-fonts` | `84ca97c76d4a4951be9911fd941478482c8023c3` | `40f75c3c3af979745cc67b4b57b0c1838e06ad5170b87bb002d672d2a27157eb` | `4147f0f6eb26966c23403c5847bda0635a229ef89979233277836c91e1b16149` | 50.0 tar.xz SHA-512 `546f03c89461c9cc998d54b0df003e6cb13daea2149ecb8e414e27934e7e29227e8886af276964cacef23dec48c5be32b59963abc61370bebba0d2bda96d178e` |

The first row covers the Sans, Serif, Mono, Math, five script and two Symbols
RPMs. The other rows cover the CJK, emoji and Adwaita files respectively.

## Binary hash correction and verification

All 16 SHA-256 values in `font-catalog-sources.md` were recomputed directly
from the Fedora VM originals on 2026-09-12. Each is now exactly 64 lowercase
hexadecimal characters and matches the VM output.

The original Symbols row had accidentally omitted the `df8` after byte 36.
Its corrected, verified digest is:

```text
63724daeff6755a45933b9a5e3a3b41df5df8fe925c0f4c991f2f68c08a4182d
```

That is the only factual correction made to `font-catalog-sources.md` in this
licensing pass.

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
