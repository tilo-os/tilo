#!/usr/bin/env python3
"""Build the language packs: every catalogue's translations, one directory per locale.

An app's package carries only its source catalogue (`locales/en-US.ftl`), the
fallback every message resolves to. Each other language ships once, for every
app and system component, as a language pack: `<out>/<locale>/<crate>.ftl`
for each crate whose catalogue (`ui::embedded_catalog!`) has that locale,
where `<crate>` is the crate's package name. The runtime reads them from
`/var/lib/tilo/language-packs` and `/usr/share/tilo/language-packs`
(ui::i18n::LanguagePack); the OS image installs them under the latter.

    language_packs.py build REPO OUT     write the packs under OUT
    language_packs.py list REPO          print `<locale> <crate> <path>` per catalogue
"""

from __future__ import annotations

import pathlib
import re
import shutil
import sys
import tomllib

SOURCE_LOCALE = "en-US"
# An invocation of the macro, not its definition or its documentation.
MACRO = re.compile(r"^\s*(?:ui::|crate::)?embedded_catalog!\(", re.MULTILINE)


def catalogues(repo: pathlib.Path) -> list[tuple[str, str, pathlib.Path]]:
    """`(locale, crate, path)` for every translated catalogue of a crate
    that declares its catalogue with the macro, sorted."""
    found = []
    for manifest in sorted(repo.glob("**/Cargo.toml")):
        crate_dir = manifest.parent
        if "target" in crate_dir.relative_to(repo).parts:
            continue
        locales = crate_dir / "locales"
        if not locales.is_dir():
            continue
        package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        name = package.get("name")
        if not name:
            continue
        sources = crate_dir / "src"
        if not sources.is_dir() or not any(
            MACRO.search(path.read_text(encoding="utf-8", errors="replace"))
            for path in sources.rglob("*.rs")
        ):
            continue
        for ftl in sorted(locales.glob("*.ftl")):
            locale = ftl.stem
            if locale == SOURCE_LOCALE or not re.fullmatch(r"[A-Za-z]{2,3}([-_][A-Za-z0-9]+)*", locale):
                continue
            found.append((locale, name, ftl))
    return found


def build(repo: pathlib.Path, out: pathlib.Path) -> int:
    if out.exists():
        shutil.rmtree(out)
    seen: dict[tuple[str, str], pathlib.Path] = {}
    for locale, crate, path in catalogues(repo):
        if (locale, crate) in seen:
            print(f"{crate} has two {locale} catalogues: {seen[locale, crate]} and {path}",
                  file=sys.stderr)
            return 1
        seen[locale, crate] = path
        target = out / locale / f"{crate}.ftl"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    locales = sorted({locale for locale, _ in seen})
    print(f"language packs: {len(seen)} catalogues in {len(locales)} locales "
          f"({', '.join(locales) or 'none'}) under {out}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) == 3 and argv[0] == "build":
        return build(pathlib.Path(argv[1]).resolve(), pathlib.Path(argv[2]))
    if len(argv) == 2 and argv[0] == "list":
        repo = pathlib.Path(argv[1]).resolve()
        for locale, crate, path in catalogues(repo):
            print(locale, crate, path.relative_to(repo))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
