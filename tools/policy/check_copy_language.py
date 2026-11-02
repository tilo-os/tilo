#!/usr/bin/env python3
"""User-facing copy follows the house language rules in every catalogue.

Copy tells a person a state ("Offline"), an outcome ("Version restored.") or a
next step ("Unlock the keyring and try again"), in the words they already use.
Every `locales/*.ftl` in the tree is checked for the classes of copy that break
that (HIG section 4):

- narration: the software describing its own mechanics ("Tilo checks…",
  "Your files appear here", "This page shows…");
- implementation: words from the implementation, not from the person's world
  ("compositor", "the workspace document did not…", "certified", "admitted",
  a job or surface number);
- raw-id: a placeable that interpolates a machine id (`{ $surface }`,
  `{ $job }`, `{ $app_id }`);
- all-caps: shouted words ("AGENT CONTROLS"); headings are sentence case;
- chain: more than one " · " in a message; a row says one thing, at most a
  name and its value;
- glyph: a separator glyph joining facts ("·", "•", "|", "—", "–" or " / "
  between values) or a unicode symbol standing in for an icon (an arrow, a
  check mark, a cross, a dot, a star). Separate facts are separate
  components: a secondary line, `ui::widget::Facts`, a property list, key
  hints with drawn keycaps; icons come from `ui::icons`
  (docs/design-language.md, "Primitives");
- instruction: telling people how to operate a control ("select to stop")
  instead of naming the control by what it does;
- vague-button: a confirming button named "Continue", "OK", "Yes" or
  "Proceed" instead of by its outcome ("Move", "Delete");
- glossary: a term that `copy_glossary.toml` replaces with the one word the
  whole system uses for that thing ("Passcode" -> "Password");
- save-status: routine saving announced ("Saving…", "Saved", "Saved { $file }",
  "Draft saved", "Gespeichert"). Saving is the system's job and says nothing;
  the framework's save status (`ui::SaveStatusSurface`, presented for every
  framework window) speaks only when a save stalls or fails (HIG §7, "Status
  has two homes"). The same rule holds for string literals in production Rust
  sources, where copy that bypasses the catalogue would slip past it.

A message or literal that has to say it anyway (the save status's own
one-time "Saved" after a failure) is listed in `copy_save_exceptions.toml`
with the reason it is not routine; an entry that no longer matches fails.

Catalogues that still carry old copy list it, per message and rule, in
`copy_language_baseline/<unit>.txt` (the unit is the crate path with `/`
replaced by `-`). A shard only shrinks: a listed message that no longer breaks
its rule fails the check until the line is deleted, and a new message that
breaks a rule fails outright.
"""

from __future__ import annotations

import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_source  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
GLOSSARY = HERE / "copy_glossary.toml"
SAVE_EXCEPTIONS = HERE / "copy_save_exceptions.toml"
BASELINE = HERE / "copy_language_baseline"

SOURCE_LOCALE = "en-US"

# A separator glyph standing between two values, and unicode symbols used as
# icons. The same sets as tools/policy/check_primitives.py.
GLYPH_JOIN = re.compile(r"\S\s+[·•|—–›»]\s+\S|\S\s/\s\S|^\s*[·•]\s|\s[·•]\s*$")
GLYPH_ICON = re.compile("[←→↑↓↖↗↘↙↔↕⇐⇒⇑⇓✓✔✗✘✕✖●○◉◎★☆▶◀▲▼▸◂▴▾►◄⬤⏵⏴⏶⏷]")

# Product and app names are matched case-sensitively; ordinary phrases are not.
NARRATION = (
    re.compile(r"\bTilo (?:checks|keeps|will|uses|creates|downloads|verifies|"
               r"signs|rechecks|is checking|kept|could not)\b"),
    re.compile(r"\b(?:Settings|Messages|Mail|Files|Store) "
               r"(?:will|checks|keeps|starts only)\b"),
    re.compile(r"\b(?:appears?|show up|shows?) here\b", re.IGNORECASE),
    re.compile(r"\bHere you can\b", re.IGNORECASE),
    re.compile(r"\bThis (?:panel|page|screen|section) (?:shows|lets)\b", re.IGNORECASE),
)

IMPLEMENTATION = {
    "en": (
        re.compile(r"\bcompositor\b", re.IGNORECASE),
        re.compile(r"\b(?:workspace )?document did not\b", re.IGNORECASE),
        re.compile(r"\b(?:identity-)?certified\b", re.IGNORECASE),
        re.compile(r"\b(?:was|were|is|are|not) admitted\b", re.IGNORECASE),
        re.compile(r"\bjobs? (?:\d|\{)", re.IGNORECASE),
        re.compile(r"\bsurface (?:\d|\{)", re.IGNORECASE),
        re.compile(r"\bCRDT\b"),
    ),
    "de": (
        re.compile(r"\bCompositor\b", re.IGNORECASE),
        re.compile(r"\bArbeitsbereichsdokument\b", re.IGNORECASE),
        re.compile(r"\b(?:identitäts-)?zertifiziert\w*\b", re.IGNORECASE),
        re.compile(r"\b(?:wurde|wurden|ist|sind|nicht) zugelassen\b", re.IGNORECASE),
        re.compile(r"\b(?:Auftrag|Job) (?:\d|\{)", re.IGNORECASE),
        re.compile(r"\bSurface (?:\d|\{)", re.IGNORECASE),
        re.compile(r"\bCRDT\b"),
    ),
}

# Placeables that only ever hold machine identifiers.
RAW_ID = re.compile(
    r"\{\s*\$(?:surface|surface_id|surface-id|job|job_id|job-id|invocation|"
    r"invocation_id|pid|uuid|app_id|app-id|package_id|package-id|peer_id|"
    r"peer-id|broker_id|object_id|node_id|device_id|device-id)\s*\}"
)

INSTRUCTION = {
    # "Tap to click" is the name of a touchpad setting, not an instruction.
    "en": re.compile(r"\b(?:select|click|tap|choose) (?:it |here |this )?to\b(?! click\b)",
                     re.IGNORECASE),
    "de": re.compile(r"\b(?:auswählen|anklicken|klicken|antippen),? um\b", re.IGNORECASE),
}

VAGUE_BUTTON = {
    "en": re.compile(r"(?:Continue|OK|Okay|Yes|Proceed|Submit|Confirm)\Z"),
    "de": re.compile(r"(?:Weiter|Fortfahren|OK|Okay|Ja|Bestätigen|Absenden)\Z"),
}
# Message ids that label a confirming button of a sheet or dialog.
BUTTON_ID = re.compile(r"(?:^|-)(?:confirm|accept|proceed|primary|ok)(?:-button|-label)?$")

# A whole visible line that only reports routine saving: the state ("Saving…",
# "Saved."), the thing saved by name ("Saved …", "… saved", "Draft saved") or
# its changes ("Saving latest changes…"). Placeables read as "…" here. A save
# with an outcome the person cannot see otherwise names it in more words
# ("Saved a copy as …", "Exported …") and does not match.
_SAVED_THINGS_EN = r"(?:(?:all |your |the )?(?:latest )?(?:changes?|edits?|drafts?|work|…) )"
ROUTINE_SAVE = {
    "en": re.compile(
        rf"{_SAVED_THINGS_EN}?(?:saved|saving)"
        r"(?: (?:(?:your |the )?(?:latest )?(?:changes|draft|direction)|…))?"
        r"(?:\s*[—–]\s.*)?\s*[.…!]*",
        re.IGNORECASE),
    "de": re.compile(
        r"(?:(?:alle |deine |die )?(?:neueste |letzte )?(?:änderungen|entwurf|entwürfe|…) )?"
        r"(?:gespeichert|(?:wird|werden) gespeichert)"
        r"(?:\s*[—–]\s.*)?\s*[.…!]*",
        re.IGNORECASE),
}


def routine_save(lang: str, line: str) -> bool:
    pattern = ROUTINE_SAVE.get(lang)
    return bool(pattern and pattern.fullmatch(line.strip()))


# Upper-case tokens that are names, not shouting.
ACRONYMS = frozenset("""
    AC AI API AV BIOS CA CPU CSV DAV DNS DPI EFI ESC EU FAQ GB GGUF GIF GPS GPU GTK
    HD HDMI HDR HEIC HTML HTTP HTTPS ICS ID IDE IMAP IP IPP ISO JPEG JPG JSON KB
    LAN LED LTE MAC MB MFA MIME MMS MP MTU NAS NFC NTP OCR OK OS OTP PC PDF PIN
    PNG PPP PPPOE PUK QR RAM RAW RSS RTF SD SDK SIM SMB SMS SMTP SSD SSH SSID SSO
    SVG TB TCP TLS TOTP TPM TV UDP UI URI URL USB UTC UUID VCF VNC VPN WAN WEBP
    WPA WPA2 WPA3 WEP XML ZIP CC BCC DE EN IBAN BIC ATM ISBN UPS ETA FM AM TOML
    YAML XWAYLAND KDE GNOME DVD CD RGB HSL HEX DRM CUPS LDAP NTFS FAT EXT HFS APFS
    BTRFS LUKS GPT MBR UEFI KVM VM IO HID SPI ACPI AES RSA ECC GPG PGP MLS NIST
    FIDO HFP A2DP SBC AAC LDAC IMEI APN VOLTE ESIM CSD ATTR TODO BCD MSRV
    WLAN HEVC OPML RAID ANSI ISO QWERTY QWERTZ AZERTY STARTTLS IANA GET POST PUT
    PATCH DELETE HEAD OPTIONS HOME PATH XDG_STATE_HOME FCAST RAW AVIF FLAC OGG
    WEBM MKV MOV AVI MPEG WAV OPUS H264 VP9 AV1 VAAPI NVENC IPV4 IPV6 DHCP IMAPS
    POP3 CALDAV CARDDAV WEBDAV OAUTH JMAP EWS ASCII UTF CJK RTL LTR FOMO GGUF
""".split())

ALL_CAPS_WORD = re.compile(r"(?<![\w$-])[A-ZÄÖÜ][A-ZÄÖÜ0-9_]+(?![\w-])")

# Search keywords are matched against queries and never shown.
UNSHOWN_KEY = re.compile(r"(?:^|-)kw-|(?:^|-)keywords?$")

PLACEABLE = re.compile(r"\{[^{}]*\}")
ENTRY = re.compile(r"(-?[A-Za-z][A-Za-z0-9_-]*)\s*=(.*)")
ATTRIBUTE = re.compile(r"\s+\.([A-Za-z][A-Za-z0-9_-]*)\s*=(.*)")
VARIANT = re.compile(r"\s*\*?\[[^\]]+\]\s*(.*)")


@dataclass(frozen=True)
class Message:
    key: str
    value: str
    line: int


def messages(text: str) -> list[Message]:
    """Messages, terms and attributes with their whole value."""
    found: list[Message] = []
    current: Message | None = None
    base = ""
    for number, line in enumerate(text.splitlines(), start=1):
        entry = ENTRY.fullmatch(line)
        attribute = ATTRIBUTE.fullmatch(line) if current or base else None
        if entry and not line[:1].isspace():
            if current:
                found.append(current)
            base = entry.group(1)
            current = Message(base, entry.group(2).strip(), number)
        elif attribute and base:
            if current:
                found.append(current)
            current = Message(f"{base}.{attribute.group(1)}",
                              attribute.group(2).strip(), number)
        elif current and line[:1] in (" ", "\t", "}"):
            variant = VARIANT.fullmatch(line)
            piece = variant.group(1) if variant else line.strip()
            current = Message(current.key, f"{current.value}\n{piece}", current.line)
        else:
            if current:
                found.append(current)
            current = None
            base = ""
    if current:
        found.append(current)
    return found


def visible_lines(value: str) -> list[str]:
    """The text a person reads: each select variant on its own line, with
    selector syntax and placeables (other than literals) blanked out."""
    lines = []
    for line in value.split("\n"):
        line = re.sub(r"\{\s*\$[A-Za-z0-9_-]+\s*->", " ", line)
        line = re.sub(r'\{\s*"([^"]*)"\s*\}', r"\1", line)
        line = PLACEABLE.sub("…", line)
        line = line.strip()
        if line and line != "}":
            lines.append(line)
    return lines


def language(locale: str) -> str:
    return locale.split("-")[0]


@dataclass(frozen=True)
class Term:
    language: str
    use: str
    avoid: re.Pattern[str]
    keys: re.Pattern[str] | None


def load_glossary(path: Path) -> list[Term]:
    """The glossary's terms, one per language each entry names."""
    data = tomllib.loads(path.read_text()) if path.exists() else {}
    terms = []
    for entry in data.get("term", []):
        if not entry.get("reason", "").strip():
            raise ValueError(f"{path.name}: a term has no reason: {entry}")
        for lang, spec in entry.items():
            if not isinstance(spec, dict):
                continue
            words = "|".join(re.escape(word) for word in spec["avoid"])
            flags = 0 if spec.get("case_sensitive") else re.IGNORECASE
            terms.append(Term(
                lang,
                spec["use"],
                re.compile(rf"(?<![\w-])(?:{words})(?![\w-])", flags),
                re.compile(spec["keys"]) if spec.get("keys") else None,
            ))
    return terms


def misused_term(terms: list[Term], lang: str, key: str, text: str) -> str | None:
    for term in terms:
        if term.language != lang or (term.keys and not term.keys.search(key)):
            continue
        match = term.avoid.search(text)
        if match:
            return f"says \"{match.group(0)}\" for what the glossary calls \"{term.use}\""
    return None


def all_caps(line: str) -> str | None:
    # Tokens with digits or underscores are identifiers (BLAKE3, FP32, an
    # environment variable), never shouting.
    words = [w for w in ALL_CAPS_WORD.findall(line)
             if w not in ACRONYMS and not re.search(r"[0-9_]", w)]
    if any(len(word) >= 4 for word in words):
        return next(word for word in words if len(word) >= 4)
    return None


def violations(locale: str, message: Message, glossary: list[Term]) -> list[tuple[str, str]]:
    """(rule, detail) for each rule `message` breaks."""
    lang = language(locale)
    found: list[tuple[str, str]] = []
    if UNSHOWN_KEY.search(message.key):
        return found
    raw = message.value
    lines = visible_lines(raw)
    text = "\n".join(lines)
    if lang == "en":
        for pattern in NARRATION:
            match = pattern.search(text)
            if match:
                found.append(("narration", f"narrates the software (\"{match.group(0)}\")"))
                break
    for pattern in IMPLEMENTATION.get(lang, ()):
        match = pattern.search(raw.replace("\n", " "))
        if match:
            found.append(("implementation",
                          f"speaks implementation (\"{match.group(0)}\")"))
            break
    match = RAW_ID.search(raw)
    if match:
        found.append(("raw-id", f"shows a machine id ({match.group(0)})"))
    for line in lines:
        word = all_caps(line)
        if word:
            found.append(("all-caps", f"shouts (\"{word}\"); use sentence case"))
            break
    if any(line.count(" · ") > 1 for line in lines):
        found.append(("chain", "chains more than two parts with \" · \""))
    shown = [re.sub(r"\{[^{}]*\}", "x", line) for line in lines]
    if any(GLYPH_JOIN.search(line) or GLYPH_ICON.search(line) for line in shown):
        found.append(("glyph", "joins facts with a separator glyph or uses a symbol as an "
                      "icon; lay facts out as separate components (ui::widget::Facts, "
                      "a secondary line, key hints) and take icons from ui::icons"))
    instruction = INSTRUCTION.get(lang)
    if instruction:
        match = instruction.search(text)
        if match:
            found.append(("instruction",
                          f"tells people how to operate a control (\"{match.group(0)}\"); "
                          "name the control by what it does"))
    vague = VAGUE_BUTTON.get(lang)
    if vague and BUTTON_ID.search(message.key) and vague.fullmatch(raw.strip()):
        found.append(("vague-button",
                      f"names a button \"{raw.strip()}\"; name it by its outcome"))
    misuse = misused_term(glossary, lang, message.key, text)
    if misuse:
        found.append(("glossary", misuse))
    saved = next((line for line in lines if routine_save(lang, line)), None)
    if saved:
        found.append(("save-status",
                      f"announces routine saving (\"{saved}\"); saving says nothing "
                      "unless it stalls or fails (ui::SaveStatusSurface)"))
    return found


@dataclass(frozen=True)
class SaveException:
    """A message (`unit` + `key`) or a production string literal (`file` +
    `text`) allowed to say "Saved"/"Saving", and why."""
    unit: str | None
    key: str | None
    file: str | None
    text: str | None


def load_save_exceptions(path: Path) -> set[SaveException]:
    data = tomllib.loads(path.read_text()) if path.exists() else {}
    allowed = set()
    for entry in data.get("allow", []):
        if not entry.get("reason", "").strip():
            raise ValueError(f"{path.name}: an exception has no reason: {entry}")
        exception = SaveException(entry.get("unit"), entry.get("key"),
                                  entry.get("file"), entry.get("text"))
        if not ((exception.unit and exception.key) or (exception.file and exception.text)):
            raise ValueError(f"{path.name}: name `unit` and `key`, or `file` and `text`: {entry}")
        allowed.add(exception)
    return allowed


def rust_sources(root: Path) -> list[Path]:
    """Production Rust sources of apps and crates."""
    found = []
    for top in ("apps", "crates"):
        base = root / top
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            relative = path.relative_to(root)
            if ("target" in relative.parts or "src" not in relative.parts
                    or any(part.startswith(".") for part in relative.parts)
                    or rust_source.is_test_file(relative)):
                continue
            found.append(path)
    return sorted(found)


def routine_save_literals(root: Path) -> list[tuple[str, int, str]]:
    """(file, line, text) of each production string literal that announces
    routine saving in English."""
    found = []
    for path in rust_sources(root):
        text = path.read_text(errors="replace")
        if "Sav" not in text and "sav" not in text:
            continue
        code, literals = rust_source.lex(text)
        # Mark each literal so blanking test items shows which it took.
        marked = list(code)
        for literal in literals:
            for index in range(literal.start, literal.end):
                if marked[index] != "\n":
                    marked[index] = "x"
        kept = rust_source.blank_test_items("".join(marked))
        for literal in literals:
            if literal.byte or kept[literal.start] != "x":
                continue
            value = re.sub(r"\{[^{}]*\}", "…", literal.value.replace("\\u{2026}", "…"))
            # Lower-case literals are identifiers ("saved" as a state's id).
            if value[:1].isupper() and routine_save("en", value):
                found.append((path.relative_to(root).as_posix(),
                              rust_source.line_number(text, literal.start), literal.value))
    return found


def catalogues(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("locales/*.ftl")
                  if "target" not in path.relative_to(root).parts
                  and not any(part.startswith(".") for part in path.relative_to(root).parts))


def unit_of(root: Path, catalogue: Path) -> str:
    return catalogue.parents[1].relative_to(root).as_posix().replace("/", "-")


def load_baseline(directory: Path) -> dict[str, set[tuple[str, str, str]]]:
    """Per unit, the (locale, message, rule) entries still allowed."""
    shards: dict[str, set[tuple[str, str, str]]] = {}
    if not directory.is_dir():
        return shards
    for shard in sorted(directory.glob("*.txt")):
        entries = set()
        for line in shard.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            locale, key, rule = line.split()
            entries.add((locale, key, rule))
        shards[shard.stem] = entries
    return shards


def scan(root: Path, glossary: list[Term]) -> dict[str, list[tuple[str, Message, str, str, Path]]]:
    """Per unit, every (locale, message, rule, detail, catalogue) violation."""
    found: dict[str, list[tuple[str, Message, str, str, Path]]] = {}
    for catalogue in catalogues(root):
        locale = catalogue.stem
        unit = unit_of(root, catalogue)
        for message in messages(catalogue.read_text()):
            for rule, detail in violations(locale, message, glossary):
                found.setdefault(unit, []).append((locale, message, rule, detail, catalogue))
    return found


def named(root: Path, path: Path) -> str:
    """How a finding names `path`: from the top of the tree when it is in it."""
    return path.relative_to(root).as_posix() if path.is_relative_to(root) else path.name


def check(root: Path, glossary_path: Path, baseline_dir: Path,
          save_exceptions_path: Path | None = None) -> list[str]:
    glossary = load_glossary(glossary_path)
    baseline = load_baseline(baseline_dir)
    exceptions = load_save_exceptions(save_exceptions_path) if save_exceptions_path else set()
    used: set[SaveException] = set()
    errors = []
    found = scan(root, glossary)
    for unit in sorted(set(found) | set(baseline)):
        allowed = baseline.get(unit, set())
        seen = set()
        for locale, message, rule, detail, catalogue in found.get(unit, []):
            entry = (locale, message.key.split(".")[0], rule)
            if entry in allowed:
                seen.add(entry)
                continue
            if rule == "save-status":
                exception = SaveException(unit, message.key.split(".")[0], None, None)
                if exception in exceptions:
                    used.add(exception)
                    continue
            errors.append(f"{named(root, catalogue)}:{message.line}: {message.key} {detail} [{rule}]")
        for locale, key, rule in sorted(allowed - seen):
            errors.append(f"{named(root, baseline_dir)}/{unit}.txt: {locale} {key} {rule} "
                          "no longer applies; delete the line")
    for file, line, text in routine_save_literals(root):
        exception = SaveException(None, None, file, text)
        if exception in exceptions:
            used.add(exception)
            continue
        errors.append(f"{file}:{line}: \"{text}\" announces routine saving; saving says "
                      "nothing unless it stalls or fails (ui::SaveStatusSurface) [save-status]")
    name = named(root, save_exceptions_path) if save_exceptions_path else "save exceptions"
    for exception in sorted(exceptions - used, key=repr):
        errors.append(f"{name}: {exception} no longer matches anything; delete it")
    return errors


def main(argv: list[str]) -> int:
    if argv[1:] == ["--print-baseline"]:
        glossary = load_glossary(GLOSSARY)
        for unit, entries in sorted(scan(ROOT, glossary).items()):
            for locale, message, rule, _, _ in entries:
                print(unit, locale, message.key.split(".")[0], rule)
        return 0
    errors = check(ROOT, GLOSSARY, BASELINE, SAVE_EXCEPTIONS)
    if errors:
        print("\n".join(errors))
        return 1
    print("copy language OK (state, outcome or next step, in the glossary's words)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
