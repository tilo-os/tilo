#!/usr/bin/env python3
"""Every catalogue a unit ships says every word its source catalogue says,
and nothing its source does not.

A unit's source language is `en-US`, the file its crate or package is written
in; the files beside it are translations. A message a translation lacks reads
in the source language for that string alone, which is the fallback every
lookup has (docs/designs/app-language/i18n.md) — one string, never a surface.
What this check finds is both shapes of drift: a catalogue that is partly
translated, so one window reads in two languages at once, and a translation
that adds a word its source has no original for, which reads in a translation
inside a window speaking the source language.

A locale may add messages of its own: the arrival screen's German jokes are
written for German speakers and have no English source. Those are not drift —
they are listed in [`LOCALE_ONLY`], and the source needs nothing.

A key a catalogue defines twice is not drift either: Fluent keeps one message
per key, so the second one breaks the whole file at load. It fails here, in
every mode, because no listing of keys should hide it.

  check_catalogue_coverage.py            fail on every drifted or repeated key
  check_catalogue_coverage.py --list     print the drift, `unit locale key`, and pass
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_copy_language import messages  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
SOURCE_LOCALE = "en-US"

# Keys a locale adds on purpose: copy written for speakers of that language,
# with no source-language original.
LOCALE_ONLY: dict[tuple[str, str], set[str]] = {
    ("crates/browser/tilo-browser-runtime/web", "de-DE"): {
        "arrival-joke-fax",
        "arrival-joke-form-a38",
        "arrival-joke-recycling",
    },
}


def catalogues(root: Path) -> list[Path]:
    """Every catalogue in the tree, the source languages and the translations."""
    return sorted(
        path
        for path in root.rglob("locales/*.ftl")
        if "target" not in path.relative_to(root).parts
        and not any(part.startswith(".") for part in path.relative_to(root).parts)
    )


def unit_of(root: Path, catalogue: Path) -> str:
    """The crate or package that ships `catalogue`: the directory holding `locales/`."""
    return catalogue.parents[1].relative_to(root).as_posix()


def keys_of(catalogue: Path) -> dict[str, int]:
    """The message and attribute keys `catalogue` defines, each by its line."""
    return {message.key: message.line for message in messages(catalogue.read_text())}


def duplicated(root: Path) -> list[tuple[Path, str, int, int]]:
    """Per (catalogue, key, first line, repeated line), the keys it defines twice.

    Fluent holds one message per key: a second definition is a parse failure for
    everything that loads the catalogue, so it is not a word to translate but a
    broken file. Indexing the entries by key, as the rest of this check does,
    would read only the last and pass."""
    found = []
    for catalogue in catalogues(root):
        first: dict[str, int] = {}
        for message in messages(catalogue.read_text()):
            if message.key in first:
                found.append((catalogue, message.key, first[message.key], message.line))
            else:
                first[message.key] = message.line
    return found


def untranslated(root: Path) -> list[tuple[str, str, Path, str, int]]:
    """Per (unit, locale, catalogue, key, source line), the keys a translation lacks."""
    units: dict[str, dict[str, Path]] = {}
    for catalogue in catalogues(root):
        units.setdefault(unit_of(root, catalogue), {})[catalogue.stem] = catalogue
    missing = []
    for unit, locales in sorted(units.items()):
        defined = {locale: keys_of(path) for locale, path in locales.items()}
        source = defined.get(SOURCE_LOCALE)
        if source is None:
            continue
        for key, line in source.items():
            for locale, keys in sorted(defined.items()):
                if locale != SOURCE_LOCALE and key not in keys:
                    missing.append((unit, locale, locales[locale], key, line))
    return missing


def unnamed(root: Path) -> list[tuple[str, str, Path, str, int]]:
    """Per (unit, locale, catalogue, key, line), the keys a translation adds."""
    units: dict[str, dict[str, Path]] = {}
    for catalogue in catalogues(root):
        units.setdefault(unit_of(root, catalogue), {})[catalogue.stem] = catalogue
    extra = []
    for unit, locales in sorted(units.items()):
        defined = {locale: keys_of(path) for locale, path in locales.items()}
        source = defined.get(SOURCE_LOCALE)
        if source is None:
            continue
        for locale, keys in sorted(defined.items()):
            if locale == SOURCE_LOCALE:
                continue
            allowed = LOCALE_ONLY.get((unit, locale), set())
            for key, line in keys.items():
                if key not in source and key not in allowed:
                    extra.append((unit, locale, locales[locale], key, line))
    return extra


def named(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def main(argv: list[str]) -> int:
    if argv[1:] not in ([], ["--list"]):
        print(__doc__.split("\n\n")[-1].strip(), file=sys.stderr)
        return 2
    missing = untranslated(ROOT)
    extra = unnamed(ROOT)
    twice = duplicated(ROOT)
    listing = argv[1:] == ["--list"]
    for unit, locale, _, key, line in missing:
        if listing:
            print(unit, locale, key)
        else:
            print(f"{unit}/locales/{SOURCE_LOCALE}.ftl:{line}: {key} has no {locale} text")
    for unit, locale, _, key, line in extra:
        if listing:
            print(unit, locale, key)
        else:
            print(f"{unit}/locales/{locale}.ftl:{line}: {key} has no {SOURCE_LOCALE} text")
    for catalogue, key, first, line in twice:
        print(f"{named(ROOT, catalogue)}:{line}: {key} is defined again; line {first} "
              "already defines it", file=sys.stderr)
    if twice:
        print(f"{len(twice)} keys are defined twice; a catalogue holds one message per key",
              file=sys.stderr)
        return 1
    if listing:
        return 0
    if missing or extra:
        print(f"{len(missing) + len(extra)} keys are untranslated or unnamed; a catalogue is "
              "translated whole or it reads in two languages at once", file=sys.stderr)
        return 1
    print("catalogue coverage OK (every translation says every word of its source, and no more)",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
