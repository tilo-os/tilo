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
  data/en.txt      English names, keywords and group labels. Another locale is
                   another <locale>.txt in the same format.
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

SOURCES = {
    "emoji-test.txt": (
        f"https://www.unicode.org/Public/emoji/{EMOJI_VERSION}/emoji-test.txt",
        "24f0c534e86cf142e2496953e8f0e46a3e702392911eddcd29c6cced85139697",
    ),
    "annotations-en.xml": (
        f"https://raw.githubusercontent.com/unicode-org/cldr/{CLDR_RELEASE}/common/annotations/en.xml",
        "7d491f1782480d50ee953d3943e8a65eacaa4394b41af9e9f89c9be418256ade",
    ),
    "annotations-derived-en.xml": (
        f"https://raw.githubusercontent.com/unicode-org/cldr/{CLDR_RELEASE}/common/annotationsDerived/en.xml",
        "461d1578079c5ebc947e506df6b5a55c93f006160e8dff3f05dcf917ce081604",
    ),
}

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
            if node.get("type") == "tts":
                tts[cp] = text
            else:
                keywords[cp] = [k.strip() for k in text.split("|") if k.strip()]
    return tts, keywords


def clean(field: str) -> str:
    return field.replace("\t", " ").replace("\n", " ").replace("|", "/")


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

    tts, keywords = parse_annotations(
        src["annotations-en.xml"], src["annotations-derived-en.xml"]
    )

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

    loc = [header.rstrip("\n")]
    loc.append("# G <group id> <label> / E <emoji> <name> <keyword|keyword|...>")
    for label, _ in groups:
        loc.append(f"G\t{slug(label)}\t{label}")
    for e in base_list:
        key = e.replace(VS16, "")
        name = tts.get(key) or test_names[e]
        kws = []
        for k in keywords.get(key, []):
            if k != name and k not in kws:
                kws.append(clean(k))
        loc.append(f"E\t{e}\t{clean(name)}\t{'|'.join(kws)}")
    (DATA / "en.txt").write_text("\n".join(loc) + "\n", encoding="utf-8")

    print(
        f"{len(base_list)} emoji, {len(toneable)} toneable "
        f"({len(overrides)} overrides), {len(genders)} gender triples"
    )


if __name__ == "__main__":
    main()
