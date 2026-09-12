# Canonical Font Resources and Explicit Loader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Supply one verified font catalog and explicit renderer construction for macOS and Linux.

**Architecture:** A manifest describes external licensed font files. A fallible loader validates their identity and metadata, provides shared fallback policy, and offers either product fonts with ambient named fonts or isolated fixtures. Existing default constructors retain their behavior until the renderer and installed-resource prerequisites are complete.

**Tech Stack:** Rust, cosmic-text 0.19.0, fontdb 0.23.0, unicode-script 0.5.8, skrifa 0.40.0, serde/TOML, SHA-256, Python 3.11+.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, sections 9 and 11. This independently testable prerequisite implements the font input boundary, not the complete Mac host or final font activation.

## Global Constraints

- “Run both hosts from the same git commit and lockfile, with separate native build directories.”
- “Keep Mac rendering expectations explicit about Metal, fonts, scale and pixel tolerance instead of weakening Linux goldens to make them pass on another backend.”
- “Use the repository's nextest process isolation and GPU scheduling rules. Keep the isolated visual-golden tail so those tests are executed after other GPU work drains.”
- “Preserve Linux CLI defaults and commands.”
- Worktree `/Users/user/dev/qos-macos-shell`, branch `codex/macos-shell`. Mac target `/Users/user/dev/qos/target/macos-shell`; controller manages Fedora clone `/home/parallels/qos-macos-shell`, target `/home/parallels/qos-target`.
- Do not embed font binaries, subset fonts, install user fonts, change process-global catalog overrides, relax metric assertions, or regenerate existing golden PNGs.
- Product payload: 15 files, 43,817,716 bytes. Controlled payload adds Adwaita: 16 files, 44,697,512 bytes. License and manifest bytes are additional.
- Existing default constructors and runtime packaging remain unchanged in this prerequisite. Explicit catalog construction must be functional, not a stub.
- Validate the full task-specific Python or Rust test selection and library clippy. Do not repeat unrelated full UI/GPU suites for resource-only changes; the broader branch gates remain required at integration.

## Design decisions and boundaries

Product catalogs reserve only families from the 15 product assets, keeping other installed named fonts available. Adwaita is fixture-only and therefore does not reserve its name in product mode. Controlled catalogs load all 16 files without system discovery. Generic sans, serif and mono names are set after discovery and duplicate removal.

Fontdb prefers name ID 16 over ID 1. `Noto Sans Hebrew Light` is a reserved source alias; the loader requires the actual canonical family `Noto Sans Hebrew`, expected PostScript name and collection index, rather than demanding that both SFNT name IDs appear as separate families.

The root is passed explicitly. Metadata alone is embedded through `include_str!`; fonts remain external. Every load validates file size, SHA-256 and face metadata before host discovery. Missing or changed files return a typed path/error and cannot fall back to ambient fonts.

The current Fedora font set is not proven to reproduce historical typography PNGs: all six unmodified fixtures fail at `d19f1a0ae`. Preserve those PNGs. A later focused renderer plan must correct requested versus painted variable weight, rich-run batching by weight, collection index selection, and backing-data sharing. Subsequent default activation must also provide strict executable-context root resolution, startup/warmup error handling, all Linux package paths, qpkg sandbox visibility and the actual Mac bundle. Those are explicit remaining goal requirements; completion of this plan does not claim their completion.

## Task interfaces and file ownership

| Task | Produces | Consumed by |
| --- | --- | --- |
| 1 | Authoritative manifest, exact 16 binaries, 13 license/notice files, provenance, verifier and stager | Task 2 reads the same manifest and external assets. Future package construction uses the stager. |
| 2 | `FontCatalog`, fallible loader, shared fallback, explicit `TextRenderer::with_font_catalog` | Focused renderer regressions, controlled metric fixtures, then default runtime integration. |

The ignored acquisition reports named in Task 1 are controller-supplied input artifacts. The committed product records preserve their content and font identities; no installed runtime depends on those scratch paths.

---

### Task 1: Land exact assets, authoritative metadata, and a reusable verifier/stager

**Files:**
- Create: `ui/assets/fonts/manifest.toml`, the 16 files in its table, `ui/assets/fonts/LICENSES/` and `ui/assets/fonts/PROVENANCE.md`.
- Create: `tools/font-catalog.py`, `tools/tests/test_font_catalog.py`.

**Interfaces:**
- Produces: immutable manifest schema 1, catalog ID `qos-fonts-fedora44-2026-09-12-v1`.
- Produces: `python3 tools/font-catalog.py verify ROOT --scope product|fixtures` and `python3 tools/font-catalog.py stage SOURCE DEST --scope product|fixtures`.
- The stager writes the **same** full manifest in either scope; absence of fixture-only fonts is valid for a product directory. It includes all full-text license files and the copyright index in both scopes for uncomplicated redistribution.

- [ ] Write the manifest exactly as below. `faces` is indexed by TTC face index; every noncollection has one entry. `reserved_aliases` includes source-family aliases even when fontdb selects typographic names. The runtime parses this very manifest, not an independently maintained Rust list. Preserve the original binary filenames.

```toml
schema = 1
catalog_id = "qos-fonts-fedora44-2026-09-12-v1"
product_bytes = 43817716
fixture_bytes = 44697512

[[font]]
scope = "product"
file = "NotoSans[wght].ttf"
bytes = 712460
sha256 = "fb53504de62e767a54695416123d68d67f71c9b69722175184ad56e7dd13547b"
source_path = "/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf"
rpm = "google-noto-sans-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans; Regular; PS `NotoSans-Regular`; v2.015; `wght` 400/400/700"
reserved_aliases = ["Noto Sans"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans"
post_script_name = "NotoSans-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSans-Italic[wght].ttf"
bytes = 739728
sha256 = "28602f4b032ed8f5d91445e830585f34aba54ff9d3ac3e58c0b8834f3d6ae684"
source_path = "/usr/share/fonts/google-noto-vf/NotoSans-Italic[wght].ttf"
rpm = "google-noto-sans-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans; Italic; PS `NotoSans-Italic`; v2.015; `wght` 400/400/700"
reserved_aliases = ["Noto Sans"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans"
post_script_name = "NotoSans-Italic"
style = "italic"

[[font]]
scope = "product"
file = "NotoSerif[wght].ttf"
bytes = 811080
sha256 = "9374c4d81d2e21b03ac03fbc165458e8cf63e434a44c82193aa0305d20dc1037"
source_path = "/usr/share/fonts/google-noto-vf/NotoSerif[wght].ttf"
rpm = "google-noto-serif-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Serif; Regular; PS `NotoSerif`; v2.015; `wght` 400/400/700"
reserved_aliases = ["Noto Serif"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Serif"
post_script_name = "NotoSerif"
style = "normal"

[[font]]
scope = "product"
file = "NotoSerif-Italic[wght].ttf"
bytes = 849200
sha256 = "30f1c0204b6bb16d7b6f0984ce6e355ad61d05656201b3d13a2b19bb9baa6779"
source_path = "/usr/share/fonts/google-noto-vf/NotoSerif-Italic[wght].ttf"
rpm = "google-noto-serif-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Serif; Italic; PS `NotoSerif-Italic`; v2.015; `wght` 400/400/700"
reserved_aliases = ["Noto Serif"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Serif"
post_script_name = "NotoSerif-Italic"
style = "italic"

[[font]]
scope = "product"
file = "NotoSansMono[wght].ttf"
bytes = 569204
sha256 = "3c874b97ce11dc54de004c81df02c8f0974033ed1b113377130cbdd71d478f11"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansMono[wght].ttf"
rpm = "google-noto-sans-mono-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Mono; Regular; PS `NotoSansMono-Regular`; v2.014; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Mono"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Mono"
post_script_name = "NotoSansMono-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansMath-Regular.ttf"
bytes = 990564
sha256 = "d51afd5739c7ba6c44fcab35a88160e25dfb69a2d4ad0bd99533f8d894af1f96"
source_path = "/usr/share/fonts/google-noto/NotoSansMath-Regular.ttf"
rpm = "google-noto-sans-math-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Math; Regular; PS `NotoSansMath-Regular`; v3.000; static"
reserved_aliases = ["Noto Sans Math"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Math-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Math"
post_script_name = "NotoSansMath-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansArabic[wght].ttf"
bytes = 239324
sha256 = "3fae4d7239d1faa55b234bda8091019a5251ff79df40d5e29367d84bed2a8138"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansArabic[wght].ttf"
rpm = "google-noto-sans-arabic-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Arabic; Regular; PS `NotoSansArabic-Regular`; v2.013; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Arabic"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Arabic-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Arabic"
post_script_name = "NotoSansArabic-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansHebrew[wght].ttf"
bytes = 25296
sha256 = "9d81d81ad592644487b03843da73a87b99cd28a2c4da4a096e1db8fbd14fda61"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansHebrew[wght].ttf"
rpm = "google-noto-sans-hebrew-vf-fonts-0:20251201-2.fc44"
source_metadata = "aliases Noto Sans Hebrew / Noto Sans Hebrew Light; default HB name Light; PS `NotoSansHebrew-Light`; v3.001; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Hebrew", "Noto Sans Hebrew Light"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Hebrew-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Hebrew"
post_script_name = "NotoSansHebrew-Light"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansDevanagari[wght].ttf"
bytes = 276816
sha256 = "ffb0d05ef3d8e6d0c3d7c9380099127b003854242c11d0751b6a2e85e1f8f1b5"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansDevanagari[wght].ttf"
rpm = "google-noto-sans-devanagari-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Devanagari; Regular; PS `NotoSansDevanagari-Regular`; v2.006; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Devanagari"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Devanagari-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Devanagari"
post_script_name = "NotoSansDevanagari-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansBengali[wght].ttf"
bytes = 165368
sha256 = "71647364a0b9851a07db3207431256279bb8db9e8a4ba9d487ff2fe7431f5fcd"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansBengali[wght].ttf"
rpm = "google-noto-sans-bengali-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Bengali; Regular; PS `NotoSansBengali-Regular`; v3.000; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Bengali"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Bengali-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Bengali"
post_script_name = "NotoSansBengali-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansThai[wght].ttf"
bytes = 31912
sha256 = "13f52f79b04f01f2751a81052a5dee2df639fd4e13f4ac30821ab45c8dd5f7e7"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansThai[wght].ttf"
rpm = "google-noto-sans-thai-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Thai; Regular; PS `NotoSansThai-Regular`; v2.002; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Thai"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Thai-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Thai"
post_script_name = "NotoSansThai-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansCJK-VF.ttc"
bytes = 32682580
sha256 = "d3d8256cdec8dbcb3552284bc6b20c734dd60c2ee9df83b5758e34807c4bac32"
source_path = "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc"
rpm = "google-noto-sans-cjk-vf-fonts-1:2.004-11.fc44"
source_metadata = "collection indices 0..4: Noto Sans CJK JP, KR, SC, TC, HK; v2.004; `wght` 100/100/900"
reserved_aliases = ["Noto Sans CJK JP", "Noto Sans CJK KR", "Noto Sans CJK SC", "Noto Sans CJK TC", "Noto Sans CJK HK"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-CJK-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans CJK JP"
post_script_name = "NotoSansCJKjp-Thin"
style = "normal"
[[font.faces]]
index = 1
family = "Noto Sans CJK KR"
post_script_name = "NotoSansCJKkr-Thin"
style = "normal"
[[font.faces]]
index = 2
family = "Noto Sans CJK SC"
post_script_name = "NotoSansCJKsc-Thin"
style = "normal"
[[font.faces]]
index = 3
family = "Noto Sans CJK TC"
post_script_name = "NotoSansCJKtc-Thin"
style = "normal"
[[font.faces]]
index = 4
family = "Noto Sans CJK HK"
post_script_name = "NotoSansCJKhk-Thin"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansSymbols[wght].ttf"
bytes = 239720
sha256 = "63724daeff6755a45933b9a5e3a3b41df5df8fe925c0f4c991f2f68c08a4182d"
source_path = "/usr/share/fonts/google-noto-vf/NotoSansSymbols[wght].ttf"
rpm = "google-noto-sans-symbols-vf-fonts-0:20251201-2.fc44"
source_metadata = "Noto Sans Symbols; Regular; PS `NotoSansSymbols-Regular`; v2.003; `wght` 400/400/700"
reserved_aliases = ["Noto Sans Symbols"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Symbols-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Symbols"
post_script_name = "NotoSansSymbols-Regular"
style = "normal"

[[font]]
scope = "product"
file = "NotoSansSymbols2-Regular.ttf"
bytes = 671568
sha256 = "c4a0a80f0041ce4be81e2478faad22776d23edb98ae3f0d19bd37044820ecf9d"
source_path = "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf"
rpm = "google-noto-sans-symbols-2-fonts-0:20251201-2.fc44"
source_metadata = "**Noto Sans Symbols 2**; Regular; PS `NotoSansSymbols2-Regular`; v2.008; static"
reserved_aliases = ["Noto Sans Symbols 2"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Sans-Symbols-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Sans Symbols 2"
post_script_name = "NotoSansSymbols2-Regular"
style = "normal"

[[font]]
scope = "product"
file = "Noto-COLRv1.ttf"
bytes = 4812896
sha256 = "94c6c870925aaafd3fdda338fc4b2e4dfb74260c9aefa87d4aa8a145a993a5a2"
source_path = "/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf"
rpm = "google-noto-color-emoji-fonts-0:20250623-4.fc44"
source_metadata = "Noto Color Emoji; Regular; PS `NotoColorEmoji`; v2.048 (`noto-emoji:20250612:c7a259fc...`); static COLRv1"
reserved_aliases = ["Noto Color Emoji"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Noto-Color-Emoji-LICENSE.txt", "LICENSES/Noto-Color-Emoji-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Noto Color Emoji"
post_script_name = "NotoColorEmoji"
style = "normal"

[[font]]
scope = "fixture"
file = "AdwaitaSans-Regular.ttf"
bytes = 879796
sha256 = "8381c33b9a44f066f2b99dba3d416a2342891e28c956a35dfd8d16ee2987e6d4"
source_path = "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf"
rpm = "adwaita-sans-fonts-0:50.0-1.fc44"
source_metadata = "Adwaita Sans; Regular; PS `AdwaitaSans`; v4.001 git `9221beed3`; axes in native order `opsz` 14/14/32, `wght` 100/400/900"
reserved_aliases = ["Adwaita Sans"]
license_spdx = "OFL-1.1"
license_files = ["LICENSES/Adwaita-Sans-OFL.txt", "LICENSES/FONT-COPYRIGHT-NOTICES.txt"]
[[font.faces]]
index = 0
family = "Adwaita Sans"
post_script_name = "AdwaitaSans"
style = "normal"

[[license]]
file = "LICENSES/Noto-Latin-Greek-Cyrillic-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSans-v2.015/OFL.txt"
sha256 = "cee9892f9f0cc8fe882c9e9537ee6a89621d86ee7ceaf70b02e2b2b1c25c061a"

[[license]]
file = "LICENSES/Noto-Sans-Arabic-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansArabic-v2.013/OFL.txt"
sha256 = "a7a5a25eb188bf1cd96982030d53e23c33485c69b1044a562254226857ee13af"

[[license]]
file = "LICENSES/Noto-Sans-Hebrew-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansHebrew-v3.001/OFL.txt"
sha256 = "9b9fe028b5ba74d231659a1bbaf0ed09b11e759d1ca6a070999e16d151616b47"

[[license]]
file = "LICENSES/Noto-Sans-Devanagari-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansDevanagari-v2.006/OFL.txt"
sha256 = "a216f6f8d85c7228093e0ee5e258d9d377e6671f68acb4db1930b29583d0f331"

[[license]]
file = "LICENSES/Noto-Sans-Bengali-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansBengali-v3.000/OFL.txt"
sha256 = "754f0e221aa7d5a915489f3bf1f20fe53ddc35ab2834a4d91656d78f9622de70"

[[license]]
file = "LICENSES/Noto-Sans-Thai-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansThai-v2.002/OFL.txt"
sha256 = "2e98fd23a52d253db8612cd5942c8f2ff4111b21d2367050fdca91d8ccc374a0"

[[license]]
file = "LICENSES/Noto-Sans-Math-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansMath-v3.000/OFL.txt"
sha256 = "c9c63b8ed6cf76b2ce0c58b241acfc1195c2c928334c989c7047cbdac58773d9"

[[license]]
file = "LICENSES/Noto-Sans-Symbols-OFL.txt"
source_in_license_scratch = "licenses/notofonts-releases/NotoSansSymbols-v2.003/OFL.txt"
sha256 = "b118dd41337806a5d4797052c77caf3bd096aed783e5eb21b4d11154351e1ac0"

[[license]]
file = "LICENSES/Noto-Sans-CJK-OFL.txt"
source_in_license_scratch = "licenses/noto-cjk-Sans2.004/LICENSE"
sha256 = "6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2"

[[license]]
file = "LICENSES/Noto-Color-Emoji-LICENSE.txt"
source_in_license_scratch = "licenses/noto-emoji-b3e3051/LICENSE"
sha256 = "500bb1ccf43df7bbb522112f9133a52b16e1c35e809632f5d8609b179152de5b"

[[license]]
file = "LICENSES/Noto-Color-Emoji-OFL.txt"
source_in_license_scratch = "licenses/fedora-installed/google-noto-color-emoji-fonts/OFL.txt"
sha256 = "ab2dcc4de7edccb08a7e453bb88dd21bd3035fd5e8c9a2e977368b0ba3992281"

[[license]]
file = "LICENSES/Adwaita-Sans-OFL.txt"
source_in_license_scratch = "licenses/adwaita-fonts-50.0/LICENSE"
sha256 = "459687971d21c53923c1d1c9c062ec273a7ea03226b36195b79ec6af7d98dc81"

[[license]]
file = "LICENSES/FONT-COPYRIGHT-NOTICES.txt"
source_in_license_scratch = "licenses/../font-name-notices.txt"
sha256 = "f3cbba1d449827c0a7c7d47d9cabb9e87a81a03136fb0bb1bf20df3744548374"
```

- [ ] Acquire the exact binaries into `ui/assets/fonts/` from the controller-approved Fedora source. The following is the copy program to run **on Fedora after the manifest exists in the shared source worktree**; its destination is explicit. No host font installation is involved.

```python
from pathlib import Path
import shutil, tomllib
dst = Path("/media/psf/Home/dev/qos-macos-shell/ui/assets/fonts")
manifest = tomllib.loads((dst / "manifest.toml").read_text())
for asset in manifest["font"]:
    shutil.copyfile(asset["source_path"], dst / asset["file"])
```

- [ ] Copy license files with the program below on Mac; preserve source bytes. The original full extracted SFNT notices become the copyright index, so no notice is lost in a paraphrase.

```python
from pathlib import Path
import shutil, tomllib
dst = Path("ui/assets/fonts")
scratch = Path(".superpowers/sdd/2026-09-12-macos-shell-build-foundation/font-license-scratch")
manifest = tomllib.loads((dst / "manifest.toml").read_text())
for asset in manifest["license"]:
    target = dst / asset["file"]
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(scratch / asset["source_in_license_scratch"], target)
```

- [ ] Create `PROVENANCE.md` by preserving `font-catalog-licenses.md` verbatim after this preamble (the report contains the exact immutable upstream/tag and Fedora dist-git commits):

```markdown
# qOS canonical fonts

The manifest records installed Fedora 44 binary paths, package NEVRs, complete
SHA-256 values, metadata, and exact license mappings. These files were copied
without modification. Product and fixture scope are independent of whether a
font happens to be installed on a host. The 15 product fonts total 43,817,716
bytes; Adwaita adds 879,796 fixture bytes.

The current Fedora rendering does not reproduce all historical qOS typography
PNGs. This catalog identifies a reviewed product input; it does not claim those
historical PNGs were generated with these bytes. Baseline migration requires a
separate evidence review. The following is the preserved licensing audit; its
scratch paths identify acquisition inputs, not runtime dependencies.
```

- [ ] Write the verifier/stager tests first, in `tools/tests/test_font_catalog.py`:

```python
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)

class CatalogTest(unittest.TestCase):
    def test_reviewed_payload_and_product_staging(self):
        source = ROOT / "ui/assets/fonts"
        self.assertEqual(fc.verify(source, "fixtures"), (16, 44697512))
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "fonts"
            fc.stage(source, dest, "product")
            self.assertEqual(fc.verify(dest, "product"), (15, 43817716))
            self.assertFalse((dest / "AdwaitaSans-Regular.ttf").exists())
            self.assertTrue((dest / "LICENSES/Noto-Color-Emoji-OFL.txt").is_file())
            with self.assertRaises(ValueError):
                fc.verify(dest, "fixtures")

    def test_corruption_is_not_substituted(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "fonts"
            fc.stage(ROOT / "ui/assets/fonts", dest, "product")
            target = dest / "NotoSansMath-Regular.ttf"
            data = bytearray(target.read_bytes())
            data[-1] ^= 1
            target.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "SHA-256.*NotoSansMath"):
                fc.verify(dest, "product")
            target.unlink()
            with self.assertRaisesRegex(ValueError, "missing.*NotoSansMath"):
                fc.verify(dest, "product")

if __name__ == "__main__":
    unittest.main()
```

- [ ] Run `python3 -m unittest discover -s tools/tests -p test_font_catalog.py`; expect missing verifier failure initially.
- [ ] Implement `tools/font-catalog.py` exactly:

```python
#!/usr/bin/env python3
"""Verify and stage qOS fonts. Requires Python 3.11+; no third-party modules."""
import argparse
import hashlib
from pathlib import Path
import shutil
import tomllib

AUTHORITATIVE = Path(__file__).resolve().parents[1] / "ui/assets/fonts/manifest.toml"

def manifest(root):
    path = root / "manifest.toml"
    if not path.is_file():
        raise ValueError(f"missing manifest: {path}")
    raw = path.read_bytes()
    if raw != AUTHORITATIVE.read_bytes():
        raise ValueError(f"manifest differs from this qOS build: {path}")
    result = tomllib.loads(raw.decode())
    if result["schema"] != 1:
        raise ValueError(f"unsupported schema: {path}")
    return result

def selected(data, scope):
    return [f for f in data["font"] if scope == "fixtures" or f["scope"] == "product"]

def checked_path(root, name):
    relative = Path(name)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"invalid manifest path: {name}")
    path = root / relative
    if not path.is_file():
        raise ValueError(f"missing regular file: {path}")
    return path

def verify(root, scope):
    root = Path(root)
    data = manifest(root)
    fonts = selected(data, scope)
    for entry in fonts + data["license"]:
        path = checked_path(root, entry["file"])
        if "bytes" in entry and path.stat().st_size != entry["bytes"]:
            raise ValueError(f"size mismatch: {path}")
        with path.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != entry["sha256"]:
            raise ValueError(f"SHA-256 mismatch: {path}")
    count, total = len(fonts), sum(f["bytes"] for f in fonts)
    expected = (16, data["fixture_bytes"]) if scope == "fixtures" else (15, data["product_bytes"])
    if (count, total) != expected:
        raise ValueError(f"invalid catalog payload: {(count, total)} != {expected}")
    licensed = {entry["file"] for entry in data["license"]}
    for entry in fonts:
        if entry["license_spdx"] != "OFL-1.1" or not set(entry["license_files"]) <= licensed:
            raise ValueError(f"invalid license mapping: {entry['file']}")
    return count, total

def stage(source, dest, scope):
    source, dest = Path(source), Path(dest)
    verify(source, scope)
    data = manifest(source)
    dest.mkdir(parents=True, exist_ok=False)
    files = [f["file"] for f in selected(data, scope) + data["license"]]
    files += ["manifest.toml", "PROVENANCE.md"]
    for name in files:
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(checked_path(source, name), target)
    verify(dest, scope)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("verify")
    check.add_argument("root", type=Path)
    check.add_argument("--scope", choices=["product", "fixtures"], required=True)
    copy = commands.add_parser("stage")
    copy.add_argument("source", type=Path)
    copy.add_argument("dest", type=Path)
    copy.add_argument("--scope", choices=["product", "fixtures"], required=True)
    args = parser.parse_args()
    try:
        if args.command == "verify":
            print(verify(args.root, args.scope))
        else:
            stage(args.source, args.dest, args.scope)
    except (ValueError, OSError) as error:
        parser.exit(1, f"qOS font catalog: {error}\n")

if __name__ == "__main__":
    main()
```

- [ ] Run the same unittest command and both source verification scopes. Require exactly `(15, 43817716)` and `(16, 44697512)`. Inspect `git diff --stat` for exactly 16 binary files and no historical PNG changes. Commit resource/verifier changes as `feat(ui): pin canonical font resources and license provenance`.

### Task 2: Add the fallible shared loader and explicit renderer construction

**Files:**
- Create: `ui/src/font_catalog.rs`, `ui/src/font_catalog/fallback.rs`, `ui/src/font_catalog/tests.rs`.
- Modify: `ui/Cargo.toml`, `Cargo.lock`, `ui/src/lib.rs`, `ui/src/text_render.rs`.

**Interfaces:**
- `FontCatalog::product(root: impl Into<PathBuf>, locale: impl Into<String>) -> FontCatalog` selects product scope and host discovery.
- `FontCatalog::controlled_fixture(root: impl Into<PathBuf>, locale: impl Into<String>) -> FontCatalog` selects all fixtures and excludes host discovery.
- `FontCatalog::load(&self) -> Result<FontSystem, FontCatalogError>` is authoritative.
- `configured_font_system_with_catalog(&FontCatalog) -> Result<FontSystem, FontCatalogError>` delegates to `load`.
- `TextRenderer::with_font_catalog(&FontCatalog) -> Result<Self, FontCatalogError>` shares existing cache initialization. At this stage no-arg construction stays unchanged until the subsequent default-integration plan.
- `FontCatalog::root(&self) -> &Path`, `FontCatalog::locale(&self) -> &str`, `catalog_manifest_sha256() -> String` support provenance receipts without mutable state.
- `FontCatalogError` is a typed path plus reason and implements Display/Error. Exact absence/corruption information survives constructor/startup propagation.

- [ ] Add `toml.workspace = true` and `unicode-script = "=0.5.8"` to `[dependencies]` in `ui/Cargo.toml`; both versions are already in the lockfile. Use existing serde, sha2, hex, thiserror, skrifa. Update only `ui`'s dependency list in Cargo.lock through Cargo's normal lock resolution; do not upgrade cosmic-text/fontdb or unrelated dependencies. Add `pub mod font_catalog;` to `ui/src/lib.rs`.
- [ ] Write the tests below in `ui/src/font_catalog/tests.rs` before implementing the loader:

```rust
use super::*;
use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};
use cosmic_text::fontdb::{Query, Style, Weight};

fn fixture() -> FontCatalog {
    FontCatalog::controlled_fixture(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"), "en-US")
}

#[test]
fn controlled_catalog_has_only_the_twenty_manifest_faces() {
    let catalog = fixture();
    let fs = catalog.load().unwrap();
    assert_eq!(fs.db().len(), 20); // 15 single faces plus five TTC faces.
    let root = catalog.root().canonicalize().unwrap();
    for face in fs.db().faces() {
        match &face.source {
            fontdb::Source::File(path) => assert!(path.starts_with(&root)),
            _ => panic!("canonical file must stay file-backed"),
        }
    }
    for (generic, expected) in [
        (Family::SansSerif, "Noto Sans"),
        (Family::Serif, "Noto Serif"),
        (Family::Monospace, "Noto Sans Mono"),
    ] {
        for weight in [Weight::NORMAL, Weight(600), Weight::BOLD] {
            let id = fs.db().query(&Query { families: &[generic], weight, ..Query::default() }).unwrap();
            assert!(fs.db().face(id).unwrap().families.iter().any(|f| f.0 == expected));
        }
    }
    for expected in ["Noto Sans", "Noto Serif"] {
        let id = fs.db().query(&Query { families: &[Family::Name(expected)], style: Style::Italic, ..Query::default() }).unwrap();
        assert_eq!(fs.db().face(id).unwrap().style, Style::Italic);
    }
    for expected in ["Noto Sans Math", "Adwaita Sans", "Noto Sans Symbols 2"] {
        assert!(fs.db().query(&Query { families: &[Family::Name(expected)], ..Query::default() }).is_some());
    }
}

#[test]
fn named_host_fonts_survive_but_reserved_claimants_do_not() {
    let catalog = fixture();
    let mut called = false;
    let mut named_id = None;
    let mut duplicate_id = None;
    let mut alias_id = None;
    let mut ambient_adwaita_id = None;
    let product = FontCatalog::product(catalog.root(), "en-US");
    let fs = product.load_with_system(|db| {
        called = true;
        let face = db.faces().next().unwrap().clone();
        let mut duplicate = face.clone();
        duplicate.families = vec![("Noto Sans".into(), face.families[0].1)];
        duplicate_id = Some(db.push_face_info(duplicate));
        let mut alias = face.clone();
        alias.families = vec![("Noto Sans Hebrew Light".into(), face.families[0].1)];
        alias_id = Some(db.push_face_info(alias));
        let mut ambient = face.clone();
        ambient.families = vec![("Adwaita Sans".into(), face.families[0].1)];
        ambient_adwaita_id = Some(db.push_face_info(ambient));
        let mut other = face;
        other.families = vec![("Local Named Test".into(), other.families[0].1)];
        named_id = Some(db.push_face_info(other));
    }).unwrap();
    assert!(called);
    assert!(fs.db().face(duplicate_id.unwrap()).is_none());
    assert!(fs.db().face(alias_id.unwrap()).is_none());
    assert!(fs.db().face(named_id.unwrap()).is_some());
    assert_eq!(fs.db().query(&Query { families: &[Family::Name("Adwaita Sans")], ..Query::default() }), ambient_adwaita_id);
    catalog.load_with_system(|_| panic!("controlled catalog must never discover ambient fonts")).unwrap();
}

#[test]
fn missing_or_corrupt_resources_fail_before_system_discovery() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let catalog = FontCatalog::product(root, "en-US");
    let error = catalog.load_with_system(|_| panic!("must not mask missing manifest")).err().unwrap();
    assert!(error.to_string().contains("manifest.toml"));
    std::fs::write(root.join("manifest.toml"), MANIFEST).unwrap();
    let error = catalog.load_with_system(|_| panic!("must not mask missing font")).err().unwrap();
    assert!(error.to_string().contains("NotoSans[wght].ttf"));
    let mut bytes = std::fs::read(fixture().root().join("NotoSans[wght].ttf")).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(root.join("NotoSans[wght].ttf"), bytes).unwrap();
    let error = catalog.load_with_system(|_| panic!("must not mask corruption")).err().unwrap();
    assert!(error.to_string().contains("SHA-256"));
}

#[test]
fn canonical_scripts_resolve_the_declared_families() {
    let mut fs = fixture().load().unwrap();
    for (text, expected) in [
        ("مرحبا", "Noto Sans Arabic"), ("שלום", "Noto Sans Hebrew"),
        ("नमस्ते", "Noto Sans Devanagari"), ("বাংলা", "Noto Sans Bengali"),
        ("สวัสดี", "Noto Sans Thai"), ("日本語", "Noto Sans CJK SC"),
        ("ひらがな", "Noto Sans CJK JP"), ("한글", "Noto Sans CJK KR"),
        ("🙂", "Noto Color Emoji"),
    ] {
        let mut buffer = Buffer::new(&mut fs, Metrics::new(18.0, 24.0));
        buffer.set_text(text, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut fs, false);
        let glyphs: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs).collect();
        assert!(!glyphs.is_empty(), "{text}");
        for glyph in glyphs {
            assert_ne!(glyph.glyph_id, 0, "{text}");
            assert!(fs.db().face(glyph.font_id).unwrap().families.iter().any(|f| f.0 == expected), "{text}: expected {expected}");
        }
    }
}

#[test]
fn adwaita_axis_order_is_part_of_the_fixture_contract() {
    use skrifa::{MetadataProvider, FontRef};
    let fs = fixture().load().unwrap();
    let id = fs.db().query(&Query { families: &[Family::Name("Adwaita Sans")], ..Query::default() }).unwrap();
    let tags = fs.db().with_face_data(id, |bytes, index| {
        FontRef::from_index(bytes, index).unwrap().axes().iter().map(|axis| axis.tag().to_string()).collect::<Vec<_>>()
    }).unwrap();
    assert_eq!(tags, ["opsz", "wght"]);
}
```

- [ ] Run `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell cargo nextest run -p ui --lib --profile ci -E 'test(font_catalog::tests::)'`; expect undefined-module/interface compile failure before implementation.
- [ ] Implement `ui/src/font_catalog.rs` with this full loader. Hashes are checked on every load, including release builds; do not weaken release to size-only verification. This is a correctness baseline; measure startup before adding an immutable verified-resource cache later. `include_str!` embeds small metadata only.

```rust
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use cosmic_text::{FontSystem, fontdb};
use serde::Deserialize;
use sha2::{Digest, Sha256};
mod fallback;
#[cfg(test)]
mod tests;

const MANIFEST: &str = include_str!("../assets/fonts/manifest.toml");

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Scope { Product, Fixture }

#[derive(Clone, Debug)]
pub struct FontCatalog { root: PathBuf, locale: String, fixtures: bool, system: bool }

#[derive(Debug, thiserror::Error)]
#[error("qOS font packaging error at {path}: {problem}")]
pub struct FontCatalogError { path: PathBuf, problem: String }

impl FontCatalogError {
    fn at(path: impl Into<PathBuf>, problem: impl ToString) -> Self {
        Self { path: path.into(), problem: problem.to_string() }
    }
}

#[derive(Deserialize)]
struct Manifest { schema: u32, font: Vec<Asset> }
#[derive(Deserialize)]
struct Asset {
    scope: Scope, file: String, bytes: u64, sha256: String,
    reserved_aliases: Vec<String>, faces: Vec<Face>,
}
#[derive(Deserialize)]
struct Face { index: u32, family: String, post_script_name: String, style: String }

pub fn catalog_manifest_sha256() -> String { hex::encode(Sha256::digest(MANIFEST.as_bytes())) }

impl FontCatalog {
    pub fn product(root: impl Into<PathBuf>, locale: impl Into<String>) -> Self {
        Self { root: root.into(), locale: locale.into(), fixtures: false, system: true }
    }
    pub fn controlled_fixture(root: impl Into<PathBuf>, locale: impl Into<String>) -> Self {
        Self { root: root.into(), locale: locale.into(), fixtures: true, system: false }
    }
    pub fn root(&self) -> &Path { &self.root }
    pub fn locale(&self) -> &str { &self.locale }
    pub fn load(&self) -> Result<FontSystem, FontCatalogError> {
        self.load_with_system(fontdb::Database::load_system_fonts)
    }
    // Injection is private, synchronous, and per-call: no test changes a global.
    fn load_with_system(&self, discover: impl FnOnce(&mut fontdb::Database)) -> Result<FontSystem, FontCatalogError> {
        let root = self.root.canonicalize().map_err(|e| FontCatalogError::at(&self.root, e))?;
        let manifest_path = root.join("manifest.toml");
        let shipped = std::fs::read(&manifest_path).map_err(|e| FontCatalogError::at(&manifest_path, e))?;
        if shipped != MANIFEST.as_bytes() {
            return Err(FontCatalogError::at(manifest_path, "manifest differs from this qOS build"));
        }
        let manifest: Manifest = toml::from_str(MANIFEST).map_err(|e| FontCatalogError::at(&manifest_path, e))?;
        if manifest.schema != 1 {
            return Err(FontCatalogError::at(manifest_path, "unsupported catalog schema"));
        }
        let mut db = fontdb::Database::new();
        let mut canonical = HashSet::new();
        let mut reserved = HashSet::new();
        for asset in &manifest.font {
            if asset.scope == Scope::Fixture && !self.fixtures { continue; }
            // Only this catalog's canonical faces reserve names. Product mode
            // can still discover an ambient Adwaita as a user-named font.
            reserved.extend(asset.reserved_aliases.iter().map(|s| s.to_lowercase()));
            let path = root.join(&asset.file);
            let metadata = std::fs::metadata(&path).map_err(|e| FontCatalogError::at(&path, e))?;
            if !metadata.is_file() || metadata.len() != asset.bytes {
                return Err(FontCatalogError::at(&path, format!("expected regular file of {} bytes", asset.bytes)));
            }
            let actual = hash_file(&path)?;
            if actual != asset.sha256 {
                return Err(FontCatalogError::at(&path, format!("SHA-256 mismatch: expected {}, found {actual}", asset.sha256)));
            }
            let ids = db.load_font_source(fontdb::Source::File(path.clone()));
            if ids.len() != asset.faces.len() {
                return Err(FontCatalogError::at(&path, format!("expected {} faces, loaded {}", asset.faces.len(), ids.len())));
            }
            for expected in &asset.faces {
                let found = ids.iter().filter_map(|id| db.face(*id)).find(|face| face.index == expected.index);
                let valid = found.is_some_and(|face| {
                    face.families.iter().any(|name| name.0 == expected.family)
                        && face.post_script_name == expected.post_script_name
                        && face.style == if expected.style == "italic" { fontdb::Style::Italic } else { fontdb::Style::Normal }
                });
                if !valid {
                    return Err(FontCatalogError::at(&path, format!("face {} must be {} / {} ({})", expected.index, expected.family, expected.post_script_name, expected.style)));
                }
            }
            canonical.extend(ids);
        }
        if self.system {
            discover(&mut db);
            let remove: Vec<_> = db.faces().filter(|face| {
                !canonical.contains(&face.id) && face.families.iter().any(|name| reserved.contains(&name.0.to_lowercase()))
            }).map(|face| face.id).collect();
            for id in remove { db.remove_face(id); }
        }
        db.set_sans_serif_family(crate::text_render::UI_SANS_FAMILY);
        db.set_serif_family(crate::text_render::UI_SERIF_FAMILY);
        db.set_monospace_family(crate::text_render::UI_MONOSPACE_FAMILY);
        for asset in &manifest.font {
            if asset.scope == Scope::Fixture && !self.fixtures { continue; }
            for face in &asset.faces {
                let query = fontdb::Query {
                    families: &[fontdb::Family::Name(&face.family)],
                    style: if face.style == "italic" { fontdb::Style::Italic } else { fontdb::Style::Normal },
                    ..fontdb::Query::default()
                };
                if !db.query(&query).is_some_and(|id| canonical.contains(&id)) {
                    return Err(FontCatalogError::at(&root, format!("required canonical family is unavailable: {}", face.family)));
                }
            }
        }
        Ok(FontSystem::new_with_locale_and_db_and_fallback(self.locale.clone(), db, fallback::QosFallback))
    }
}

fn hash_file(path: &Path) -> Result<String, FontCatalogError> {
    let mut file = std::fs::File::open(path).map_err(|e| FontCatalogError::at(path, e))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| FontCatalogError::at(path, e))?;
        if n == 0 { break; }
        let chunk = buffer.get(..n).ok_or_else(|| FontCatalogError::at(path, "font read exceeded its buffer"))?;
        hash.update(chunk);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn configured_font_system_with_catalog(catalog: &FontCatalog) -> Result<FontSystem, FontCatalogError> {
    catalog.load()
}
```

- [ ] Implement `ui/src/font_catalog/fallback.rs`. This retains Unix-style script naming on **all** hosts. The script table follows cosmic-text 0.19's Unix policy; the common list is canonical-first and fixes `Noto Sans Symbols 2` spelling after emoji. Keep an attribution comment to cosmic-text (MIT OR Apache-2.0) for the adapted script table.

```rust
// Script-family mapping adapted from cosmic-text 0.19.0 (MIT OR Apache-2.0).
use cosmic_text::Fallback;
use unicode_script::Script;
pub(super) struct QosFallback;

fn han(locale: &str) -> &'static [&'static str] {
    let normalized = locale.replace('_', "-").to_ascii_lowercase();
    let parts: Vec<_> = normalized.split(['-', '.', '@']).collect();
    match parts.first().copied() {
        Some("ja") => &["Noto Sans CJK JP"],
        Some("ko") => &["Noto Sans CJK KR"],
        Some("zh") if parts.contains(&"hk") => &["Noto Sans CJK HK"],
        Some("zh") if parts.contains(&"tw") || parts.contains(&"hant") => &["Noto Sans CJK TC"],
        _ => &["Noto Sans CJK SC"],
    }
}
impl Fallback for QosFallback {
    fn common_fallback(&self) -> &[&'static str] {
        &["Noto Sans", "Noto Sans Mono", "Noto Sans Symbols", "Noto Color Emoji", "Noto Sans Symbols 2"]
    }
    fn forbidden_fallback(&self) -> &[&'static str] { &[".LastResort"] }
    fn script_fallback(&self, script: Script, locale: &str) -> &[&'static str] {
        match script {
            Script::Adlam => &["Noto Sans Adlam", "Noto Sans Adlam Unjoined"],
            Script::Arabic => &["Noto Sans Arabic"],
            Script::Armenian => &["Noto Sans Armenian"],
            Script::Bengali => &["Noto Sans Bengali"],
            Script::Bopomofo | Script::Han => han(locale),
            Script::Braille => &["FreeMono"],
            Script::Buhid => &["Noto Sans Buhid"],
            Script::Chakma => &["Noto Sans Chakma"],
            Script::Cherokee => &["Noto Sans Cherokee"],
            Script::Deseret => &["Noto Sans Deseret"],
            Script::Devanagari => &["Noto Sans Devanagari"],
            Script::Ethiopic => &["Noto Sans Ethiopic"],
            Script::Georgian => &["Noto Sans Georgian"],
            Script::Gothic => &["Noto Sans Gothic"],
            Script::Grantha => &["Noto Sans Grantha"],
            Script::Gujarati => &["Noto Sans Gujarati"],
            Script::Gurmukhi => &["Noto Sans Gurmukhi"],
            Script::Hangul => &["Noto Sans CJK KR"],
            Script::Hanunoo => &["Noto Sans Hanunoo"],
            Script::Hebrew => &["Noto Sans Hebrew"],
            Script::Hiragana | Script::Katakana => &["Noto Sans CJK JP"],
            Script::Javanese => &["Noto Sans Javanese"],
            Script::Kannada => &["Noto Sans Kannada"],
            Script::Khmer => &["Noto Sans Khmer"],
            Script::Lao => &["Noto Sans Lao"],
            Script::Malayalam => &["Noto Sans Malayalam"],
            Script::Mongolian => &["Noto Sans Mongolian"],
            Script::Myanmar => &["Noto Sans Myanmar"],
            Script::Oriya => &["Noto Sans Oriya"],
            Script::Runic => &["Noto Sans Runic"],
            Script::Sinhala => &["Noto Sans Sinhala"],
            Script::Syriac => &["Noto Sans Syriac"],
            Script::Tagalog => &["Noto Sans Tagalog"],
            Script::Tagbanwa => &["Noto Sans Tagbanwa"],
            Script::Tai_Le => &["Noto Sans Tai Le"],
            Script::Tai_Tham => &["Noto Sans Tai Tham"],
            Script::Tai_Viet => &["Noto Sans Tai Viet"],
            Script::Tamil => &["Noto Sans Tamil"],
            Script::Telugu => &["Noto Sans Telugu"],
            Script::Thaana => &["Noto Sans Thaana"],
            Script::Thai => &["Noto Sans Thai"],
            Script::Tibetan => &["Noto Serif Tibetan"],
            Script::Tifinagh => &["Noto Sans Tifinagh"],
            Script::Vai => &["Noto Sans Vai"],
            Script::Yi => &["Noto Sans Yi", "Noto Sans CJK SC"],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locale_forms_and_script_overrides_are_stable() {
        for (locale, family) in [
            ("en-US", "SC"), ("ja", "JP"), ("ja-JP", "JP"),
            ("ja_JP.UTF-8", "JP"), ("KO-kr", "KR"), ("zh-TW", "TC"),
            ("zh_Hant_TW", "TC"), ("zh-Hant", "TC"), ("zh-Hant-HK", "HK"),
            ("zh_CN", "SC"),
        ] {
            assert_eq!(han(locale), [format!("Noto Sans CJK {family}")]);
            assert_eq!(QosFallback.script_fallback(Script::Hiragana, locale), ["Noto Sans CJK JP"]);
            assert_eq!(QosFallback.script_fallback(Script::Hangul, locale), ["Noto Sans CJK KR"]);
        }
        assert_eq!(QosFallback.common_fallback(), ["Noto Sans", "Noto Sans Mono", "Noto Sans Symbols", "Noto Color Emoji", "Noto Sans Symbols 2"]);
        assert_eq!(QosFallback.forbidden_fallback(), [".LastResort"]);
    }
}
```

- [ ] In `TextRenderer` replace the constructor header and first field with this exact factoring; retain all remaining initializer fields unchanged, through `text_scale: 1.0`. The private constructor has no platform/font policy.

```rust
pub fn new() -> Self {
    Self::with_font_system(configured_font_system())
}
pub fn with_font_catalog(catalog: &crate::font_catalog::FontCatalog) -> Result<Self, crate::font_catalog::FontCatalogError> {
    Ok(Self::with_font_system(crate::font_catalog::configured_font_system_with_catalog(catalog)?))
}
fn with_font_system(font_system: FontSystem) -> Self {
    Self {
        font_system,
        swash_cache: SwashCache::new(),
        font_cache: HashMap::new(),
        measure_cache: HashMap::with_capacity(256),
        generation: 0,
        hits: 0,
        misses: 0,
        shape_cache: HashMap::with_capacity(256),
        shape_hits: 0,
        shape_misses: 0,
        shape_ns: 0,
        text_scale: 1.0,
    }
}
```

- [ ] Add a constructor regression in `text_render::tests`:

```rust
#[test]
fn explicit_catalog_constructs_the_production_renderer_and_caches() {
    let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"), "en-US");
    let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
    let first = renderer.measure("Reading", 16.0, None);
    let second = renderer.measure("Reading", 16.0, None);
    assert_eq!(first, second);
    assert!(first.0 > 0.0 && first.1 > 0.0);
    assert_eq!(renderer.hits, 1);
    assert_eq!(renderer.misses, 1);
}
```

- [ ] Run the font-catalog tests plus explicit constructor test with `--locked`; run `cargo clippy -p ui --lib --locked -- -D warnings`. Add narrowly justified lint allowances where the repository's deny policy flags the small manifest-validation function/compatibility wrapper; do not blanket-allow the module. Record loaded face metadata if exact validation fails and compare with original SFNT evidence before changing the manifest. Commit as `feat(ui): load shared canonical fonts with explicit fixture policy`.

