"""Which files a policy check judges, and the text it judges them by.

A check judges the whole tree by default. That is the mode tools/policy/run.sh
and CI run, and it fails on a violation anywhere, whoever wrote it.

The pre-commit hook runs the checks whose verdict on a file depends only on
that file with ``--staged``: they judge the files the commit stages, read from
the index, which is the tree the commit will contain. A commit can only
introduce a violation in a file it stages, so the hook is as strict about the
change itself as the whole-tree run is -- and it reads the handful of files in
the commit instead of the ten thousand of the tree, which is what keeps a
commit down to a few seconds. Nothing is lost by judging less here: the
whole-tree run still runs, and still names every check.

Reading the index rather than the working tree is the point. A staged file with
further, unstaged edits is judged as it is committed, so those edits cannot
hide a violation, and a merge is judged on what it resolved.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterator, Sequence

# The flag every check that can judge a commit takes.
STAGED_FLAG = "--staged"
STAGED_HELP = (
    "judge only the files this commit stages, read from the index, as the "
    "pre-commit hook does; without it every file of the tree is judged, as "
    "tools/policy/run.sh and CI do"
)


def add_staged_argument(parser) -> None:
    """Give ``parser`` the flag that selects a commit's files."""
    parser.add_argument(STAGED_FLAG, action="store_true", dest="staged", help=STAGED_HELP)


def git(root: Path, *args: str, input: bytes | None = None, empty: tuple[int, ...] = ()) -> bytes:
    """Run git in ``root`` and return its standard output.

    ``GIT_INDEX_FILE`` is left to the environment: the hook points it at the
    index of the commit it is assembling, which is the one to judge. An exit
    status in ``empty`` says nothing matched rather than that git failed.
    """
    finished = subprocess.run(
        ["git", "-C", str(root), *args],
        input=input,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if finished.returncode != 0 and finished.returncode not in empty:
        raise subprocess.CalledProcessError(
            finished.returncode, finished.args, finished.stdout, finished.stderr
        )
    return finished.stdout


@dataclass
class Judged:
    """The files a check judges in one tree: all of them, or one commit's.

    The tree is read once per process: the listing is one ``git`` call, and the
    text of each staged file is read once, so the checks the hook runs together
    over one commit share both.
    """

    root: Path
    staged: bool = False
    _changed: set[Path] | None = field(default=None, repr=False)
    _blobs: dict[Path, bytes] = field(default_factory=dict, repr=False)
    _only: set[Path] | None = field(default=None, repr=False)

    def holding(self, pattern: str, *scopes: str | Path) -> "Judged":
        """This judgement narrowed to the files whose text extends ``pattern``.

        A check that needs a name defined somewhere in the tree reads only the
        files that can define it. One ``git grep`` answers which they are: the
        expression is an extended regular one (``fn[[:space:]]+(decode|parse)``),
        and a file it does not match cannot hold what the check looks for.
        """
        scopes = [Path(scope) for scope in scopes if (self.root / scope).exists()]
        if not scopes:
            return Judged(root=self.root, staged=self.staged, _only=set())
        found = git(
            self.root,
            "grep",
            "-l",
            "-E",
            *(("--cached",) if self.staged else ()),
            pattern,
            "--",
            *[scope.as_posix() for scope in scopes],
            empty=(1,),
        )
        return Judged(
            root=self.root,
            staged=self.staged,
            _only={Path(os.fsdecode(name)) for name in found.splitlines() if name},
        )

    def files(
        self, *scopes: str | Path, suffixes: tuple[str, ...] = (".rs",)
    ) -> list[Path]:
        """The files under ``scopes`` to judge, relative to the root, sorted.

        A scope is a directory (``crates/sdk/ui-core``) or the whole root
        (``.``). In ``--staged`` mode only the files the commit stages are
        listed; each scope a check passes narrows that further.
        """
        found: set[Path] = set()
        for scope in scopes or (Path("."),):
            scope = Path(scope)
            for relative in self._within(scope, suffixes):
                if self._only is None or relative in self._only:
                    found.add(relative)
        return sorted(found)

    def _within(self, scope: Path, suffixes: tuple[str, ...]) -> Iterator[Path]:
        if self.staged:
            for relative in self.changed():
                if relative.suffix not in suffixes:
                    continue
                if scope in (Path("."), Path("")) or scope == relative or scope in relative.parents:
                    yield relative
            return
        base = self.root / scope
        if base.is_file():
            if scope.suffix in suffixes:
                yield scope
            return
        if not base.is_dir():
            return
        for suffix in suffixes:
            for path in base.rglob(f"*{suffix}"):
                if path.is_file():
                    yield path.relative_to(self.root)

    def changed(self) -> set[Path]:
        """The paths the index holds a change for: added, copied, modified, renamed."""
        if self._changed is None:
            names = git(self.root, "diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR")
            self._changed = {Path(os.fsdecode(name)) for name in names.split(b"\0") if name}
        return self._changed

    def is_staged(self, relative: Path) -> bool:
        return self.staged and relative in self.changed()

    def read(self, relative: str | Path, errors: str = "replace") -> str:
        """The text of ``relative``: its index blob when the commit stages it, else the file's."""
        relative = Path(relative)
        if self.is_staged(relative):
            return self._index_blob(relative).decode("utf-8", errors)
        return (self.root / relative).read_text(encoding="utf-8", errors=errors)

    def sources(
        self,
        *scopes: str | Path,
        suffixes: tuple[str, ...] = (".rs",),
        errors: str = "replace",
    ) -> Iterator[tuple[Path, str]]:
        """``(relative, text)`` for each file to judge, in path order."""
        relatives = self.files(*scopes, suffixes=suffixes)
        if self.staged:
            # One `git cat-file` for the whole commit, not one per file.
            self._blobs.update(
                self._index_blobs([relative for relative in relatives if relative not in self._blobs])
            )
        return ((relative, self.read(relative, errors)) for relative in relatives)

    def _index_blob(self, relative: Path) -> bytes:
        if relative not in self._blobs:
            self._blobs.update(self._index_blobs([relative]))
            if relative not in self._blobs:
                # Git holds no blob for it (a gitlink, or a name it cannot be
                # asked about): the file on disk is what there is.
                self._blobs[relative] = (self.root / relative).read_bytes()
        return self._blobs[relative]

    def _index_blobs(self, relatives: Sequence[Path]) -> dict[Path, bytes]:
        """The index blobs of ``relatives``, from one ``git cat-file`` call.

        A path git cannot answer for (a newline in the name, a gitlink) is left
        out, and its reader falls back to the file on disk.
        """
        wanted = [relative for relative in relatives if "\n" not in relative.as_posix()]
        if not wanted:
            return {}
        query = b"".join(b":" + os.fsencode(relative.as_posix()) + b"\n" for relative in wanted)
        output = git(self.root, "cat-file", "--batch", input=query)
        blobs: dict[Path, bytes] = {}
        offset = 0
        for relative in wanted:
            end = output.index(b"\n", offset)
            header = output[offset:end].split()
            offset = end + 1
            if len(header) != 3:
                continue
            _oid, kind, size = header
            if kind != b"blob":
                continue
            length = int(size)
            blobs[relative] = output[offset : offset + length]
            offset += length + 1
        return blobs


def judged(root: Path, staged: bool = False) -> Judged:
    """The files of ``root``: all of them, or the ones a commit stages."""
    return Judged(root, staged)
