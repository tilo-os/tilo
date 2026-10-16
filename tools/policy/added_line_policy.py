#!/usr/bin/env python3
"""Lines a change adds carry no credential, merge-conflict marker or dbg!.

One definition, three callers: the pre-commit hook (`--staged`),
scripts/scan-secrets.sh (`--secrets`, optionally `--all`) and CI's per-commit
pass in tools/policy/check_commit_policy.py, which repeats the check on every
new commit and so catches one made with --no-verify.

Only added lines count. A line is added when no parent version of the same file
holds it, so an existing string does not fire again on every commit that
touches its file, a line moved within a file is not new, and a merge is charged
only with what its resolution introduced.

The credential patterns are deliberately narrow: a scanner that cries wolf gets
bypassed, and a bypassed scanner catches nothing. Each matches a credential
*shape* with a vendor prefix or a fixed length, not "a long string".
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]

# Square brackets around a single literal character keep each pattern's own
# source line from matching it, so this file passes its own scan.
SECRET_PATTERNS = (
    ("AWS access key", re.compile(r"AKIA[0-9A-Z]{16}")),
    ("GitHub token", re.compile(r"gh[pousr]_[A-Za-z0-9]{36,}")),
    ("GitLab token", re.compile(r"glpat-[A-Za-z0-9_-]{20,}")),
    ("Slack token", re.compile(r"xox[abprs]-[A-Za-z0-9-]{10,}")),
    ("Stripe key", re.compile(r"sk_live_[A-Za-z0-9]{24,}")),
    ("Anthropic key", re.compile(r"sk-ant-[A-Za-z0-9_-]{20,}")),
    ("OpenAI key", re.compile(r"sk-[A-Za-z0-9]{48}")),
    ("Google API key", re.compile(r"AIza[A-Za-z0-9_-]{35}")),
    ("private key block", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("PuTTY private key", re.compile(r"PuTTY-User-Key-File[-]")),
    ("JWT", re.compile(r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.")),
    ("npm token", re.compile(r"npm_[A-Za-z0-9]{36}")),
    ("password in a URL", re.compile(r"[a-z][a-z0-9+.-]*://[^/:@\s]+:[^/@\s]{6,}@")),
)
# A committed example is fine as long as it says so on the same line. CI has
# no --no-verify, so this marker is the only way past the credential rule.
ALLOW = re.compile(r"example|placeholder|dummy|fake|redacted|not-?a-?real|test-only|EXAMPLEKEY", re.I)
CONFLICT_MARKER = re.compile(r"^(<{7}|={7}|>{7})( |$)")
DBG = re.compile(r"(^|[^a-z_])dbg!\(")

GUIDANCE = """\
  A credential hit that is genuinely not a credential needs `example` or
  `placeholder` on the same line. That marker is the only exception: CI repeats
  this check on every new commit, so --no-verify does not get a hit past it.
  If it IS a credential: do not amend it away. It is in your reflog and, once
  pushed, in everyone's. Rotate the key first, then remove it.
  Resolve merge-conflict markers and remove dbg! before committing."""


def redact(secret: str) -> str:
    """Enough of a match to find it again, never the credential itself."""
    return secret[:4] + "..." + f" ({len(secret)} chars)"


def checks_dbg(path: str) -> bool:
    # A vendored crate under vendor/ is upstream's code, carried as it ships.
    return path.endswith(".rs") and not path.startswith("vendor/")


def added_line_issues(path: str, lines, secrets_only: bool = False) -> list[str]:
    """Findings for `lines`, (line number, text) pairs that `path` adds."""
    issues = []
    dbg = not secrets_only and checks_dbg(path)
    for number, text in lines:
        where = f"{path}:{number}: "
        if not ALLOW.search(text):
            for name, pattern in SECRET_PATTERNS:
                match = pattern.search(text)
                if match:
                    issues.append(where + f"possible {name} ({redact(match.group())})")
        if secrets_only:
            continue
        if CONFLICT_MARKER.match(text):
            issues.append(where + "merge-conflict marker")
        if dbg and DBG.search(text):
            issues.append(where + "dbg!")
    return issues


def added_lines(new: bytes, parents) -> list[tuple[int, str]]:
    """Lines of `new` that no parent version holds, numbered as in `new`."""
    if b"\0" in new:
        return []
    known = set()
    for parent in parents:
        known.update(parent.decode("utf-8", "replace").split("\n"))
    return [(number, line)
            for number, line in enumerate(new.decode("utf-8", "replace").split("\n"), 1)
            if line and line not in known]


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(root), *args])


@contextmanager
def object_reader(root: Path):
    """Read objects by name through one Git process; None when absent."""
    with subprocess.Popen(["git", "-C", str(root), "cat-file", "--batch"],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL) as process:
        def read(name: str):
            process.stdin.write(name.encode() + b"\n")
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) == 2 and header[1] == b"missing":
                return None
            if len(header) != 3:
                raise ValueError("cannot read " + name)
            size = int(header[2])
            if size < 0:
                raise ValueError("invalid object size: " + name)
            data = process.stdout.read(size)
            if len(data) != size or process.stdout.read(1) != b"\n":
                raise ValueError("incomplete object: " + name)
            return data if header[1] == b"blob" else None
        try:
            yield read
        finally:
            process.stdin.close()
            process.stdout.close()
        if process.wait() != 0:
            raise ValueError("Git object reader failed")


def staged_parents(root: Path) -> list[str]:
    head = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", "HEAD"],
                          capture_output=True, text=True)
    parents = [head.stdout.strip()] if head.returncode == 0 else []
    merge = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", "MERGE_HEAD"],
                           capture_output=True, text=True)
    if merge.returncode == 0:
        parents.append(merge.stdout.strip())
    return parents


def scan_staged(root: Path, secrets_only: bool) -> list[str]:
    parents = staged_parents(root)
    # No commit yet: everything staged is added against the empty tree.
    against = parents[0] if parents else git(root, "hash-object", "-t", "tree", "/dev/null").decode().strip()
    raw = git(root, "diff", "--cached", "--raw", "-z", "--no-renames", "--no-abbrev",
              "--diff-filter=ACMT", against).split(b"\0")
    issues = []
    with object_reader(root) as read:
        for index in range(0, len(raw) - 1, 2):
            fields, path = raw[index].split(), raw[index + 1].decode(errors="replace")
            if fields[1] == b"160000" or "\n" in path:
                continue
            new = read(fields[3].decode())
            if new is None:
                continue
            olds = [read(parent + ":" + path) for parent in parents]
            lines = added_lines(new, [old for old in olds if old is not None])
            issues.extend(added_line_issues(path, lines, secrets_only))
    return issues


def scan_all(root: Path, secrets_only: bool) -> list[str]:
    """Every tracked blob as committed to the index, all lines counted (an audit)."""
    issues = []
    with object_reader(root) as read:
        for record in git(root, "ls-files", "--stage", "-z").split(b"\0"):
            if not record:
                continue
            metadata, path = record.split(b"\t", 1)
            mode, oid = metadata.split()[:2]
            if mode == b"160000":
                continue
            data = read(oid.decode())
            if data is not None:
                issues.extend(added_line_issues(path.decode(errors="replace"),
                                                added_lines(data, []), secrets_only))
    return issues


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--staged", action="store_true", help="scan the staged change (the default)")
    parser.add_argument("--all", action="store_true", help="scan every tracked file (an audit)")
    parser.add_argument("--secrets", action="store_true", help="only the credential rule")
    parser.add_argument("--root", type=Path, default=None)
    args = parser.parse_args(argv)
    root = args.root or Path(git(Path.cwd(), "rev-parse", "--show-toplevel").decode().strip())
    scan = scan_all if args.all else scan_staged
    issues = scan(root, args.secrets)
    if not issues:
        return 0
    for issue in issues:
        print("  " + issue[:160], file=sys.stderr)
    print("\n" + GUIDANCE, file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
