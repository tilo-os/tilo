#!/usr/bin/env python3
"""Build the language packs: every catalogue's translations, one directory per locale.

An app's package carries only its source catalogue (`locales/en-US.ftl`), the
fallback every message resolves to. Each other language ships once, for every
app and system component, as a language pack: `<out>/<locale>/<crate>.ftl`
for each crate whose catalogue (`ui::embedded_catalog!`) has that language
(one pack per language: a crate's `de-DE.ftl` and another's `de.ftl` both go
to `de`),
where `<crate>` is the crate's package name. The OS image carries them under
`/usr/share/tilo/language-packs`; the registry offers each as a signed
`kind = "language-pack"` package (`so.tilo.language-pack.<locale>`, data for
every architecture), which the installer service puts in the person's
`$XDG_DATA_HOME/tilo/language-packs` when they choose its language. The
runtime reads the person's packs first (ui::i18n::language_pack_roots).

    language_packs.py build REPO OUT     write the packs under OUT
    language_packs.py list REPO          print `<locale> <crate> <path>` per catalogue
    language_packs.py pack REPO OUT VERSION IDENTITY PUBKEY PKG
                                         sign one package per locale into OUT with
                                         the tilo-app-package binary PKG, beside its
                                         registry metadata and an index.json
"""

from __future__ import annotations

import pathlib
import re
import json
import shutil
import subprocess
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


def language(locale: str) -> str:
    """The language a locale tag names (`de` of `de-DE`), the name of its pack."""
    return re.split(r"[-_]", locale, maxsplit=1)[0].lower()


def build(repo: pathlib.Path, out: pathlib.Path) -> int:
    """One pack per language: every crate's catalogue of a language, whether
    the crate tags it `de` or `de-DE`, goes to `<out>/de/<crate>.ftl`. A crate
    with two different catalogues of one language would need a regional
    overlay, which nothing has, so it is refused rather than one picked."""
    if out.exists():
        shutil.rmtree(out)
    # A tree with no translations packs none, and says so in an empty index.
    out.mkdir(parents=True)
    seen: dict[tuple[str, str], pathlib.Path] = {}
    for locale, crate, path in catalogues(repo):
        pack = language(locale)
        known = seen.get((pack, crate))
        if known is not None:
            if known.read_bytes() == path.read_bytes():
                continue
            print(f"{crate} has two different {pack} catalogues: {known} and {path}; "
                  "one pack per language takes one", file=sys.stderr)
            return 1
        seen[pack, crate] = path
        target = out / pack / f"{crate}.ftl"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
    languages = sorted({pack for pack, _ in seen})
    print(f"language packs: {len(seen)} catalogues in {len(languages)} languages "
          f"({', '.join(languages) or 'none'}) under {out}")
    return 0


PACKAGE_PREFIX = "so.tilo.language-pack."


def manifest(locale: str, version: str, pubkey: str) -> str:
    """The signed manifest of `locale`'s pack: data, for every architecture."""
    return (
        "[package]\n"
        f'id = "{PACKAGE_PREFIX}{locale.lower()}"\n'
        f'name = "Language pack {locale}"\n'
        'category = "System"\n'
        f'version = "{version}"\n'
        'kind = "language-pack"\n\n'
        "[publisher]\n"
        f'identity = "ed25519:{pubkey}"\n'
        'signature = ""\n\n'
        "[sandbox]\n"
        "namespaces = []\n"
        'seccomp = "tilo-default"\n'
        'landlock = "strict"\n\n'
        "[sandbox.net]\n"
        'mode = "off"\n'
    )


def metadata(locale: str) -> dict:
    """The registry listing of `locale`'s pack."""
    return {
        "name": {"en": f"Language pack {locale}"},
        "description": {
            "en": f"Every Tilo app's and the desktop's words in {locale}. Settings installs "
            "it when a person chooses the language."
        },
        "tagline": {"en": f"Tilo in {locale}"},
        "supported_languages": ["en"],
        "source_url": "https://code.tilo.so/tilo/tilo",
    }


def pack(repo: pathlib.Path, out: pathlib.Path, version: str, identity: str, pubkey: str,
         pkg: str) -> int:
    work = out / "work"
    status = build(repo, work / "packs")
    if status:
        return status
    index = []
    for locale_dir in sorted((work / "packs").iterdir()):
        locale = locale_dir.name
        payload = work / "payload" / locale
        payload.mkdir(parents=True)
        shutil.copytree(locale_dir, payload / locale)
        manifest_path = work / f"{locale}.toml"
        manifest_path.write_text(manifest(locale, version, pubkey), encoding="utf-8")
        tapp = out / f"language-pack-{locale.lower()}-{version}.tapp"
        for command in (
            [pkg, "pack", "--manifest", str(manifest_path), "--payload", str(payload),
             "--identity", identity, "--output", str(tapp)],
            [pkg, "verify", "--trusted-key", pubkey, str(tapp)],
        ):
            done = subprocess.run(command, check=False)
            if done.returncode:
                print(f"language pack {locale}: {command[1]} failed", file=sys.stderr)
                return done.returncode
        listing = out / f"language-pack-{locale.lower()}.json"
        listing.write_text(json.dumps(metadata(locale), indent=2), encoding="utf-8")
        index.append({"id": PACKAGE_PREFIX + locale.lower(), "locale": locale,
                      "file": tapp.name, "metadata": listing.name})
    shutil.rmtree(work)
    (out / "index.json").write_text(json.dumps({"format": 1, "packs": index}, indent=2),
                                    encoding="utf-8")
    print(f"language packs: signed {len(index)} package(s) under {out}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) == 7 and argv[0] == "pack":
        return pack(pathlib.Path(argv[1]).resolve(), pathlib.Path(argv[2]), argv[3], argv[4],
                    argv[5], argv[6])
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
