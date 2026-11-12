"""Types for Git (`so.tilo.git-client`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Repo` is the app's `git_client.repo` at `git-client/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "Branch",
    "ChangeArea",
    "ChangeKind",
    "Commit",
    "FileChange",
    "RefKind",
    "RefLabel",
    "Remote",
    "SharedRepo",
    "Stash",
    "Tag",
    "Repo",
]

class Branch(TypedDict):
    name: str
    is_head: bool
    is_remote: bool
    upstream: str | None
    ahead: int
    behind: int
    tip: str

ChangeArea = Literal["Staged", "Unstaged", "Untracked"]

ChangeKind = Literal["Added", "Modified", "Deleted", "Renamed", "Untracked", "Conflicted"]

class Commit(TypedDict):
    id: str
    summary: str
    body: str
    author_name: str
    author_email: str
    when: str
    time: int
    parents: list[str]
    refs: list[RefLabel]
    lane: int
    files: list[FileChange]

class FileChange(TypedDict):
    path: str
    old_path: str | None
    area: ChangeArea
    kind: ChangeKind
    additions: int
    deletions: int

RefKind = Literal["Head", "LocalBranch", "RemoteBranch", "Tag"]

class RefLabel(TypedDict):
    name: str
    kind: RefKind

class Remote(TypedDict):
    name: str
    url: str

class SharedRepo(TypedDict):
    name: str
    head: str
    workdir: str
    commits: list[Commit]
    branches: list[Branch]
    tags: list[Tag]
    stashes: list[Stash]
    remotes: list[Remote]
    graph_width: int
    author_name: str
    author_email: str

class Stash(TypedDict):
    index: int
    message: str
    branch: str

class Tag(TypedDict):
    name: str
    target: str
    message: str | None

class Repo(Resource):
    """Repository — `git_client.repo`."""
    repository: str
    @property
    def shared(self) -> Any:
        """Read only."""
        return self.fields["shared"]

    def record_repository(self, *, shared: SharedRepo) -> Run | None:
        """Runs the app's action."""
        return self.action("record-repository", shared=shared)

    def diff_layout(self) -> None:
        """Runs the app's action."""
        return self.action("diff-layout")

    def syntax_highlight(self) -> None:
        """Runs the app's action."""
        return self.action("syntax-highlight")

    def show_whitespace(self) -> None:
        """Runs the app's action."""
        return self.action("show-whitespace")

    def wrap_lines(self) -> None:
        """Runs the app's action."""
        return self.action("wrap-lines")


