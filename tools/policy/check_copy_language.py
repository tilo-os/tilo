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

A crate that ships a translation paints what a person reads through its
catalogue: an English literal handed to a copy builder there ships the source
language inside a translated window, so it fails unless
`copy_source_literals.toml` says why no catalogue has to carry it (what an
agent is told a tool does, a key's name, a capture label). A label is painted
as much when a closure or a conditional picks the literal
(`map_or_else(|| "Search", …)`, `if open { "Shown" } else { "Hidden" }`) as
when the builder is handed it directly, so both count.

A surface — an app's window or a session role's — paints through the toolkit's
text constructors (`Text::new`, `Button::new`, the `with_*`/`set_*` label and
accessible-name setters, the paint helpers that take the scene first, a
property list's own label, a section's title), not only through the builders
above. A literal handed to one of those is the same defect on a German desktop,
so surfaces are checked for them too. A name a person reads is as often
selected as it is written: a literal in the result position of a `match` arm or
an `if` branch is painted when the selection reaches a constructor, whether
through a tuple pattern (`let (role, name, state) = match …`) or a local.

Two more ways a word reaches a window without the constructor coming first:

- through a helper — a function that hands its parameter to a text constructor
  (`fn button(&self, id: &str, label: &str) -> Button { Button::new(id,
  label) }`): a literal at the parameter's position at any call site of it is
  painted just the same, and so is one passed on through a second helper;
- from a function that keeps the words rather than passing them on
  (`fn friendly_reason(reason: &str) -> &'static str { … "Add paper to
  continue." … }`): the value it returns is what is painted where the crate
  hands it on — a helper's painted argument, or a resource's or an action's
  words — and the words inside a branch or a match arm count as the value. A
  name the crate defines twice with different jobs (`line` as a painted row and
  as a page outline) says nothing about the word it is handed, so only a name
  whose every definition hands copy back counts, and a value that is a name the
  model answers with (`CameraApp`, `demo.thing`) is not copy. A constructor
  handed the call directly (`Text::new(kind_label(kind))`) is the same defect
  as one handed a call through a helper: the words the function returns are
  what the constructor paints. A name the keyboard prints (`Backspace`) is not
  words a person reads in their language, so it is listed in
  `copy_source_literals.toml` like any other literal no catalogue has to
  carry.
- as a struct field named like copy, where a `let` binds the struct and a copy
  builder reads the field back (`let row = Row { title: "Open settings", … };
  Text::new(row.title)`), or where the struct's own type paints the field
  through `self` (`HudItem { name: "Resolution", … }` written straight into a
  `vec!` a drawing method is handed, with
  `ButtonFace::icon_only(rect, icon, self.name)` in the type's `impl`). A
  fixture's sample data is bound to nothing a constructor reads and its type
  paints no field, and a field that only holds a person's own content is never
  read that way, so none of them counts;
- as a resource's or an action's title (`ResourceSpec::new(…, "History
  paused")`, `.unavailable(…, "Open a web page first")`): the action directory
  and the palette show those as the rows a person reads before invoking
  anything.
A sentence built around a value is copy as much as a plain one: a `format!`
whose template reads as English and whose result reaches one of those calls —
as an argument, or through the local it is stored in — fails, because a German
window needs the catalogue's own sentence with the value in it (Fluent
variables and plural branches). A catalogued message's variables are the
person's data — a name, a count, a filesystem's name — never a word the app
wrote itself: a literal interpolated into a message (`ArgValue::Text("Error")`,
`("state", "Shown".into())`) reads in English inside every language's sentence,
so the app resolves that word from the catalogue first. A word reaches a call
the same way through a wrapper or a conversion (`Some("Move")`,
`"Move".to_string()`, `"Move".into()`), through a default
(`origin.unwrap_or("No secure page")`) and through a local
(`let label = "Waiting for the first screen update";`), and an action's own
label is what its menu row and the action directory show, so a literal assigned
to `.type_mut().label` counts too. The test is syntactic, on purpose: a
sentence that travels through a chain of app helpers is a migration to do, not
a checker to build.

A window's bar draws what an app declares for it, so an app's slot words reach
the bar through the bar's own tables rather than through a text constructor: an
action's words are the `name` of a `ToolbarAction` (or of an entry the crate's
`BarCommands` makes, `bar.entry(icon, name, command)`), a notice's are a
`ToolbarNotice`'s `text`, and a place, a level of it or a view is a `label`. The
check reads those fields by their type, so a `name` another type carries, a
`ToolbarAction`'s `id` and a location's `value` (the address a person typed)
stay out of it.

A widget hands a reader many names at once, so it projects them as a tuple and
names each node from one element of it (`let (target, name, role, state) =
self.a11y_control(action)?`, then `.with_name(name)`). That element is words a
person hears: the tuple the projecting function returns, and the arms of the
`match` its own `let` picks the tuple from, are checked there — a literal and a
`format!` sentence alike.

A headless participant has no window of its own: it says what it does to the
person through a task run's update, which the task's own surface shows as it
is. A crate that ships a translation and hands that update an English literal
— or an English `format!` sentence, or a local holding one — ships the source
language into a German desktop's task as surely as a button would, so the
update's message is checked like any other copy.

The toolkit's own crates are outside that sweep: a widget's demo and example
code paints sample content, not a window's copy.
"""

from __future__ import annotations

import bisect
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
SOURCE_LITERALS = HERE / "copy_source_literals.toml"
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
    WLAN HEVC OPML RAID ANSI ISO EPUB SMART QWERTY QWERTZ AZERTY STARTTLS IANA GET POST PUT
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


COPY_BUILDERS = (
    "Notification::new(",
    ".with_name(",
    ".with_label(",
    ".with_tooltip(",
    ".with_description(",
    ".with_title(",
    ".with_body(",
    ".with_placeholder(",
    ".with_hint(",
    ".with_subtitle(",
    "push_error_notification(",
    "push_notification(",
    ".draw_text(",
    ".draw_text_weighted(",
    "draw_task_button(",
    "draw_more_row(",
    ".set_placeholder(",
    ".set_title(",
    ".set_text(",
    ".set_label(",
    ".set_body(",
)


# What a surface paints its words through: the call or setter, and which of its
# arguments carries them (0-based). `draw_text` and `draw_style` take the scene
# first, so the words are the second argument. A value interpolated into a
# catalogued message (`ArgValue::Text`) is painted inside the sentence around
# it, so it is held to the same rule as a label.
TEXT_CONSTRUCTORS = (
    ("Property::new(", 0),
    ("Section::titled(", 0),
    ("SectionHeader::heading(", 0),
    ("SectionHeader::group(", 0),
    ("Text::new(", 0),
    ("ArgValue::Text(", 0),
    ("Notification::new(", 0),
    ("Button::new(", 1),
    # A sheet is titled by the words a person reads above it, so a literal
    # handed to `ModalSpec::new` is the same defect as one handed a label. The
    # type qualifies the name: `new` alone is every other type's constructor,
    # whose argument may be a measurement string or a model id.
    ("ModalSpec::new(", 0),
    ("Button::icon_only(", 2),
    ("ListRow::new(", 1),
    ("ModalAction::primary(", 0),
    ("ModalAction::secondary(", 0),
    ("ModalAction::danger(", 0),
    ("ModalAction::alternate(", 0),
    ("ModalAction::alternate_danger(", 0),
    (".with_name(", 0),
    (".with_label(", 0),
    (".with_title(", 0),
    (".with_description(", 0),
    (".with_tooltip(", 0),
    (".with_placeholder(", 0),
    (".with_hint(", 0),
    (".with_subtitle(", 0),
    (".with_body(", 0),
    (".with_message(", 0),
    (".with_text(", 0),
    (".with_caption(", 0),
    (".with_spoken(", 0),
    (".with_divider_label(", 0),
    (".with_tail_title(", 0),
    (".with_primary_label(", 0),
    (".with_accept_label(", 0),
    (".with_cancel_label(", 0),
    (".with_empty_text(", 0),
    (".with_empty_hint(", 0),
    (".with_accessible_name(", 0),
    (".with_resolved_accessible_name(", 0),
    (".set_name(", 0),
    (".set_label(", 0),
    (".set_title(", 0),
    (".set_text(", 0),
    (".set_placeholder(", 0),
    (".set_body(", 0),
    (".set_description(", 0),
    (".set_caption(", 0),
    (".set_hint(", 0),
    (".set_tooltip(", 0),
    (".set_primary_label(", 0),
    (".set_empty_text(", 0),
    (".set_empty_hint(", 0),
    (".set_accessible_name(", 0),
    # The window's bar draws what an app declares for it, so an action's words
    # are its `name` and a notice's are its text: the calls that fill them are
    # text constructors like any other, qualified by their type.
    ("ToolbarAction::new(", 2),
    ("ToolbarNotice::new(", 0),
    (".label(", 0),
    (".title(", 0),
    (".heading(", 0),
    (".caption(", 0),
    (".secondary(", 0),
    (".message(", 0),
    (".text(", 0),
    (".draw_text(", 1),
    (".draw_text_weighted(", 1),
    (".draw_style(", 1),
    ("Segment::new(", 0),
    ("Stepper::new(", 0),
    ("Banner::error(", 0),
    ("Banner::warning(", 0),
    ("Banner::info(", 0),
)

# Where a surface's own words are painted: the roots whose crates draw a
# window a person opens.
SURFACE_ROOTS = ("apps/", "crates/session/")

# The fields of a struct that carry words a person reads (a row's title, a
# notice's detail), as opposed to a person's own content of the same name (a
# note's title).
COPY_FIELDS = (
    "title", "label", "subtitle", "tooltip", "description", "placeholder",
    "body", "message", "hint", "summary", "detail", "caption", "reason",
)


# What a run reports to the person whose task it is, in a message the task's
# own surface shows as it is: the words a headless participant has instead of
# a window. Its message is the first field of each.
WIRE_MESSAGE_BUILDERS = (
    "TaskRunUpdate::Progress {",
    "TaskRunUpdate::Succeeded {",
    "TaskRunUpdate::Failed {",
    "TaskRunUpdate::Cancelled {",
)

# What a surface titles a published resource or action with: the action
# directory and the palette show it as the row a person reads before invoking
# anything, and a reason an action is unavailable is a sentence they read.
SPEC_BUILDERS = (
    ("ResourceSpec::new(", 2),
    (".unavailable(", 1),
)

# What resolves a catalogued message with named values: the pairs of
# `("name", value)`, where the value is what the sentence reads out. It is the
# person's data — their name, a count, a filesystem's own name — never a word
# the app wrote itself, which would stand in English inside every language's
# sentence.
ARG_BUILDERS = ("t_args(", ".with_args(", "t_args_owned(")

BINDING = re.compile(
    r"(?m)^\s*(?:let\s+(?:mut\s+)?([a-z_][a-z0-9_]*)|let\s+\(([^)]*)\)"
    r"|[\w.()]+\.([a-z_][a-z0-9_]*))\s*"
    r"(?::[^=;\n]+)?=(?!=)"
)

# A literal a constructor receives can pass through a wrapper or a conversion on
# the way: `&"Move"`, `Some("Move")`, `String::from("Move")`, and then
# `"Move".to_string()`, `.into()`, `.as_str()`. It is painted just the same.
WRAPPER = re.compile(r"^\s*(?:&\s*)?(?:(?:Some|String::from|String::from_utf8_lossy|"
                     r"Cow::Borrowed|ToOwned::to_owned)\s*\(\s*)*$")
CONVERSION = re.compile(r"^(?:\s|\)|,|\.(?:into|to_string|to_owned|as_str|as_ref)\(\))*$")
# A call that hands back a default when there is nothing better, so the literal
# in it is the words the constructor gets, as a branch of an `if` is.
DEFAULT_CALLS = ("unwrap_or(", "unwrap_or_else(|", "unwrap_or_else(||",
                 ".or(", "or_else(|", "or_else(||")

# An action's label, assigned rather than set: it names the action in menus and
# in the action directory, so an English literal here is copy like any other.
ACTION_LABEL = re.compile(r"\.type_mut\(\)\.label\s*=(?!=)")


def english_words(value: str) -> bool:
    """A literal that reads as English words rather than an id or a key."""
    letters = [character for character in value]
    has_word = any(
        letters[index].isalpha() and letters[index + 1].isalpha()
        for index in range(len(letters) - 1)
    )
    if not has_word:
        return False
    spaced = any(
        letters[index].isalpha()
        and letters[index + 1].isalpha()
        and letters[index + 2] == " "
        and letters[index + 3].isalpha()
        for index in range(len(letters) - 3)
    )
    capitalised = len(letters) > 1 and letters[0].isupper() and letters[1].islower()
    return spaced or capitalised


def translated_units(root: Path) -> list[str]:
    """The crates that ship a language beyond their source catalogue."""
    units = set()
    for catalogue in catalogues(root):
        if catalogue.stem != SOURCE_LOCALE:
            units.add(catalogue.parents[1].relative_to(root).as_posix())
    return sorted(units)


def next_significant(code: str, offset: int) -> str:
    """The first character at or after `offset` that lexed code kept."""
    while offset < len(code) and code[offset].isspace():
        offset += 1
    return code[offset : offset + 1]


def arrow_before(code: str, offset: int) -> bool:
    """`code[:offset]` ends in `=>` and whitespace."""
    offset -= 1
    while offset >= 0 and code[offset].isspace():
        offset -= 1
    return offset >= 1 and code[offset - 1 : offset + 1] == "=>"


def wrapped(code: str, begin: int, start: int) -> bool:
    """`code[begin:start]` only wraps the literal: `&`, `Some(`, `String::from(`."""
    return bool(WRAPPER.match(code[begin:start]))


def converted(code: str, start: int, end: int) -> bool:
    """`code[start:end]` only converts the literal on its way out: `.into()`,
    `.to_string()`, `.as_str()`."""
    return bool(CONVERSION.match(code[start:end]))


def fallback(code: str, begin: int, start: int, end: int) -> bool:
    """The literal is a default the constructor gets when nothing better is
    there (`origin.unwrap_or("No secure page")`), the way a branch of an `if`
    is: `code[begin:start]` ends in that call and only its brackets follow."""
    if not converted(code, start, end):
        return False
    return code[begin:start].rstrip().endswith(DEFAULT_CALLS)


def painted_literals(code: str, starts: list[int],
                     by_start: dict[int, rust_source.Literal],
                     begin: int, end: int) -> list[rust_source.Literal]:
    """The literals a copy builder's first argument paints, in `[begin, end)`.

    The argument itself, and what a closure, a `match` arm or a conditional
    inside it hands back: `map_or_else(|| "Search", |named| …)`,
    `if open { "Shown" } else { "Hidden" }` and `match mode { A => "Back",
    … }` all reach the label as surely as the literal argument does, and so
    does a default (`origin.unwrap_or("No secure page")`). A literal that only
    feeds a call (`|word| say(word, "Search")`) is not painted, and does not
    count.
    """
    found = []
    at = bisect.bisect_left(starts, begin)
    while at < len(starts) and starts[at] < end:
        literal = by_start[starts[at]]
        after = next_significant(code, literal.end)
        if (not code[begin : literal.start].strip()
                or wrapped(code, begin, literal.start)
                or after == "}"
                or (arrow_before(code, literal.start) and after in ",}")
                or fallback(code, begin, literal.start, end)
                or (arm_result(code, begin, literal.start)
                    and closes_value(code, literal.end, end))):
            found.append(literal)
        at += 1
    return found


# A match arm's or a branch's `=>`, `|` or `{`, then the wrappers a value can
# sit in after it (`=> Some("Move")`).
ARM_OPEN = re.compile(
    r"(?:=>|\||\{)\s*(?:&\s*)?(?:(?:Some|String::from|String::from_utf8_lossy|"
    r"Cow::Borrowed|ToOwned::to_owned)\s*\(\s*)*$")
CONVERSION_CALL = re.compile(r"\s*\.(?:into|to_string|to_owned|as_str|as_ref)\(\)")


def arm_result(code: str, begin: int, start: int) -> bool:
    """`code[begin:start]` opens what a match arm or a branch hands back."""
    return ARM_OPEN.search(code[begin:start]) is not None


def closes_value(code: str, start: int, end: int) -> bool:
    """`code[start:end]` only converts or closes the literal: it is the value."""
    at = start
    while at < end:
        conversion = CONVERSION_CALL.match(code, at)
        if not conversion:
            break
        at = conversion.end()
    if at >= end:
        return True
    return next_significant(code, at) in ",})]"


def struct_field(code: str, begin: int, end: int, name: str) -> tuple[int, int]:
    """The value of field `name` in the struct literal whose body is
    `code[begin:end]`, or of the shorthand `name` binding; an empty range when
    it has neither."""
    position = begin
    segment = begin
    depth = 0
    while position <= end:
        character = code[position] if position < end else ","
        if character in "([{":
            depth += 1
        elif character in ")]}":
            depth -= 1
        elif character == "," and depth == 0:
            field = code[segment:position].strip()
            if field == name:
                return segment, position
            if field.startswith(f"{name}:"):
                return segment + code[segment:position].index(":") + 1, position
            segment = position + 1
        position += 1
    return 0, 0


def argument(code: str, start: int, index: int) -> tuple[int, int]:
    """The `index`th (0-based) argument of the call whose bracket opens at
    `start - 1`, or an empty range at the call's close when it has fewer
    arguments: a call that hands nothing there paints nothing."""
    end = rust_source.matching(code, start - 1) - 1
    begin = position = start
    seen = 0
    while position < end:
        if code[position] in "([{":
            position = rust_source.matching(code, position)
            continue
        if code[position] == "," and seen == index:
            return begin, position
        if code[position] == ",":
            seen += 1
            begin = position + 1
        position += 1
    return (begin, end) if seen == index else (end, end)


def surfaces(root: Path) -> list[str]:
    """The units that draw a window a person opens and ship a translation."""
    return [unit for unit in translated_units(root)
            if unit.startswith(SURFACE_ROOTS)]


def format_template(code: str, starts: list[int],
                    by_start: dict[int, rust_source.Literal],
                    begin: int, end: int) -> list[rust_source.Literal]:
    """The `format!` templates inside `[begin, end)`: the sentence a person
    reads once the values are in it."""
    found = []
    at = code.find("format!", begin, end)
    while at >= 0:
        open_paren = code.find("(", at, end)
        if open_paren >= 0:
            following = bisect.bisect_right(starts, open_paren)
            if following < len(starts):
                literal = by_start[starts[following]]
                if literal.start < end and not code[open_paren + 1 : literal.start].strip():
                    found.append(literal)
        at = code.find("format!", at + 1, end)
    return found


def statement_end(code: str, start: int) -> int:
    """Where the statement that starts at `start` ends: its `;` at this depth or
    the bracket that closes the enclosing block."""
    position = start
    while position < len(code):
        if code[position] in "([{":
            position = rust_source.matching(code, position)
            continue
        if code[position] in ";}" or code[position:position + 2] == "\n\n":
            return position
        position += 1
    return len(code)


def top_level_parts(code: str, begin: int, end: int) -> list[tuple[int, int]]:
    """The ranges between the top-level commas of `[begin, end)`."""
    parts = []
    start = position = begin
    while position < end:
        if code[position] in "([{":
            position = rust_source.matching(code, position)
            continue
        if code[position] == ",":
            parts.append((start, position))
            start = position + 1
        position += 1
    if start < end:
        parts.append((start, end))
    return parts


def top_level_find(code: str, begin: int, end: int, needle: str) -> int:
    """Where `needle` starts in `[begin, end)` outside any bracket, or -1."""
    position = begin
    while position < end:
        if code[position] in "([{":
            position = rust_source.matching(code, position)
            continue
        if code.startswith(needle, position):
            return position
        position += 1
    return -1


def argument_values(code: str, begin: int, end: int) -> list[tuple[int, int]]:
    """The `value` ranges of the `("name", value)` pairs a builder hands a
    catalogued message, inside its argument list `[begin, end)`."""
    found = []
    for part_begin, part_end in top_level_parts(code, begin, end):
        position = part_begin
        while position < part_end and (code[position].isspace() or code[position] == "&"):
            position += 1
        for prefix in ("vec!", "Vec::from", "Cow::Borrowed"):
            if code.startswith(prefix, position):
                position += len(prefix)
                while position < part_end and code[position].isspace():
                    position += 1
        if position >= part_end:
            continue
        if code[position] == "[":
            found.extend(argument_values(
                code, position + 1, rust_source.matching(code, position) - 1))
        elif code[position] == "(":
            close = rust_source.matching(code, position)
            comma = top_level_find(code, position + 1, close - 1, ",")
            if comma >= 0:
                found.append((comma + 1, close - 1))
    return found


def tuple_arms(code: str, begin: int, end: int,
               element: int) -> list[tuple[int, int]]:
    """Element `element` of each tuple the arms of the `match` in `[begin, end)`
    hand back: the names a tuple pattern binds are those elements."""
    if not code[begin:end].lstrip().startswith("match "):
        return []
    brace = code.find("{", begin, end)
    if brace < 0:
        return []
    close = rust_source.matching(code, brace) - 1
    found = []
    for part_begin, part_end in arm_parts(code, brace + 1, close):
        arrow = top_level_find(code, part_begin, part_end, "=>")
        if arrow < 0:
            continue
        found.extend(element_of_tuple(code, arrow + 2, part_end, element))
    return found


def arm_parts(code: str, begin: int, end: int) -> list[tuple[int, int]]:
    """The range of each arm of a `match` body `[begin, end)`: between the
    top-level commas, and after a block arm, whose `}` is its own end — an arm
    that is a block needs no comma, so splitting on commas alone merges it with
    the arm that follows."""
    parts = []
    start = position = begin
    depth = 0
    while position < end:
        character = code[position]
        if character in "([{":
            depth += 1
        elif character in ")]}":
            depth = max(depth - 1, 0)
            if not depth and character == "}":
                parts.append((start, position))
                start = position + 1
        elif character == "," and not depth:
            parts.append((start, position))
            start = position + 1
        position += 1
    if start < end:
        parts.append((start, end))
    return parts


def bound_names(binding: re.Match[str]) -> list[tuple[str, int | None]]:
    """The names a `let` binds, and for a tuple pattern each one's position."""
    if binding.group(1):
        return [(binding.group(1), None)]
    if binding.group(2) is not None:
        names = []
        for element, piece in enumerate(binding.group(2).split(",")):
            piece = piece.strip().removeprefix("mut ").strip()
            if re.fullmatch(r"[a-z_][A-Za-z0-9_]*", piece) and piece != "_":
                names.append((piece, element))
        return names
    return [(binding.group(3), None)] if binding.group(3) else []


def copy_bindings(code: str, starts: list[int], by_start: dict[int, rust_source.Literal],
                  kept: str) -> dict[str, list[tuple[tuple[int, int], rust_source.Literal]]]:
    """Words a constructor is handed through the name they are stored in, and
    the body the name is bound in.

    Only a value that is the sentence counts: a `format!`, a `match`/`if` that
    picks one (in a tuple pattern, the tuple's element at that name's
    position), or a plain literal. A sentence built inside a call — an error in
    `ok_or_else(|| format!(…))?` — is an outcome the surface shows in its own
    words, not a value a constructor gets handed.

    A local lives in the body that declares it: the same name in another
    function (`title` in one empty state, `title` in a sheet further down the
    file) holds its own words, and a constructor reading one does not read the
    other.
    """
    scopes = function_scopes(code)
    found: dict[str, list[tuple[tuple[int, int], rust_source.Literal]]] = {}
    for binding in BINDING.finditer(code):
        scope = next(((body, close) for body, close in scopes
                      if body <= binding.start() < close), (0, len(code)))
        begin = binding.end()
        end = statement_end(code, begin)
        value = code[begin:end].lstrip()
        for wrapper in ("&", "Some(", "Some ("):
            if value.startswith(wrapper):
                value = value[len(wrapper):].lstrip()
        selection = value.startswith(("match ", "if "))
        for name, element in bound_names(binding):
            ranges = ([(begin, end)] if element is None
                      else tuple_arms(code, begin, end, element))
            for range_begin, range_end in ranges:
                words = [literal for literal
                         in format_template(code, starts, by_start, range_begin, range_end)
                         if kept[literal.start] == "x" and english_words(literal.value)]
                if element is None:
                    stored = (stored_literal(code, starts, by_start, kept, range_begin,
                                             range_end)
                              if binding.group(1) else None)
                    if stored is not None and english_words(stored.value):
                        words.append(stored)
                if selection or element is not None:
                    words.extend(literal for literal
                                 in painted_literals(code, starts, by_start,
                                                     range_begin, range_end)
                                 if kept[literal.start] == "x"
                                 and english_words(literal.value))
                for literal in dict.fromkeys(words):
                    found.setdefault(name, []).append((scope, literal))
    return found


def stored_literal(code: str, starts: list[int],
                   by_start: dict[int, rust_source.Literal], kept: str,
                   begin: int, end: int) -> rust_source.Literal | None:
    """The literal a `let` stores as it stands in `[begin, end)`, or None.

    A local holds the words for as long as it lives (`let label = "Waiting for
    the first screen update";`), so a constructor handed the name paints them.
    A field assignment is not read this way: a struct's `title` is as often a
    person's own content as it is copy (`note.title = "Quiet hours"`).
    """
    at = bisect.bisect_left(starts, begin)
    if at >= len(starts):
        return None
    literal = by_start[starts[at]]
    if literal.start >= end or kept[literal.start] != "x":
        return None
    if not wrapped(code, begin, literal.start) or not converted(code, literal.end, end):
        return None
    return literal


def named_position(code: str, begin: int, end: int, name: str) -> int:
    """Where `name` is used as a whole identifier inside `[begin, end)`, or -1."""
    at = code.find(name, begin, end)
    while at >= 0:
        before = code[at - 1 : at]
        after = code[at + len(name) : at + len(name) + 1]
        if not (before.isalnum() or before == "_" or after.isalnum() or after == "_"):
            return at
        at = code.find(name, at + len(name), end)
    return -1


def named_in(code: str, begin: int, end: int, name: str) -> bool:
    """Whether `name` is used as a whole identifier inside `[begin, end)`."""
    return named_position(code, begin, end, name) >= 0


def function_scopes(code: str) -> list[tuple[int, int]]:
    """The body of each of the file's functions, as `(brace, close)`."""
    scopes = []
    for function in FUNCTION.finditer(code):
        brace = code.find("{", function.end())
        if brace >= 0:
            scopes.append((brace, rust_source.matching(code, brace)))
    return scopes


FUNCTION = re.compile(r"\bfn\s+([a-z_][A-Za-z0-9_]*)\s*(?:<[^>]*>)?\s*\(")
# A call to a helper: a free function, a method on `self`, an associated call.
HELPER_CALL = r"(?<![\w$]){name}\s*\("
# An inherent or trait implementation, whose brace follows the type it is for:
# `impl ActionButton`, `impl<'a> Widget for Row<'a>`. What a method of it is
# called by is the type's own name, which tells `ActionButton::new` from the
# `new` every other type has.
IMPLEMENTED_TYPE = re.compile(r"\bimpl(?:\s*<[^>{;]*>)?\s+([^{;]*?)\s*\{")
# A constant or static holding a value: `const TITLE: &str = "Move";`,
# `const LABELS: [&str; 2] = ["Erase", "Keep"]`.
WORD_CONSTANT = re.compile(
    r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:const|static)\s+"
    r"([A-Z][A-Z0-9_]*)\s*(?::[^=]+)?=(?!=)"
)
# A catalogue lookup: what a window paints is the message `key` names, so a
# helper that hands its parameter here takes a key, not words. A call site
# filling that parameter with English words (`"Refresh disks"`) paints nothing
# the catalogue carries, so it ships the source language inside a German
# window.
KEY_LOOKUP = re.compile(
    r"(?<![\w$])(?:[A-Za-z_][A-Za-z0-9_]*\s*(?:::|\.)\s*)*(?:t|t_args)\s*\("
)


def parameters(code: str, start: int) -> list[str]:
    """A function's parameter names in order, from the `(` at `start - 1`; an
    empty string where a pattern has no plain name, to keep the positions."""
    end = rust_source.matching(code, start - 1) - 1
    pieces = []
    begin = position = start
    generic = 0
    while position < end:
        char = code[position]
        if char in "([{":
            position = rust_source.matching(code, position)
            continue
        if char == "<":
            generic += 1
        elif char == ">" and code[position - 1] != "-":
            generic = max(generic - 1, 0)
        elif char == "," and generic == 0:
            pieces.append(code[begin:position])
            begin = position + 1
        position += 1
    pieces.append(code[begin:end])
    names = []
    for piece in pieces:
        piece = piece.strip()
        head = piece.split(":", 1)[0] if ":" in piece else piece
        words = head.replace("&", " ").replace("mut ", " ").split()
        name = words[-1] if words else ""
        names.append(name if re.fullmatch(r"[a-z_][A-Za-z0-9_]*", name) else "")
    return names


def bare(argument_text: str, name: str) -> bool:
    """`argument_text` is `name` itself, only wrapped or converted on the way."""
    text = re.sub(r"^\s*(?:&\s*)?(?:(?:Some|String::from)\s*\(\s*)*", "", argument_text)
    text = re.sub(r"(?:\s*\))*\s*(?:\.(?:into|to_string|to_owned|as_str|as_ref)\(\))*\s*$",
                  "", text)
    return text == name


def helper_table(paths: list[Path]) -> dict[str, set[int]]:
    """Per function of one crate, the argument indexes its callers fill with
    copy: the parameter at that index reaches a text constructor, directly or
    through another helper of the same crate.

    A helper is a function a surface builds its words through
    (`fn button(&self, id: &str, label: &str) -> Button`). Apps grow the same
    helper in several modules (`fn button(&self, id, label, action)` in each of
    them); a name keeps its place only when every definition paints the same
    argument, because only then does a call site say which word is copy — a
    name the crate uses for two different jobs (`new`) says nothing. A test
    double's method paints nothing a person reads, so it decides no name.

    A parameter a body hands to a catalogue lookup (`t(key)`, `CATALOG.t(key)`,
    `i18n::t(key)`) is a key: the call site chooses which message is painted,
    so words there never reach the catalogue at all.
    """
    bodies: list[tuple[str, str, list[str], str, int, int]] = []
    for path in paths:
        text = path.read_text(errors="replace")
        if "fn " not in text:
            continue
        code, _ = rust_source.lex(text)
        shown = rust_source.blank_test_items(code)
        implementations = []
        for found in IMPLEMENTED_TYPE.finditer(code):
            held = found.group(1)
            if " for " in held:
                held = held.split(" for ", 1)[1]
            held = held.split("<", 1)[0].strip().rsplit("::", 1)[-1]
            if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", held):
                implementations.append((held, found.end(), rust_source.matching(code,
                                                                               found.end() - 1)))
        for function in FUNCTION.finditer(code):
            if shown[function.start()] == " ":
                continue
            names = parameters(code, function.end())
            # A receiver is not an argument at a call site.
            if names and names[0] == "self":
                names = names[1:]
            brace = code.find("{", function.end())
            if names and brace >= 0:
                owner = next(
                    (held for held, body, close in reversed(implementations)
                     if body <= function.start() < close),
                    "",
                )
                qualified = f"{owner}::{function.group(1)}" if owner else ""
                bodies.append((function.group(1), qualified, names, code, brace,
                               rust_source.matching(code, brace)))

    lands: dict[int, set[int]] = {}
    flows: list[tuple[int, int, str, int]] = []
    for number, (name, _, names, code, brace, body_end) in enumerate(bodies):
        painted_at: set[tuple[int, str]] = set()
        for call, index in TEXT_CONSTRUCTORS + SPEC_BUILDERS:
            at = code.find(call, brace, body_end)
            while at >= 0:
                begin, end = argument(code, at + len(call), index)
                if begin < end:
                    painted_at.add((begin, code[begin:end]))
                at = code.find(call, at + len(call), body_end)
        # What a function iterates and paints one element of, it paints whole:
        # `labels.iter().map(|label| Segment::new(*label))` draws every label it
        # is given, so the caller's argument is copy like any constructor's.
        painted_variables = {
            found.group(1)
            for _, source in painted_at
            if (found := re.fullmatch(r"\s*[*&]?\s*([a-z_][A-Za-z0-9_]*)\s*", source))
        }
        iterated = [
            parameter for parameter in names
            if parameter and parameter != "self" and painted_variables
            and re.search(rf"(?<![\w$]){re.escape(parameter)}\s*\.\s*"
                          r"(?:iter|into_iter|iter_mut)\s*\(", code[brace:body_end])
        ]
        keys_at: set[tuple[int, str]] = set()
        for lookup in KEY_LOOKUP.finditer(code, brace, body_end):
            begin, end = argument(code, lookup.end(), 0)
            if begin < end:
                keys_at.add((begin, code[begin:end]))
        passed_on: list[tuple[str, int, str]] = []
        for hand in re.finditer(r"[A-Za-z_][A-Za-z0-9_:]*\s*\(", code[brace:body_end]):
            callee = hand.group(0)[:-1].strip()
            open_paren = brace + hand.start() + len(hand.group(0)) - 1
            for called, (begin, end) in enumerate(calls_of(code, open_paren)):
                if begin < end:
                    passed_on.append((callee, called, code[begin:end]))
        for index, parameter in enumerate(names):
            if not parameter or parameter == "self":
                continue
            if any(bare(source, parameter) for _, source in painted_at):
                lands.setdefault(number, set()).add(index)
            if parameter in iterated:
                lands.setdefault(number, set()).add(index)
            if any(bare(source, parameter) for _, source in keys_at):
                lands.setdefault(number, set()).add(index)
            for callee, called, source in passed_on:
                if bare(source, parameter):
                    flows.append((number, index, callee, called))
    by_name: dict[str, set[int]] = {}
    by_type_name: dict[str, set[int]] = {}
    for number, (name, qualified, _, _, _, _) in enumerate(bodies):
        by_name.setdefault(name, set()).add(number)
        if qualified:
            by_type_name.setdefault(qualified, set()).add(number)

    def landings(numbers) -> set[int]:
        found: set[int] = set()
        for number in numbers:
            found |= lands.get(number, set())
        return found

    def painted_by(name: str) -> set[int]:
        """What callers share with a callee: the arguments every definition of
        a bare name paints, or the one definition of a type's method."""
        if "::" in name:
            typed = landings(by_type_name.get(name, ()))
            if typed:
                return typed
            name = name.rsplit("::", 1)[-1]
        return landings(by_name.get(name, ()))

    changed = True
    while changed:
        changed = False
        for number, index, callee, callee_index in flows:
            if callee_index in painted_by(callee) and index not in lands.get(number, set()):
                lands.setdefault(number, set()).add(index)
                changed = True
    table: dict[str, set[int]] = {}
    for name, numbers in by_name.items():
        sets = [lands.get(number, set()) for number in numbers]
        if sets[0] and all(other == sets[0] for other in sets):
            table[name] = sets[0]
    for qualified, numbers in by_type_name.items():
        shared = landings(numbers)
        if shared and all(lands.get(number, set()) == shared for number in numbers):
            table[qualified] = shared
    return table


def calls_of(code: str, open_paren: int):
    """The argument ranges of the call whose `(` is at `open_paren`."""
    close = rust_source.matching(code, open_paren) - 1
    yield from top_level_parts(code, open_paren + 1, close)


def prose_words(value: str) -> bool:
    """A value that reads as the words a person reads, rather than a name the
    model answers with for itself.

    `english_words` is tuned to what a builder is handed; a function's return
    value is as often its own name (`CameraApp`) or a machine word
    (`demo.thing`). A single token counts only when it is one capitalised word
    (`Inbox`, `Paused`, `Self-timer`); anything with a space is words.
    """
    if not english_words(value):
        return False
    word = value.strip()
    if re.fullmatch(r"[A-Za-z][A-Za-z0-9_.:-]*", word):
        return re.fullmatch(r"[A-Z][a-z]+(?:-[a-z]+)*", word) is not None
    return True


def return_ranges(code: str, brace: int, end: int) -> list[tuple[int, int]]:
    """Where the value of the function opening at `brace` is built: after each
    `return`, and the tail expression before the closing brace."""
    ranges = []
    for found in re.finditer(r"\breturn\b", code[brace:end]):
        begin = brace + found.end()
        ranges.append((begin, statement_end(code, begin)))
    at = end - 1
    while at > brace and code[at].isspace():
        at -= 1
    depth = 0
    position = at
    while position > brace:
        char = code[position]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if not depth:
                break
            depth -= 1
        elif char == ";" and not depth:
            break
        position -= 1
    ranges.append((position + 1, at + 1))
    return ranges


# A call that reports a refusal to the code that asked: the sentence in it is
# that caller's outcome, not a value the function hands back.
REFUSAL_CALL = re.compile(r"(?:ok_or_else|ok_or|map_err|unwrap_or_else|Err)\s*\(")


def inside_refusal(code: str, begin: int, start: int) -> bool:
    """Whether the literal at `start` is built inside a refusal in `[begin,
    start)`: `ok_or_else(|| "…".to_string())`, `Err("…".into())`."""
    for found in REFUSAL_CALL.finditer(code, begin, start):
        if rust_source.matching(code, found.end() - 1) > start:
            return True
    return False


def returned_words(code: str, starts: list[int],
                   by_start: dict[int, rust_source.Literal], kept: str,
                   brace: int, end: int) -> list[rust_source.Literal]:
    """The English words the function opening at `brace` hands back: its tail
    value and each `return`, with the branches, arms and sentences in them. A
    sentence built inside a refusal (`ok_or_else(|| "…")`) is the caller's
    outcome, which that caller shows in its own words."""
    found = []
    for begin, close in return_ranges(code, brace, end):
        if code[:begin].rstrip().endswith("return") and code[begin:close].lstrip().startswith(
                "Err("):
            continue
        for literal in painted_literals(code, starts, by_start, begin, close):
            if (kept[literal.start] == "x" and prose_words(literal.value)
                    and not inside_refusal(code, begin, literal.start)):
                found.append(literal)
        for literal in format_template(code, starts, by_start, begin, close):
            if (kept[literal.start] == "x" and prose_words(literal.value)
                    and not inside_refusal(code, begin, literal.start)):
                found.append(literal)
    return found


FILE_WORDS: dict[tuple[str, int, int], dict[str, list[tuple[str, int, str]]]] = {}


def file_returning_words(root: Path, path: Path) -> dict[str, list[tuple[str, int, str]]]:
    """Per function name of one file, the words it hands back, with where they
    are written. A file is read once however many crates build on it."""
    held = path.stat()
    key = (str(path), held.st_mtime_ns, held.st_size)
    if key in FILE_WORDS:
        return FILE_WORDS[key]
    table: dict[str, list[tuple[str, int, str]]] = {}
    text = path.read_text(errors="replace")
    if "fn " in text:
        code, literals = rust_source.lex(text)
        starts = sorted(literal.start for literal in literals if not literal.byte)
        by_start = {literal.start: literal for literal in literals if not literal.byte}
        marked = list(code)
        for literal in literals:
            for index in range(literal.start, literal.end):
                if marked[index] != "\n":
                    marked[index] = "x"
        kept = rust_source.blank_test_items("".join(marked))
        for function in FUNCTION.finditer(code):
            brace = code.find("{", function.end())
            if brace < 0 or kept[function.start()] == " ":
                continue
            words = returned_words(code, starts, by_start, kept, brace,
                                   rust_source.matching(code, brace))
            held = table.setdefault(function.group(1), [])
            held.extend(
                (path.relative_to(root).as_posix(),
                 rust_source.line_number(text, literal.start), literal.value)
                for literal in words
            )
    FILE_WORDS[key] = table
    return table


def returning_words(root: Path,
                    paths: list[Path]) -> dict[str, list[tuple[str, int, str]]]:
    """Per function name of one crate, the words it hands back, with where they
    are written.

    A name keeps its place only when every definition of it in these files
    hands copy back: a crate that uses one name for two jobs (`line` as a
    painted row and as a page's outline) says nothing about the word a call
    site gives it. What a function returns is judged by the value it builds,
    not by its signature: a reason behind a type alias (`-> Blocked`) or in a
    tuple reaches a painter the same as a bare `&str` does.
    """
    table: dict[str, list[tuple[str, int, str]]] = {}
    every: dict[str, bool] = {}
    for path in paths:
        for name, words in file_returning_words(root, path).items():
            every[name] = every.get(name, True) and bool(words)
            table.setdefault(name, []).extend(words)
    return {name: sorted(set(words)) for name, words in table.items() if every[name]}


def stored_fields(code: str, starts: list[int],
                  by_start: dict[int, rust_source.Literal],
                  kept: str) -> list[tuple[str, str, rust_source.Literal]]:
    """`(binding, field, literal)` for each copy-named field a struct literal
    stores words in, where a `let` binds the struct.

    Only a struct the code binds and can paint through (`let row = Row { title:
    "Open settings", … }`) counts; a fixture's sample data is bound to nothing
    a constructor reads.
    """
    found = []
    for binding in BINDING.finditer(code):
        if not binding.group(1):
            continue
        begin = binding.end()
        end = statement_end(code, begin)
        open_brace = code.find("{", begin, end)
        if open_brace < 0 or not re.fullmatch(
                r"\s*&?\s*[A-Za-z_][A-Za-z0-9_]*(?:\s*::\s*[A-Za-z_][A-Za-z0-9_]*)*\s*",
                code[begin:open_brace]):
            continue
        close = rust_source.matching(code, open_brace) - 1
        for part_begin, part_end in top_level_parts(code, open_brace + 1, close):
            piece = code[part_begin:part_end]
            colon = piece.find(":")
            if colon < 0:
                continue
            field_name = piece[:colon].strip()
            if field_name not in COPY_FIELDS:
                continue
            at = bisect.bisect_left(starts, part_begin + colon + 1)
            if at >= len(starts):
                continue
            literal = by_start[starts[at]]
            if literal.start >= part_end or kept[literal.start] != "x":
                continue
            if not wrapped(code, part_begin + colon + 1, literal.start):
                continue
            if not converted(code, literal.end, part_end):
                continue
            if prose_words(literal.value):
                found.append((binding.group(1), field_name, literal))
    return found


def read_at(code: str, ranges: list[tuple[int, int]], name: str, field: str) -> bool:
    """Whether `name.field` is read inside one of `ranges`."""
    pattern = re.compile(rf"(?<![\w$]){re.escape(name)}\s*\.\s*{field}\b")
    return any(pattern.search(code[begin:end]) for begin, end in ranges)


# What a type's own method hands a field to when it paints the struct: the
# toolkit's labelled button face takes its label as the second argument, the
# icon-only one its accessible name as the third.
FIELD_PAINTERS = TEXT_CONSTRUCTORS + (
    ("ButtonFace::new(", 1),
    ("ButtonFace::icon_only(", 2),
)

# The type an `impl` block is for: `impl<'a> HudItem<'a>`, `impl Trait for Row`.
IMPL = re.compile(r"\bimpl(?:<[^>{]*>)?\s+(?:[^;{}\n]*?\bfor\s+)?([A-Z][A-Za-z0-9_]*)")

# A struct literal: `HudItem {`, `Row::<T> {`.
STRUCT_LITERAL = re.compile(r"(?<![\w:])([A-Z][A-Za-z0-9_]*)\s*(?:::\s*<[^<>]*>\s*)?\{")

# The window's bar draws what an app declares for it, so an app's slot words
# travel through the bar's own types and reach no text constructor at all: an
# app names an action in a `ToolbarAction`'s `name`, says a notice in a
# `ToolbarNotice`'s `text`, and names a place, a view or a level in a
# `label`. Each is `(type, field)`, exactly: a `name` another type carries, a
# `ToolbarAction`'s `id`, a location's `value` (the whole address a person
# typed) or a page's `image` are not words.
WINDOW_BAR_LABELS = (
    ("ToolbarAction", "name"),
    ("ToolbarNotice", "text"),
    ("ToolbarLocation", "label"),
    ("ToolbarLocation", "name"),
    ("ToolbarLocation", "go_name"),
    ("ToolbarLocation", "placeholder"),
    ("LocationChoice", "label"),
    ("LocationSegment", "label"),
    ("ToolbarView", "label"),
    ("ToolbarViews", "name"),
)

# The crate's window bar's commands, as a field or a parameter holds them
# (`bar_entries: ui::framework::BarCommands<BarCommand>`,
# `bar: &BarCommands<BarAction>`) and as a local is bound from one
# (`let bar = &self.bar_entries;`). An entry of them —
# `bar.entry(icon, name, command)` — is a `ToolbarAction` at the name's
# position.
BAR_COMMANDS_DECLARED = re.compile(
    r"(?<![\w$])([a-z_][A-Za-z0-9_]*)\s*:\s*&?\s*"
    r"(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*BarCommands\s*<")
BAR_COMMANDS_LOCAL = re.compile(
    r"(?<![\w$])let\s+([a-z_][A-Za-z0-9_]*)\s*=\s*&\s*"
    r"(?:(?:self)\s*\.\s*)?([a-z_][A-Za-z0-9_]*)\s*;")
# Where an entry of them names its action: `bar.entry(icon, name, command)`,
# the words at the second argument.
BAR_ENTRY_CALL = r"(?<![\w$])(?:self\s*\.\s*)?{name}\s*\.\s*entry\s*\("

# What tells a file that it declares or edits the window's bar: its types (an
# app's import names the one it uses) and its commands' table.
BAR_MARKERS = tuple(sorted({kind for kind, _ in WINDOW_BAR_LABELS} | {"BarCommands"}))

# One of the bar's types built as a struct literal, by its own name however it
# is pathed (`ToolbarLocation {`, `ui::window_toolbar::ToolbarLocation {`).
BAR_STRUCT_LITERAL = re.compile(
    r"(?<![\w])(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*("
    + "|".join(kind for kind, _ in WINDOW_BAR_LABELS)
    + r")\s*(?:::\s*<[^<>]*>\s*)?\{")


def bar_command_names(paths: list[Path]) -> set[str]:
    """The identifiers one crate's window-bar commands are held in: the
    `BarCommands` a field or a parameter declares, and each local bound from
    one. A receiver of any other type is not the bar's table, so its `entry`
    (a map's, a menu's) says nothing about a word."""
    declared: set[str] = set()
    from_locals: list[tuple[str, str]] = []
    for path in paths:
        text = path.read_text(errors="replace")
        code = rust_source.lex(text)[0]
        declared.update(found.group(1) for found in BAR_COMMANDS_DECLARED.finditer(code))
        from_locals.extend((found.group(1), found.group(2))
                           for found in BAR_COMMANDS_LOCAL.finditer(code))
    names = set(declared)
    changed = True
    while changed:
        changed = False
        for local, source in from_locals:
            if source in names and local not in names:
                names.add(local)
                changed = True
    return names


def bar_label_ranges(code: str, names: set[str]) -> list[tuple[int, int]]:
    """Where `code` puts words the window's bar draws: the value of a
    `WINDOW_BAR_LABELS` field in a struct literal of that type, and the name
    an entry of the crate's bar commands is made with."""
    found = []
    for literal in BAR_STRUCT_LITERAL.finditer(code):
        open_brace = literal.end() - 1
        close = rust_source.matching(code, open_brace) - 1
        fields = {field for kind, field in WINDOW_BAR_LABELS if kind == literal.group(1)}
        if not fields:
            continue
        for part_begin, part_end in top_level_parts(code, open_brace + 1, close):
            piece = code[part_begin:part_end]
            colon = piece.find(":")
            if colon >= 0 and piece[:colon].strip() in fields:
                found.append((part_begin + colon + 1, part_end))
            elif piece.strip() in fields:
                # The field written short (`ToolbarLocation { label, … }`): the
                # words are in the local of that name.
                found.append((part_begin, part_end))
    for name in sorted(names):
        pattern = re.compile(BAR_ENTRY_CALL.format(name=re.escape(name)))
        for hand in pattern.finditer(code):
            begin, end = argument(code, hand.end(), 1)
            if begin < end:
                found.append((begin, end))
    return found


def painted_fields(code: str) -> set[tuple[str, str]]:
    """`(type, field)` for each struct field a type's own method paints
    (`ButtonFace::icon_only(rect, icon, self.name)`).

    A field read off `self` where the type draws itself is painted wherever
    the struct is built, however it is built.
    """
    found: set[tuple[str, str]] = set()
    for block in IMPL.finditer(code):
        brace = code.find("{", block.end())
        if brace < 0:
            continue
        end = rust_source.matching(code, brace)
        for call, index in FIELD_PAINTERS:
            at = code.find(call, brace, end)
            while at >= 0:
                begin, close = argument(code, at + len(call), index)
                if begin < close:
                    read = re.fullmatch(r"self\s*\.\s*([a-z_][a-z0-9_]*)",
                                        code[begin:close].strip())
                    if read:
                        found.add((block.group(1), read.group(1)))
                at = code.find(call, at + len(call), end)
    return found


def painted_literal_fields(code: str, starts: list[int],
                           by_start: dict[int, rust_source.Literal],
                           kept: str, painted: set[tuple[str, str]]
                           ) -> list[rust_source.Literal]:
    """The words an inline struct literal gives a field its own type paints.

    `HudItem { name: "Resolution", … }` written straight into a `vec!` a
    drawing function is handed says the words as surely as one a `let` binds,
    so the type's paint decides, not a local's name.
    """
    found = []
    for literal in STRUCT_LITERAL.finditer(code):
        open_brace = literal.end() - 1
        close = rust_source.matching(code, open_brace) - 1
        fields = {field for kind, field in painted if kind == literal.group(1)}
        if not fields:
            continue
        for part_begin, part_end in top_level_parts(code, open_brace + 1, close):
            piece = code[part_begin:part_end]
            colon = piece.find(":")
            if colon < 0 or piece[:colon].strip() not in fields:
                continue
            at = bisect.bisect_left(starts, part_begin + colon + 1)
            if at >= len(starts):
                continue
            word = by_start[starts[at]]
            if word.start >= part_end or kept[word.start] != "x":
                continue
            if not wrapped(code, part_begin + colon + 1, word.start):
                continue
            if not converted(code, word.end, part_end):
                continue
            if prose_words(word.value):
                found.append(word)
    return found


WORD_CONSTANT_TABLE_CACHE: dict[tuple[str, str], dict[str, list[tuple[str, int, str]]]] = {}


def word_constants(code: str, starts: list[int],
                   by_start: dict[int, rust_source.Literal],
                   kept: str) -> dict[str, list[rust_source.Literal]]:
    """Per constant and static, the English words it holds.

    A constant keeps its words out of every painting call site: `const LABELS:
    [&str; 2] = ["Erase disk", "Keep"]` reaches a label, a station caption or a
    reason through its name alone, so a German window would paint the source
    language from a place no constructor argument shows.
    """
    found: dict[str, list[rust_source.Literal]] = {}
    for match in WORD_CONSTANT.finditer(code):
        begin = match.end()
        end = statement_end(code, begin)
        held = [by_start[start] for start in starts
                if begin <= start < end and kept[start] == "x"]
        if not held or not all(english_words(literal.value) for literal in held):
            continue
        found[match.group(1)] = held
    return found


def word_constant_table(root: Path,
                        paths: list[Path]) -> dict[str, list[tuple[str, int, str]]]:
    """Per constant name of one crate, its words and where they are written."""
    table: dict[str, list[tuple[str, int, str]]] = {}
    for path in paths:
        text = path.read_text(errors="replace")
        if "const " not in text and "static " not in text:
            continue
        code, literals = rust_source.lex(text)
        starts = sorted(literal.start for literal in literals if not literal.byte)
        by_start = {literal.start: literal for literal in literals if not literal.byte}
        marked = list(code)
        for literal in literals:
            for index in range(literal.start, literal.end):
                if marked[index] != "\n":
                    marked[index] = "x"
        kept = rust_source.blank_test_items("".join(marked))
        for name, held in word_constants(code, starts, by_start, kept).items():
            table.setdefault(name, []).extend(
                (path.relative_to(root).as_posix(),
                 rust_source.line_number(text, literal.start), literal.value)
                for literal in held
            )
    return {name: sorted(set(words)) for name, words in table.items()}


# A helper of the crate that maps a word to the catalogue key for it
# (`fn text_size_ftl(english: &str) -> &'static str { match english { "Small" =>
# "settings-appearance-text-small", … } }`). What it is handed is a key.
def key_mappers(paths: list[Path], keys: set[str]) -> set[str]:
    """Per crate, the functions that turn a word into the catalogue key for it."""
    found: set[str] = set()
    for path in paths:
        text = path.read_text(errors="replace")
        if "fn " not in text or "match " not in text:
            continue
        code, literals = rust_source.lex(text)
        held = [literal for literal in literals if not literal.byte
                and literal.value in keys]
        if not held:
            continue
        for function in FUNCTION.finditer(code):
            brace = code.find("{", function.end())
            if brace < 0:
                continue
            close = rust_source.matching(code, brace)
            if any(brace < literal.start < close for literal in held):
                found.add(function.group(1))
    return found


def catalogue_keys(root: Path, crate: str) -> set[str]:
    """The message ids of one crate's source catalogue."""
    path = root / crate / f"locales/{SOURCE_LOCALE}.ftl"
    if not path.is_file():
        return set()
    return {message.key for message in messages(path.read_text(errors="replace"))}


def words_returned_in(code: str, begin: int, end: int,
                      returning: dict[str, list[tuple[str, int, str]]]
                      ) -> list[tuple[str, int, str]]:
    """The words a function of the crate hands back inside `[begin, end)`.

    The call is a constructor's, a resource's or an action's words, or a
    painted helper's argument: the value the function returns is what is
    painted there, in the file the function is written in.
    """
    found = []
    for name, words in returning.items():
        pattern = re.compile(HELPER_CALL.format(name=re.escape(name)))
        if any(begin <= hand.start() < end
               for hand in pattern.finditer(code, begin, end)):
            found.extend(words)
    return found


# The setter that gives a node its name for assistive technology, and the `let`
# that destructures what a method hands back: `let (role, name, state) =
# self.semantics(hit)?`.
NAME_SETTER = re.compile(r"\.(?:with|set)_(?:accessible_)?name\s*\(")
TUPLE_CALL = re.compile(r"let\s*\(([^)]*)\)\s*=\s*(?:self\.)?([a-z_][A-Za-z0-9_]*)\s*\(")
SOME_OPEN = re.compile(r"^\s*Some\s*\(")


def element_of_tuple(code: str, begin: int, end: int,
                     element: int) -> list[tuple[int, int]]:
    """Where element `element` of the tuple `begin` and `end` enclose is built:
    its place in the tuple itself, in each arm of the `match` that picks it, or
    in the value the block hands back."""
    while begin < end and code[begin].isspace():
        begin += 1
    some = SOME_OPEN.match(code[begin:end])
    if some:
        return element_of_tuple(code, begin + some.end(), end, element)
    if code.startswith("match ", begin):
        return tuple_arms(code, begin, end, element)
    if code.startswith("(", begin):
        open_element, close_element = argument(code, begin + 1, element)
        return [(open_element, close_element)] if open_element < close_element else []
    if code.startswith("{", begin):
        block = rust_source.matching(code, begin) - 1
        found = []
        for value_begin, value_end in return_ranges(code, begin, block):
            found.extend(element_of_tuple(code, value_begin, value_end, element))
        return found
    return []


def tuple_words(code: str, starts: list[int],
                by_start: dict[int, rust_source.Literal], kept: str,
                begin: int, end: int, element: int) -> list[rust_source.Literal]:
    """The English words element `element` of the tuple `[begin, end)`, or of
    the `match` that picks the tuple, is built from."""
    found = []
    for element_begin, element_end in element_of_tuple(code, begin, end, element):
        for literal in (painted_literals(code, starts, by_start, element_begin, element_end)
                        + format_template(code, starts, by_start, element_begin,
                                          element_end)):
            if kept[literal.start] == "x" and prose_words(literal.value):
                found.append(literal)
    return found


def named_element(binding: re.Match[str], code: str) -> int | None:
    """The position in the tuple `binding` destructures whose name the same
    function hands to a name setter, or None when none of them is a name."""
    names = [piece.strip().removeprefix("mut ").strip()
             for piece in binding.group(1).split(",")]
    enclosing = [(function, code.find("{", function.end()))
                 for function in FUNCTION.finditer(code, 0, binding.start())]
    enclosing = [(function, brace) for function, brace in enclosing
                 if brace >= 0 and brace < binding.start()
                 and binding.start() < rust_source.matching(code, brace)]
    if not enclosing:
        return None
    _, brace = enclosing[-1]
    close = rust_source.matching(code, brace)
    for element, name in enumerate(names):
        if not re.fullmatch(r"[a-z_][A-Za-z0-9_]*", name) or name == "_":
            continue
        for setter in NAME_SETTER.finditer(code, brace, close):
            first_begin, first_end = argument(code, setter.end(), 0)
            if first_end > first_begin and bare(code[first_begin:first_end], name):
                return element
    return None


def named_tuple_words(root: Path,
                      paths: list[Path]) -> dict[str, list[tuple[str, int, str]]]:
    """Per function name of one crate, the words it hands back for a name an
    assistive technology hears, with where they are written.

    A widget projects its nodes as tuples and names each one from an element
    (`let (target, name, role, state) = self.a11y_control(action)?` and then
    `.with_name(name)`), so that element of what the projecting function
    returns is copy: a reader with a German desktop says "Termin öffnen", not
    "Open event".
    """
    elements: dict[str, set[int]] = {}
    lexed = []
    for path in paths:
        text = path.read_text(errors="replace")
        if "fn " not in text:
            continue
        code, literals = rust_source.lex(text)
        for binding in TUPLE_CALL.finditer(code):
            element = named_element(binding, code)
            if element is not None:
                elements.setdefault(binding.group(2), set()).add(element)
        lexed.append((path, text, code, literals))
    table: dict[str, list[tuple[str, int, str]]] = {}
    for path, text, code, literals in lexed:
        if not any(name in text for name in elements):
            continue
        marked = list(code)
        for literal in literals:
            for index in range(literal.start, literal.end):
                if marked[index] != "\n":
                    marked[index] = "x"
        kept = rust_source.blank_test_items("".join(marked))
        by_start = {literal.start: literal for literal in literals if not literal.byte}
        starts = sorted(by_start)
        for function in FUNCTION.finditer(code):
            positions = elements.get(function.group(1))
            if not positions or kept[function.start()] == " ":
                continue
            brace = code.find("{", function.end())
            if brace < 0:
                continue
            close = rust_source.matching(code, brace)
            values = [(binding.end(), statement_end(code, binding.end()))
                      for binding in BINDING.finditer(code, brace, close)
                      if binding.group(2) is not None]
            values.extend(return_ranges(code, brace, close))
            words = set()
            for element in positions:
                for begin, end in values:
                    words.update(tuple_words(code, starts, by_start, kept, begin, end,
                                             element))
            if words:
                found = table.setdefault(function.group(1), [])
                found.extend((path.relative_to(root).as_posix(),
                              rust_source.line_number(text, literal.start),
                              literal.value)
                             for literal in words)
    return {name: sorted(set(words)) for name, words in table.items()}


def source_copy_literals(root: Path) -> list[tuple[str, int, str]]:
    """(file, line, text) of each English literal handed to a copy builder by a
    crate that ships translations, in production code; of each literal or
    `format!` sentence a surface hands to a text constructor or through a
    helper of its crate; and of each title it publishes for a resource or an
    action."""
    from check_app_copy import first_argument

    units = translated_units(root)
    windows = surfaces(root)
    sources = rust_sources(root)
    by_crate: dict[str, list[Path]] = {}
    for path in sources:
        location = path.relative_to(root).as_posix()
        for crate in windows:
            if location.startswith(f"{crate}/"):
                by_crate.setdefault(crate, []).append(path)
    helpers: dict[str, dict[str, set[int]]] = {}
    returning: dict[str, dict[str, list[tuple[str, int, str]]]] = {}
    naming: dict[str, dict[str, list[tuple[str, int, str]]]] = {}
    fields: dict[str, set[tuple[str, str]]] = {}
    constants: dict[str, dict[str, list[tuple[str, int, str]]]] = {}
    mappers: dict[str, re.Pattern[str] | None] = {}
    bars: dict[str, set[str]] = {}
    found = []
    for path in sources:
        unit = path.relative_to(root).as_posix()
        builder_unit = any(unit.startswith(f"{crate}/") for crate in units)
        crate = next((c for c in windows if unit.startswith(f"{c}/")), None)
        window = crate is not None
        if not builder_unit and not window:
            continue
        text = path.read_text(errors="replace")
        builders = tuple(b for b in COPY_BUILDERS if b in text) if builder_unit else ()
        wire = tuple(b for b in WIRE_MESSAGE_BUILDERS if b in text) if builder_unit else ()
        labels = builder_unit and ACTION_LABEL.search(text) is not None
        if (not builders and not wire and not labels and "ButtonFace::" not in text
                and not any(call in text for call, _ in TEXT_CONSTRUCTORS + SPEC_BUILDERS)
                and not (window and any(marker in text for marker in BAR_MARKERS))):
            continue
        code, literals = rust_source.lex(text)
        marked = list(code)
        for literal in literals:
            for index in range(literal.start, literal.end):
                if marked[index] != "\n":
                    marked[index] = "x"
        kept = rust_source.blank_test_items("".join(marked))
        by_start = {literal.start: literal for literal in literals if not literal.byte}
        starts = sorted(by_start)
        hits: list[rust_source.Literal] = []

        def painted(begin: int, end: int) -> None:
            for literal in painted_literals(code, starts, by_start, begin, end):
                if kept[literal.start] == "x" and english_words(literal.value):
                    hits.append(literal)

        def sentences(begin: int, end: int, close: int, bindings) -> None:
            """The words at `[begin, end)`, and a sentence or a selected local
            anywhere in the call ending at `close`."""
            painted(begin, end)
            for literal in format_template(code, starts, by_start, begin, close):
                if kept[literal.start] == "x" and english_words(literal.value):
                    hits.append(literal)
            for name, stored in bindings.items():
                at = named_position(code, begin, close, name)
                if at < 0:
                    continue
                for (body, end_of_body), literal in stored:
                    if body <= at < end_of_body and kept[literal.start] == "x":
                        hits.append(literal)
            if (pattern := mappers.get(crate)) and pattern.search(code, begin, close):
                return
            # Words a constant reaches this painted place with: the constant's
            # own definition is where the source language is written.
            for name, words in (constants.get(crate) or {}).items():
                if named_in(code, begin, close, name):
                    found.extend(words)

        bindings = (copy_bindings(code, starts, by_start, kept)
                    if window or labels else {})
        for builder in builders:
            start = code.find(builder)
            while start >= 0:
                begin, end = first_argument(code, start + len(builder))
                if begin < len(code) and code[begin] == "&":
                    begin += 1
                painted(begin, end)
                start = code.find(builder, start + len(builder))
        for builder in wire:
            start = code.find(builder)
            while start >= 0:
                opened = start + len(builder) - 1
                close = rust_source.matching(code, opened)
                # The message is the sequence itself: a literal or a
                # `format!` template handed to it, never a local named
                # somewhere else in the file.
                begin, end = struct_field(code, opened + 1, close, "message")
                sentences(begin, end, close, {})
                start = code.find(builder, start + len(builder))
        if labels:
            for field in ACTION_LABEL.finditer(code):
                end = statement_end(code, field.end())
                sentences(field.end(), end, end, bindings)
        if window:
            if crate not in helpers:
                helpers[crate] = helper_table(by_crate[crate])
                returning[crate] = returning_words(root, by_crate[crate])
                naming[crate] = named_tuple_words(root, by_crate[crate])
                constants[crate] = word_constant_table(root, by_crate[crate])
                named = key_mappers(by_crate[crate], catalogue_keys(root, crate))
                mappers[crate] = (
                    re.compile(r"(?<![\w$])(?:{})\s*\(".format(
                        "|".join(re.escape(name) for name in sorted(named))))
                    if named
                    else None
                )
            ranges = []
            for call, index in TEXT_CONSTRUCTORS:
                start = code.find(call)
                while start >= 0:
                    begin, end = argument(code, start + len(call), index)
                    if begin < end:
                        sentences(begin, end,
                                  rust_source.matching(code, start + len(call) - 1), bindings)
                        ranges.append((begin, end))
                        found.extend(words_returned_in(code, begin, end, returning[crate]))
                    start = code.find(call, start + len(call))
            for call, index in SPEC_BUILDERS:
                start = code.find(call)
                while start >= 0:
                    begin, end = argument(code, start + len(call), index)
                    if begin < end:
                        sentences(begin, end,
                                  rust_source.matching(code, start + len(call) - 1), bindings)
                        ranges.append((begin, end))
                        found.extend(words_returned_in(code, begin, end, returning[crate]))
                    start = code.find(call, start + len(call))
            for words in naming[crate].values():
                found.extend(words)
            if crate not in bars:
                bars[crate] = bar_command_names(by_crate[crate])
            for begin, end in bar_label_ranges(code, bars[crate]):
                sentences(begin, end, end, bindings)
                ranges.append((begin, end))
                found.extend(words_returned_in(code, begin, end, returning[crate]))
            for builder in ARG_BUILDERS:
                start = code.find(builder)
                while start >= 0:
                    close = rust_source.matching(code, start + len(builder) - 1)
                    for value_begin, value_end in argument_values(
                            code, start + len(builder), close - 1):
                        sentences(value_begin, value_end, value_end, bindings)
                    start = code.find(builder, start + len(builder))
            for name, indexes in helpers[crate].items():
                pattern = re.compile(HELPER_CALL.format(name=re.escape(name)))
                for hand in pattern.finditer(code):
                    for index in indexes:
                        begin, end = argument(code, hand.end(), index)
                        if begin >= end:
                            continue
                        sentences(begin, end, end, bindings)
                        ranges.append((begin, end))
                        found.extend(words_returned_in(code, begin, end,
                                                       returning[crate]))
            for binding, field, literal in stored_fields(code, starts, by_start, kept):
                if read_at(code, ranges, binding, field):
                    found.append((path.relative_to(root).as_posix(),
                                  rust_source.line_number(text, literal.start),
                                  literal.value))
            if crate not in fields:
                fields[crate] = set()
                for other in by_crate[crate]:
                    source = other.read_text(errors="replace")
                    if "ButtonFace::" in source:
                        fields[crate] |= painted_fields(rust_source.lex(source)[0])
            for literal in painted_literal_fields(code, starts, by_start, kept,
                                                  fields[crate]):
                found.append((path.relative_to(root).as_posix(),
                              rust_source.line_number(text, literal.start),
                              literal.value))
        for literal in dict.fromkeys(hits):
            found.append((
                path.relative_to(root).as_posix(),
                rust_source.line_number(text, literal.start),
                literal.value,
            ))
    return sorted(set(found))


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
          save_exceptions_path: Path | None = None,
          source_literals_path: Path | None = None) -> list[str]:
    glossary = load_glossary(glossary_path)
    baseline = load_baseline(baseline_dir)
    exceptions = load_save_exceptions(save_exceptions_path) if save_exceptions_path else set()
    literals = (load_save_exceptions(source_literals_path)
                if source_literals_path else set())
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
    used_literals: set[SaveException] = set()
    for file, line, text in source_copy_literals(root):
        exception = SaveException(None, None, file, text)
        if exception in literals:
            used_literals.add(exception)
            continue
        errors.append(f"{file}:{line}: \"{text}\" is painted as-is; a window that ships a "
                      "translation resolves its copy through the crate's catalogue "
                      "[uncatalogued]")
    name_literals = named(root, source_literals_path) if source_literals_path else "source copy"
    for exception in sorted(literals - used_literals, key=repr):
        errors.append(f"{name_literals}: {exception.file}: \"{exception.text}\" no longer "
                      "matches anything; delete it")
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
    errors = check(ROOT, GLOSSARY, BASELINE, SAVE_EXCEPTIONS, SOURCE_LITERALS)
    if errors:
        print("\n".join(errors))
        return 1
    print("copy language OK (state, outcome or next step, in the glossary's words)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
