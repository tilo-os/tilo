#!/usr/bin/env python3
"""A test that pins something the user asked for keeps its assertions.

A Rust test is pinned by a comment right above it (attributes and further
comment lines may sit between the two):

    // USER REQUIREMENT (2026-10-18): "<what the user said, in their words>"
    #[test]
    fn ...

Such a test states what the product must do, so code is changed to pass it,
never the other way round. For every commit in BASE..HEAD, every test pinned in
the commit's parent must still exist in the commit (same name, in the same file
or another Rust file the commit touches), still be pinned, and make the same
assertions: each call whose name starts with `assert` (macros such as
`assert_eq!` and helpers such as `assert_widths(...)`), compared with all
whitespace removed, in order. Renaming the test, dropping the marker, deleting
an assertion or editing one all fail.

When the user has agreed to the change, the commit says so in a trailer,
which this check then accepts:

    User-approved: <what the user agreed to, and when>

Usage: check_user_requirement_tests.py BASE [HEAD]
"""
import argparse
import re
import subprocess
import sys

MARKER = re.compile(r"^\s*//\s*USER REQUIREMENT\b")
FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
APPROVAL = re.compile(r"^User-approved:\s*\S", re.MULTILINE)
ASSERT_CALL = re.compile(r"\bassert[A-Za-z0-9_]*!?\s*[\(\[\{]")


def git(*args):
    """Git in the repository the check runs in (the current directory's)."""
    return subprocess.check_output(["git", *args])


def code_mask(text):
    """`text` with every comment, string and char literal blanked to spaces,
    so braces and parentheses inside them do not count."""
    out = list(text)
    i, n = 0, len(text)

    def blank(start, end):
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            blank(i, end)
            i = end
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            blank(i, end)
            i = end
        elif c == "r" and re.match(r'r#*"', text[i:]) and not (i and (text[i - 1].isalnum() or text[i - 1] == "_")):
            hashes = len(re.match(r"r(#*)\"", text[i:]).group(1))
            close = '"' + "#" * hashes
            end = text.find(close, i + 2 + hashes)
            end = n if end < 0 else end + len(close)
            blank(i, end)
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i, j + 1)
            i = j + 1
        elif c == "'":
            literal = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'", text[i:])
            if literal:
                blank(i, i + literal.end())
                i += literal.end()
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def matching(masked, open_at):
    """Index just past the bracket that closes the one at `open_at`."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    stack = []
    for k in range(open_at, len(masked)):
        ch = masked[k]
        if ch in pairs:
            stack.append(pairs[ch])
        elif stack and ch == stack[-1]:
            stack.pop()
            if not stack:
                return k + 1
    return len(masked)


def functions(text):
    """{name: (pinned, [assertion, ...])} for every fn in `text`, the first of a name winning."""
    masked = code_mask(text)
    lines = text.split("\n")
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line) + 1)
    pinned_names = set()
    for index, line in enumerate(lines):
        if not MARKER.match(line):
            continue
        depth = 0
        for follow in range(index + 1, len(lines)):
            stripped = lines[follow].strip()
            if depth == 0:
                found = FN.match(lines[follow])
                if found:
                    pinned_names.add(found.group(1))
                    break
                if not (stripped == "" or stripped.startswith("//") or stripped.startswith("#")):
                    break
            depth += lines[follow].count("[") - lines[follow].count("]")
    result = {}
    for index, line in enumerate(lines):
        found = FN.match(masked[offsets[index]:offsets[index + 1]])
        if not found or found.group(1) in result:
            continue
        brace = masked.find("{", offsets[index])
        semicolon = masked.find(";", offsets[index])
        if brace < 0 or (0 <= semicolon < brace):
            continue
        end = matching(masked, brace)
        asserts = []
        for call in ASSERT_CALL.finditer(masked, brace, end):
            close = matching(masked, call.end() - 1)
            asserts.append(re.sub(r"\s+", "", text[call.start():close]))
        result[found.group(1)] = (found.group(1) in pinned_names, asserts)
    return result


def blob(commit, path):
    try:
        return git("show", f"{commit}:{path}").decode("utf-8", errors="replace")
    except subprocess.CalledProcessError:
        return None


def check_commit(commit):
    """The failures `commit` makes against the tests its parent pins."""
    raw = git("cat-file", "commit", commit).decode("utf-8", errors="replace")
    headers, _, message = raw.partition("\n\n")
    parents = [line[7:] for line in headers.splitlines() if line.startswith("parent ")]
    if not parents or APPROVAL.search(message):
        return []
    parent = parents[0]
    changed = [
        path for path in git("diff-tree", "-r", "--name-only", "--no-commit-id", parent, commit)
        .decode().splitlines() if path.endswith(".rs")
    ]
    after = {path: functions(text) for path in changed if (text := blob(commit, path)) is not None}
    failures = []
    for path in changed:
        before = blob(parent, path)
        if before is None:
            continue
        for name, (pinned, asserts) in functions(before).items():
            if not pinned:
                continue
            here = after.get(path, {}).get(name)
            if here is None:
                here = next((fns[name] for fns in after.values() if name in fns), None)
            where = f"{commit[:12]}: {path}: test `{name}` pins a user requirement"
            if here is None:
                failures.append(f"{where} and was removed or renamed")
            elif not here[0]:
                failures.append(f"{where} and lost its USER REQUIREMENT marker")
            elif here[1] != asserts:
                gone = [a for a in asserts if a not in here[1]]
                new = [a for a in here[1] if a not in asserts]
                detail = "".join(f"\n    - {a[:150]}" for a in gone) + "".join(f"\n    + {a[:150]}" for a in new)
                failures.append(f"{where} and its assertions changed:{detail or ' (reordered)'}")
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("base")
    parser.add_argument("head", nargs="?", default="HEAD")
    args = parser.parse_args()
    commits = git("rev-list", "--reverse", f"{args.base}..{args.head}").decode().split()
    failures = [failure for commit in commits for failure in check_commit(commit)]
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(
            "\nA USER REQUIREMENT test states what the user asked for: change the code to pass it."
            "\nIf the user agreed to change the requirement itself, say so in the commit message:"
            "\n    User-approved: <what they agreed to, and when>",
            file=sys.stderr,
        )
        return 1
    print(f"User requirement tests OK: {len(commits)} commits inspected")
    return 0


if __name__ == "__main__":
    sys.exit(main())
