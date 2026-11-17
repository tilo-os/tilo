#!/usr/bin/env python3
"""Keep the words docs/glossary.md does not use out of the tree.

The glossary's "Not used" section lists the words that name nothing in Tilo:
old names of renamed things and vague metaphors. This check reads that list,
so adding a word there is enough, and finds each word and its plural in Rust,
Swift, Kotlin, JavaScript, TypeScript, Python and shell sources (identifiers,
comments and literals), Markdown, Fluent catalogues, tilolang sources and TOML
keys. Vendored and minified files are not judged. A word counts as a whole word or as one part of an
identifier: `room_id`, `RoomId` and `ROOM_ID` count, `bathroom`, `gateway`
and `outstanding` do not.

`glossary_words_allowed.toml` lists the uses that mean something outside
Tilo (a Wayland seat, an X.509 subject, a word list), by path or by the exact
identifier the word is part of. Every other use counts.

`glossary_words_baseline.txt` holds how many uses each word still has while
it is being renamed. A count only goes down: a commit that adds uses fails,
and a commit that removes some lowers the count (the pre-commit hook recounts
the index, writes the counts and stages them). That never raises a count by
what the tree already held: uses that arrived unjudged (a commit made before
its base had this check, then rebased onto it) stay above the baseline, where
judging the tree names their word and lists the uses added since the baseline
was last written, until they are renamed. A word the baseline
does not list has no uses at all.

    check_glossary_words.py            judge the tree
    check_glossary_words.py --staged   judge what the commit changes (the hook)
    check_glossary_words.py --list W   print every counted use of W
    check_glossary_words.py --update   lower the baseline to the tree's counts
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import re
import subprocess
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GLOSSARY = "docs/glossary.md"
ALLOWED = "tools/policy/glossary_words_allowed.toml"
BASELINE = "tools/policy/glossary_words_baseline.txt"
SCRIPT = "tools/policy/check_glossary_words.py"
SCANNED = (
    "*.rs", "*.md", "*.ftl", "*.tl", "*.toml",
    "*.swift", "*.kt", "*.kts", "*.js", "*.mjs", "*.cjs", "*.ts", "*.py", "*.sh",
)
SUFFIXES = tuple(pattern[1:] for pattern in SCANNED)
# Files that name the words to say they are not used.
NOT_JUDGED = (GLOSSARY, ALLOWED, BASELINE, SCRIPT, "tools/tests/test_glossary_words.py")
IDENTIFIER = re.compile(r"[A-Za-z0-9_]+")
BASELINE_HEADER = (
    "# Uses of each word docs/glossary.md does not use, while it is being renamed\n"
    "# (tools/policy/check_glossary_words.py). Counts only go down.\n"
)


@dataclass(frozen=True)
class Hit:
    path: str
    line: int
    text: str
    word: str


def not_used(glossary: str) -> list[str]:
    """The words of the glossary's "Not used" section, in order."""
    section = re.search(r"(?ms)^## Not used\s*$(.*?)(?=^## |\Z)", glossary)
    if not section:
        raise SystemExit(f"{GLOSSARY}: no '## Not used' section")
    words: list[str] = []
    # Each list is a paragraph "Label: word, word (except ...), word." and
    # whatever follows its full stop is prose.
    for paragraph in re.split(r"\n\s*\n", section.group(1)):
        listed = re.match(r"\s*[A-Z][^:\n]*:([^.]*)\.", re.sub(r"\([^)]*\)", "", paragraph))
        if not listed:
            continue
        for word in re.split(r"[,\s]+", listed.group(1)):
            word = word.strip().lower()
            if word and word not in words:
                words.append(word)
    return words


def plural(word: str) -> str:
    return word + ("es" if word.endswith(("s", "sh", "ch", "x")) else "s")


class Matcher:
    """Finds the words in a line, as whole words or identifier parts."""

    def __init__(self, words: list[str]):
        self.words = words
        self.base = {}
        for word in words:
            for form in (word, plural(word)):
                self.base[form] = word
        forms = sorted(self.base, key=len, reverse=True)
        lower = "|".join(forms)
        title = "|".join(form.capitalize() for form in forms)
        upper = "|".join(form.upper() for form in forms)
        # Lower case starts a word; a capital starts one after anything but
        # a capital (`ChatRoom`); all capitals stand alone (`ROOM_ID`).
        self.exact = re.compile(
            rf"(?<![A-Za-z])(?:{lower})(?![a-z])"
            rf"|(?<![A-Z])(?:{title})(?![a-z])"
            rf"|(?<![A-Za-z])(?:{upper})(?![A-Za-z])"
        )
        # The same in POSIX ERE for git grep, which has no lookaround: it
        # selects the lines, `exact` finds the uses in them.
        self.lines = (
            rf"(^|[^A-Za-z])({lower})([^a-z]|$)"
            rf"|(^|[^A-Z])({title})([^a-z]|$)"
            rf"|(^|[^A-Za-z])({upper})([^A-Za-z]|$)"
        )

    def find(self, text: str):
        for match in self.exact.finditer(text):
            yield match.start(), match.end(), self.base[match.group().lower()]


def toml_key(text: str) -> str:
    """The part of a TOML line that names a key or a table."""
    stripped = text.lstrip()
    if stripped.startswith("["):
        return text[: text.find("]") + 1] if "]" in text else text
    if "=" in stripped and not stripped.startswith(("#", '"""', "'''")):
        key = text[: text.find("=")]
        if not re.search(r"[\"'\[{,]\s*$", key) or key.strip().startswith(('"', "'")):
            return key
    return ""


class Allowed:
    def __init__(self, text: str):
        self.rules = []
        for rule in tomllib.loads(text).get("allow", []):
            self.rules.append(
                (set(rule["words"]), rule.get("paths", []), set(rule.get("identifiers", [])))
            )

    def __call__(self, word: str, path: str, identifier: str) -> bool:
        for words, paths, identifiers in self.rules:
            if word not in words and "*" not in words:
                continue
            if identifier in identifiers:
                return True
            if any(fnmatch.fnmatchcase(path, pattern) for pattern in paths):
                return True
        return False


def judged_path(path: str) -> bool:
    return (
        path.endswith(SUFFIXES)
        and not path.startswith("vendor/")
        and "/node_modules/" not in path
        and not path.endswith(".min.js")
        and path not in NOT_JUDGED
    )


def git(*args: str, env: dict | None = None, check: bool = True) -> bytes:
    done = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, env=env)
    if check and done.returncode not in (0, 1):
        raise SystemExit(f"git {args[0]}: {done.stderr.decode(errors='replace').strip()}")
    return done.stdout


def show(revision: str, path: str) -> str | None:
    """A file at `HEAD`, in the index (`""`) or in the working tree (`None`)."""
    if revision is None:
        file = ROOT / path
        return file.read_text(encoding="utf-8") if file.is_file() else None
    done = subprocess.run(["git", "show", f"{revision}:{path}"], cwd=ROOT, capture_output=True)
    return done.stdout.decode("utf-8") if done.returncode == 0 else None


def hits(matcher: Matcher, allowed: Allowed, where: list[str], paths: list[str] | None) -> list[Hit]:
    """The counted uses in the working tree (`where` empty), the index
    (`--cached`) or a commit, in `paths` or every scanned file."""
    env = None
    if paths is None:
        pathspecs = [*SCANNED, ":!vendor/", ":!*/node_modules/*", ":!*.min.js"]
    else:
        pathspecs = [path for path in paths if judged_path(path)]
        if not pathspecs:
            return []
        env = {**os.environ, "GIT_LITERAL_PATHSPECS": "1"}
    # PCRE is several times faster; a git built without it takes the ERE.
    cached = where == ["--cached"]
    command = ["grep", "-I", "-n", "-z", *(where if cached else [])]
    revision = [] if cached else where
    done = subprocess.run(
        ["git", *command, "-P", matcher.exact.pattern, *revision, "--", *pathspecs],
        cwd=ROOT, capture_output=True, env=env,
    )
    if done.returncode in (0, 1):
        out = done.stdout
    else:
        out = git(*command, "-E", matcher.lines, *revision, "--", *pathspecs, env=env)
    prefix = f"{where[0]}:" if where and where[0] != "--cached" else ""
    found: list[Hit] = []
    for record in out.decode("utf-8", "replace").split("\n"):
        if not record:
            continue
        path, number, text = record.split("\0", 2)
        path = path.removeprefix(prefix)
        if not judged_path(path):
            continue
        judged_text = toml_key(text) if path.endswith(".toml") else text
        for start, end, word in matcher.find(judged_text):
            identifier = identifier_at(judged_text, start, end)
            if not allowed(word, path, identifier):
                found.append(Hit(path, int(number), text.strip(), word))
    return found


def identifier_at(text: str, start: int, end: int) -> str:
    while start > 0 and IDENTIFIER.fullmatch(text[start - 1]):
        start -= 1
    while end < len(text) and IDENTIFIER.fullmatch(text[end]):
        end += 1
    return text[start:end]


def read_baseline(text: str | None) -> dict[str, int]:
    counts: dict[str, int] = {}
    for line in (text or "").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            word, count = line.split()
            counts[word] = int(count)
    return counts


def render_baseline(counts: dict[str, int], order: list[str]) -> str:
    lines = [f"{word} {counts[word]}" for word in order if counts.get(word)]
    return BASELINE_HEADER + "".join(f"{line}\n" for line in lines)


def setup(revision: str | None) -> tuple[list[str], Matcher, Allowed]:
    glossary = show(revision, GLOSSARY) or ""
    words = not_used(glossary)
    return words, Matcher(words), Allowed(show(revision, ALLOWED) or "")


def describe(found: list[Hit], word: str, limit: int = 40) -> list[str]:
    lines = [f"  {hit.path}:{hit.line}: {hit.text[:160]}" for hit in found if hit.word == word]
    if len(lines) > limit:
        lines = lines[:limit] + [f"  ... and {len(lines) - limit} more"]
    return lines


def new_uses(before: list[Hit], after: list[Hit]) -> list[Hit]:
    """The uses in `after` that `before` does not have, by file and line text."""
    old = Counter((hit.path, hit.word, hit.text) for hit in before)
    added = []
    for hit in after:
        if old[(hit.path, hit.word, hit.text)]:
            old[(hit.path, hit.word, hit.text)] -= 1
        else:
            added.append(hit)
    return added


def added_since_baseline(matcher: Matcher, allowed: Allowed) -> list[Hit]:
    """The uses in the working tree that the commit which last wrote the
    baseline did not have."""
    base = git("log", "-1", "--format=%H", "HEAD", "--", BASELINE).decode().strip()
    if not base:
        return []
    names = [
        name
        for name in git("diff", "--name-only", "--no-renames", "-z", base).decode().split("\0")
        if name
    ]
    return new_uses(hits(matcher, allowed, [base], names), hits(matcher, allowed, [], names))


def judge_tree() -> int:
    words, matcher, allowed = setup(None)
    found = hits(matcher, allowed, [], None)
    counts = Counter(hit.word for hit in found)
    baseline = read_baseline(show(None, BASELINE))
    over = [word for word in words if counts[word] > baseline.get(word, 0)]
    added = added_since_baseline(matcher, allowed) if over else []
    for word in words:
        have, may = counts[word], baseline.get(word, 0)
        if have > may:
            if may:
                print(f"'{word}': {have} uses, {may} allowed; added since the baseline was last "
                      f"written:", file=sys.stderr)
                print("\n".join(describe(added, word)), file=sys.stderr)
            else:
                print(f"'{word}' is not used (docs/glossary.md):", file=sys.stderr)
                print("\n".join(describe(found, word)), file=sys.stderr)
        elif have < may:
            print(f"'{word}': {have} uses, the baseline still allows {may}; "
                  f"run `{SCRIPT} --update`", file=sys.stderr)
    if over:
        print(f"Name the thing with the glossary's word; a use that means something outside "
              f"Tilo goes in {ALLOWED}.", file=sys.stderr)
    return 1 if over else 0


def head_counts() -> Counter:
    """`HEAD`'s tree as `HEAD`'s check counts it: its script, glossary and
    exceptions, so a commit that widens the check raises the counts by what
    it newly counts."""
    source = show("HEAD", SCRIPT)
    if source is None:
        _, matcher, allowed = setup("HEAD")
        return Counter(hit.word for hit in hits(matcher, allowed, ["HEAD"], None))
    name = "check_glossary_words_at_head"
    module = type(sys)(name)
    module.__file__ = str(ROOT / SCRIPT)
    sys.modules[name] = module
    try:
        exec(compile(source, SCRIPT, "exec"), module.__dict__)
        module.ROOT = ROOT
        _, matcher, allowed = module.setup("HEAD")
        return Counter(hit.word for hit in module.hits(matcher, allowed, ["HEAD"], None))
    finally:
        del sys.modules[name]


def judge_staged() -> int:
    """A commit adds no use to the files it stages. When it removes some, or
    changes what counts, the baseline is recounted from the index: a count
    goes down to what is left, and up only by what the commit itself makes
    count (an allowed use that no longer is one), never by uses the tree
    held unjudged before it."""
    names = [
        name
        for name in git("diff", "--cached", "--name-only", "--no-renames", "-z").decode().split("\0")
        if name
    ]
    has_head = subprocess.run(["git", "rev-parse", "-q", "--verify", "HEAD"], cwd=ROOT,
                              capture_output=True).returncode == 0
    head_words = not_used(show("HEAD", GLOSSARY) or "") if has_head else []
    words, matcher, allowed = setup("")
    recount = not has_head or any(name in (GLOSSARY, ALLOWED, SCRIPT, BASELINE) for name in names)
    judged_names = [name for name in names if judged_path(name)]
    staged_hits = hits(matcher, allowed, ["--cached"], judged_names)
    before = hits(matcher, allowed, ["HEAD"], judged_names) if has_head else []
    delta = Counter(hit.word for hit in staged_hits)
    delta.subtract(Counter(hit.word for hit in before))
    failed = False
    for word in words:
        # A word new to the glossary starts at its count.
        if word in head_words and delta[word] > 0:
            failed = True
            new = new_uses(before, staged_hits)
            print(f"'{word}' is not used (docs/glossary.md); this commit adds uses:",
                  file=sys.stderr)
            print("\n".join(describe(new, word)), file=sys.stderr)
    if failed:
        print(f"Name the thing with the glossary's word; a use that means something outside "
              f"Tilo goes in {ALLOWED}.", file=sys.stderr)
        return 1
    if not recount and not any(count < 0 for count in delta.values()):
        return 0
    total = Counter(hit.word for hit in hits(matcher, allowed, ["--cached"], None))
    if has_head:
        # A staged baseline may lower a count by hand and never raises one.
        may, staged_may = read_baseline(show("HEAD", BASELINE)), read_baseline(show("", BASELINE))
        grown: Counter = Counter()
        if recount:
            # What counts may have changed. A count rises by what this commit
            # itself makes count: the index against HEAD's tree as HEAD counted it.
            head_total = head_counts()
            grown = Counter({word: total[word] - head_total[word] for word in words})
        for word in words:
            # A word new to the glossary starts at its count. Any other keeps
            # the uses that reached the tree unjudged above its count, where
            # judging the tree names the word until they are renamed.
            if word in head_words:
                allowed_before = min(may.get(word, 0), staged_may.get(word, 0))
                total[word] = min(total[word], allowed_before + max(0, grown[word]))
    wanted = render_baseline(total, words)
    if (show("", BASELINE) or "") != wanted:
        if (show("", BASELINE) or "") != (show(None, BASELINE) or ""):
            print(f"{BASELINE} has unstaged edits; stage it, then commit again", file=sys.stderr)
            return 1
        (ROOT / BASELINE).write_text(wanted, encoding="utf-8")
        git("add", "--", BASELINE)
        print(f"  {BASELINE}: written with the uses left and staged", file=sys.stderr)
    return 0


def update() -> int:
    words, matcher, allowed = setup(None)
    counts = Counter(hit.word for hit in hits(matcher, allowed, [], None))
    path = ROOT / BASELINE
    if path.exists():
        baseline = read_baseline(path.read_text(encoding="utf-8"))
        raised = [word for word in words if counts[word] > baseline.get(word, 0)]
        if raised:
            print(f"more uses than the baseline allows: {', '.join(raised)}; "
                  f"the baseline only goes down", file=sys.stderr)
            return 1
    path.write_text(render_baseline(counts, words), encoding="utf-8")
    return 0


def list_uses(word: str) -> int:
    _, matcher, allowed = setup(None)
    for hit in hits(matcher, allowed, [], None):
        if hit.word == word:
            print(f"{hit.path}:{hit.line}: {hit.text}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--staged", action="store_true", help="judge the staged change")
    mode.add_argument("--update", action="store_true", help="lower the baseline")
    mode.add_argument("--list", metavar="WORD", help="print every counted use of WORD")
    args = parser.parse_args()
    if args.staged:
        return judge_staged()
    if args.update:
        return update()
    if args.list:
        return list_uses(args.list)
    return judge_tree()


if __name__ == "__main__":
    sys.exit(main())
