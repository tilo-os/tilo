#!/usr/bin/env python3
"""Check the other words actions declare for search.

An action declares the words people use for it beside its title: a shell
action names a `search-kw-*` catalogue message, an app's action or effect
says `keywords = "…"` in its `#[action]` or `#[effect]` attribute. Search
matches them and never shows them, so the copy check skips them; this check
holds them to the copy rules that still apply to words a person types:

- plain words, one phrase each, comma separated, none empty or listed twice;
- no code: no identifiers (`snake_case`, `a.b`, `a::b`), no markup or
  placeholders;
- no internal terms: the implementation's names for things people know by
  other names (a workspace is not a room, a device is not a peer).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Words for the implementation, never for what a person sees.
INTERNAL_TERMS = re.compile(
    r"\b(?:room|rooms|peer|peers|enroll|enrol|enrollment|broker|facet|descriptor|"
    r"automerge|crdt|payload|uuid|resource id)\b",
    re.IGNORECASE,
)
CODE = re.compile(r"[_{}<>$`=\\]|::|[a-z]\.[a-z]")
ATTRIBUTE = re.compile(r"#\[(?:action|effect)\((?P<body>[^\]]*)\)\]", re.DOTALL)
KEYWORDS = re.compile(r'\bkeywords\s*=\s*"(?P<list>[^"]*)"')
MESSAGE = re.compile(r"^(?P<key>search-kw-[A-Za-z0-9-]+) = (?P<list>.*)$", re.MULTILINE)


def phrase_problems(words: str) -> list[str]:
    problems = []
    seen: set[str] = set()
    for word in (part.strip() for part in words.split(",")):
        if not word:
            problems.append("an empty phrase")
            continue
        if word.casefold() in seen:
            problems.append(f"{word!r} is listed twice")
        seen.add(word.casefold())
        if CODE.search(word):
            problems.append(f"{word!r} is code, not a word people type")
        internal = INTERNAL_TERMS.search(word)
        if internal:
            problems.append(f"{word!r} uses the internal term {internal.group()!r}")
    return problems


def catalogue_findings(root: Path) -> list[str]:
    findings = []
    for path in sorted(root.glob("crates/**/locales/*.ftl")) + sorted(root.glob("apps/**/locales/*.ftl")):
        text = path.read_text(encoding="utf-8")
        for match in MESSAGE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            for problem in phrase_problems(match.group("list")):
                findings.append(f"{path.relative_to(root)}:{line}: {match.group('key')}: {problem}")
    return findings


def declaration_findings(root: Path) -> list[str]:
    findings = []
    for directory in ("apps", "crates", "examples"):
        for path in sorted((root / directory).glob("**/*.rs")):
            if "/target/" in path.as_posix() or "/vendor/" in path.as_posix():
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if "keywords" not in text:
                continue
            for attribute in ATTRIBUTE.finditer(text):
                keywords = KEYWORDS.search(attribute.group("body"))
                if not keywords:
                    continue
                line = text.count("\n", 0, attribute.start()) + 1
                for problem in phrase_problems(keywords.group("list")):
                    findings.append(f"{path.relative_to(root)}:{line}: {problem}")
    return findings


def main() -> int:
    findings = catalogue_findings(ROOT) + declaration_findings(ROOT)
    if findings:
        print(f"action keywords: {len(findings)} problem(s); see tools/policy/check_action_keywords.py")
        for finding in findings:
            print(finding)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
