#!/usr/bin/env python3
"""Regenerate the emoji crate's checked-in data from pinned Unicode sources.

    python3 crates/text/emoji/tools/regenerate.py            # download pinned sources
    python3 crates/text/emoji/tools/regenerate.py --cache D  # reuse/keep downloads in D

Every input is pinned by version and SHA-256, and the output is a pure function
of those inputs, so a rerun on any machine produces byte-identical files.

The emoji version tracks the colour emoji font the OS ships (Noto Color Emoji
2025-06 covers Emoji 16.0): offering a sequence the font cannot draw would put
tofu in the picker. CLDR 46 is the release aligned with Emoji 16.0.

Outputs (see src/data.rs for the reader):

  data/emoji.txt   locale-independent structure: groups, subgroups, the
                   fully-qualified emoji in Unicode order, which accept a skin
                   tone, uniform-tone overrides and gender variant triples.
  data/en.txt      English names, keywords and group labels.
  data/<l>.txt     the same for each other locale in LOCALE_SOURCES, from its
                   CLDR annotations and characterLabels. An emoji or group CLDR
                   leaves untranslated is omitted, and the reader keeps its
                   English name. build.rs deflates these files into the crate.

Adding a locale is one LOCALE_SOURCES entry and a rerun.
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import re
import sys
import urllib.request
import xml.etree.ElementTree as ET

EMOJI_VERSION = "16.0"
CLDR_RELEASE = "release-46"

CLDR = f"https://raw.githubusercontent.com/unicode-org/cldr/{CLDR_RELEASE}/common"

# Per locale: SHA-256 of CLDR's annotations/<l>.xml, annotationsDerived/<l>.xml
# and, for translated group labels, main/<l>.xml. English group labels come
# from emoji-test.txt, so English needs no main file.
LOCALE_SOURCES = {
    "en": (
        "7d491f1782480d50ee953d3943e8a65eacaa4394b41af9e9f89c9be418256ade",
        "461d1578079c5ebc947e506df6b5a55c93f006160e8dff3f05dcf917ce081604",
        None,
    ),
    "de": (
        "4cf875e5487f8d542d1c8634edd3a83d3020f15fa7c9d64d98c7b0e265edbb67",
        "1476a74a3d3cfc8880d92b96ac086863a4e37f0a64f695c862a582fa5dbaef2f",
        "9187264ac8c99b8654802b418081098918940183a246d02e53d4fbb5cef3d6f5",
    ),
    "es": (
        "baeb76f846321d3838a93bf63126ea52a35240987f555c6cd87651a01f734955",
        "1779e3aa00cbc56533dc218b735643acf27967c9f4ab6ee58d0a349d9745019c",
        "05e5b29fc098457a59b49eb2edc8c010b5b7cfacc549bb50405ae18973b045e5",
    ),
    "fr": (
        "03a8f16c52ef592eb77d56bf9407bb166837cf3ba9d87e53f59e0a963e00bbd9",
        "42af059617558844564c232116f9c06a7878028259dc171233e4a167d021f7cf",
        "8f8a56993bc40ec9a32e8bf9bf2a585b16c42719e0c73f56614535152ddc58ea",
    ),
    "it": (
        "08c7220fb41c122ac7ea6583012e53a1538a1acb545dfcbe6d2b4f83493f3c81",
        "1f4e7808261c372524aef50d6ebe6174d869d82ec8afccaf8dae91f9f3e49458",
        "49410d81008aeb0eaa52513a86487405322512ac3884d23a0995cc3f3514e1c3",
    ),
    "ja": (
        "ba0fc62bf90b7ab33b47593825aee3745cfabdcee3d6d322cef89100c409ca1e",
        "7349579fd7bd3f09a2a62d06ba2a270b88df78d58f834cc87e9fd373f89c4ae1",
        "3e5ff4a7cf05cbef79cc338553bcfcb8347c73fbfcc707068b87ecb4132bd222",
    ),
    "zh": (
        "487e16c5e64b6ac1cb38a91b5f2dc25349ded224bcd78ec1f14a92bba3ef3147",
        "01d48d98a8612f779c17707db51f2ba2c55292d6c690f8a0828dd748c042206a",
        "bc40d32d5f602bae6b5e4ab58f4983a533f3a28c14c4642afca57b095d067862",
    ),
    "zh_Hant": (
        "2d7ee90b147b127b1e42214784892cc98cdd4a7a294ebb1f37bdf00489c4ba6b",
        "406b93a2b61d4161fc356931a1321f3301fcc07aecda9ad4221cb4137ffb2caa",
        "e846d2247b1fb0d5aaa86e3633f2cf2784fe3266a176ab7eb0b61884c1d12e0d",
    ),
}

SOURCES = {
    "emoji-test.txt": (
        f"https://www.unicode.org/Public/emoji/{EMOJI_VERSION}/emoji-test.txt",
        "24f0c534e86cf142e2496953e8f0e46a3e702392911eddcd29c6cced85139697",
    ),
}
for _loc, (_ann, _derived, _main) in LOCALE_SOURCES.items():
    SOURCES[f"annotations-{_loc}.xml"] = (f"{CLDR}/annotations/{_loc}.xml", _ann)
    SOURCES[f"annotations-derived-{_loc}.xml"] = (
        f"{CLDR}/annotationsDerived/{_loc}.xml",
        _derived,
    )
    if _main:
        SOURCES[f"main-{_loc}.xml"] = (f"{CLDR}/main/{_loc}.xml", _main)

# CLDR's characterLabel for each Unicode group, for translated tab labels.
GROUP_LABELS = {
    "smileys-emotion": "smiley",
    "people-body": "person",
    "animals-nature": "animals_nature",
    "food-drink": "food_drink",
    "travel-places": "travel_places",
    "activities": "activities",
    "objects": "objects",
    "symbols": "symbols",
    "flags": "flags",
}

# CLDR's marker for "inherit this value from the parent locale".
INHERIT = "↑↑↑"

TONES = [chr(c) for c in range(0x1F3FB, 0x1F400)]
ZWJ = "‍"
VS16 = "️"
MALE = ZWJ + "♂" + VS16
FEMALE = ZWJ + "♀" + VS16
MAN, WOMAN, PERSON = "\U0001f468", "\U0001f469", "\U0001f9d1"

HERE = pathlib.Path(__file__).resolve().parent
DATA = HERE.parent / "data"


def fetch(cache: pathlib.Path | None) -> dict[str, bytes]:
    out = {}
    for name, (url, sha) in SOURCES.items():
        path = cache / name if cache else None
        if path and path.exists():
            body = path.read_bytes()
        else:
            with urllib.request.urlopen(url) as resp:
                body = resp.read()
            if path:
                path.write_bytes(body)
        got = hashlib.sha256(body).hexdigest()
        if got != sha:
            sys.exit(f"{name}: sha256 {got} != pinned {sha} ({url})")
        out[name] = body
    return out


def slug(label: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", label.lower()).strip("-")


def parse_emoji_test(text: str):
    """[(group_label, subgroup, [emoji...])] of fully-qualified sequences."""
    groups: list[tuple[str, list[tuple[str, list[str]]]]] = []
    names: dict[str, str] = {}
    for line in text.splitlines():
        if line.startswith("# group:"):
            groups.append((line.split(":", 1)[1].strip(), []))
        elif line.startswith("# subgroup:"):
            groups[-1][1].append((line.split(":", 1)[1].strip(), []))
        elif line and not line.startswith("#"):
            cps, rest = line.split(";", 1)
            status, comment = rest.split("#", 1)
            if status.strip() != "fully-qualified":
                continue
            seq = "".join(chr(int(c, 16)) for c in cps.split())
            groups[-1][1][-1][1].append(seq)
            # "# 😀 E1.0 grinning face"
            names[seq] = comment.strip().split(" ", 2)[2]
    return groups, names


def strip_tones(seq: str) -> str:
    return "".join(c for c in seq if c not in TONES)


def naive_tone(seq: str, tone: str, bases: set[str]) -> str:
    """Put `tone` after every modifier base, replacing a following VS16."""
    out = []
    i = 0
    while i < len(seq):
        c = seq[i]
        out.append(c)
        if c in bases:
            out.append(tone)
            if i + 1 < len(seq) and seq[i + 1] == VS16:
                i += 1
        i += 1
    return "".join(out)


def parse_annotations(*blobs: bytes) -> tuple[dict[str, str], dict[str, list[str]]]:
    tts: dict[str, str] = {}
    keywords: dict[str, list[str]] = {}
    for blob in blobs:
        root = ET.fromstring(blob)
        for node in root.iter("annotation"):
            cp = node.get("cp", "").replace(VS16, "")
            text = (node.text or "").strip()
            if not text or text == INHERIT:
                continue
            if node.get("type") == "tts":
                tts[cp] = text
            else:
                keywords[cp] = [k.strip() for k in text.split("|") if k.strip()]
    return tts, keywords


def clean(field: str) -> str:
    return field.replace("\t", " ").replace("\n", " ").replace("|", "/")


def group_labels(main: bytes) -> dict[str, str]:
    """Group id -> CLDR's translated characterLabel, first letter raised."""
    labels = {}
    wanted = {cldr: gid for gid, cldr in GROUP_LABELS.items()}
    for node in ET.fromstring(main).iter("characterLabel"):
        gid = wanted.get(node.get("type", ""))
        text = (node.text or "").strip()
        if gid and text and text != INHERIT:
            labels[gid] = text[:1].upper() + text[1:]
    return labels


def locale_lines(
    header: str,
    groups,
    base_list: list[str],
    labels: dict[str, str],
    names: dict[str, str],
    tts: dict[str, str],
    keywords: dict[str, list[str]],
) -> list[str]:
    """One names file. An emoji or group without a name here is left out, and
    the reader falls back to English for it."""
    loc = [header.rstrip("\n")]
    loc.append("# G <group id> <label> / E <emoji> <name> <keyword|keyword|...>")
    for label, _ in groups:
        gid = slug(label)
        if gid in labels:
            loc.append(f"G\t{gid}\t{clean(labels[gid])}")
    for e in base_list:
        key = e.replace(VS16, "")
        name = tts.get(key) or names.get(e)
        if not name:
            continue
        kws = []
        for k in keywords.get(key, []):
            k = clean(k)
            if k != name and k not in kws:
                kws.append(k)
        loc.append(f"E\t{e}\t{clean(name)}\t{'|'.join(kws)}")
    return loc


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", type=pathlib.Path)
    args = ap.parse_args()
    if args.cache:
        args.cache.mkdir(parents=True, exist_ok=True)
    src = fetch(args.cache)

    groups, test_names = parse_emoji_test(src["emoji-test.txt"].decode("utf-8"))
    groups = [g for g in groups if g[0] != "Component"]
    fq = {e for _, subs in groups for _, es in subs for e in es}

    # A code point is a modifier base when a fully-qualified sequence puts a
    # tone right after it.
    bases = set()
    for e in fq:
        for i, c in enumerate(e[:-1]):
            if e[i + 1] in TONES:
                bases.add(c)

    # Uniform-tone variants per untoned emoji, straight from the test file.
    uniform: dict[str, dict[str, str]] = {}
    for e in fq:
        tones = {c for c in e if c in TONES}
        if len(tones) == 1:
            base_key = strip_tones(e).replace(VS16, "")
            uniform.setdefault(base_key, {})[tones.pop()] = e

    base_list = []  # untoned entries, Unicode order
    toneable: set[str] = set()
    overrides: dict[str, list[str]] = {}
    for _, subs in groups:
        for _, es in subs:
            for e in es:
                if any(c in TONES for c in e):
                    continue
                base_list.append(e)
                forms = uniform.get(e.replace(VS16, ""), {})
                if len(forms) != len(TONES):
                    continue
                toneable.add(e)
                wanted = [forms[t] for t in TONES]
                if [naive_tone(e, t, bases) for t in TONES] != wanted:
                    overrides[e] = wanted

    untoned = set(base_list)
    genders = []
    for e in base_list:
        man = woman = None
        if e.startswith(PERSON + ZWJ):
            rest = e[len(PERSON):]
            man, woman = MAN + rest, WOMAN + rest
        elif e == PERSON:
            man, woman = MAN, WOMAN
        elif not e.endswith((MALE, FEMALE)) and not e.startswith((MAN, WOMAN)):
            man, woman = e + MALE, e + FEMALE
        else:
            continue
        man = man if man in untoned else None
        woman = woman if woman in untoned else None
        if man or woman:
            genders.append((e, man or "", woman or ""))

    header = (
        f"# Generated by tools/regenerate.py from Unicode Emoji {EMOJI_VERSION} "
        f"emoji-test.txt and CLDR {CLDR_RELEASE}.\n"
        "# Do not edit by hand. Unicode data: see LICENSE-UNICODE.txt.\n"
    )

    lines = [header.rstrip("\n")]
    lines.append("# M <modifier base code points>")
    lines.append("M\t" + " ".join(f"{ord(c):X}" for c in sorted(bases)))
    lines.append("# G <group id> / S <subgroup id> / E <emoji> <flags: t = takes a skin tone>")
    for label, subs in groups:
        lines.append(f"G\t{slug(label)}")
        for sub, es in subs:
            lines.append(f"S\t{sub}")
            for e in es:
                if e in untoned:
                    lines.append(f"E\t{e}\t{'t' if e in toneable else ''}")
    lines.append("# T <emoji> <light> <medium-light> <medium> <medium-dark> <dark>")
    for e in base_list:
        if e in overrides:
            lines.append("T\t" + "\t".join([e, *overrides[e]]))
    lines.append("# V <gender-neutral> <man> <woman> (empty when absent)")
    for n, m, w in genders:
        lines.append(f"V\t{n}\t{m}\t{w}")
    (DATA / "emoji.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")

    for locale, (_, _, main_sha) in LOCALE_SOURCES.items():
        tts, keywords = parse_annotations(
            src[f"annotations-{locale}.xml"], src[f"annotations-derived-{locale}.xml"]
        )
        if main_sha:
            labels, names = group_labels(src[f"main-{locale}.xml"]), {}
        else:
            # English is complete: every group and emoji has a name, taken
            # from emoji-test.txt where CLDR has none.
            labels = {slug(label): label for label, _ in groups}
            names = test_names
        loc = locale_lines(header, groups, base_list, labels, names, tts, keywords)
        (DATA / f"{locale}.txt").write_text("\n".join(loc) + "\n", encoding="utf-8")
        named = sum(1 for line in loc if line.startswith("E\t"))
        print(f"{locale}: {named} of {len(base_list)} emoji named")

    print(
        f"{len(base_list)} emoji, {len(toneable)} toneable "
        f"({len(overrides)} overrides), {len(genders)} gender triples"
    )


if __name__ == "__main__":
    main()
