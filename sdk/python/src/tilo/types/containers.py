"""Types for Containers (`so.tilo.containers`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Draft` is the app's `tl.so.tilo.containers.Draft` at `containers/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Draft",
    "Pull",
    "Containers",
    "PullAsk",
    "JobAnswer",
    "JobState",
    "TiloContainers",
    "ClearCreateArgs",
    "FillArgs",
    "BeginPullArgs",
    "PollPullArgs",
    "CancelPullArgs",
    "DismissPullArgs",
]

ClearCreateArgs = TypedDict("ClearCreateArgs", {
})
"""The arguments of `clear_create`."""

FillArgs = TypedDict("FillArgs", {
    "field": "str",
    "text": "str",
})
"""The arguments of `fill`."""

BeginPullArgs = TypedDict("BeginPullArgs", {
    "image": "str",
})
"""The arguments of `begin_pull`."""

PollPullArgs = TypedDict("PollPullArgs", {
})
"""The arguments of `poll_pull`."""

CancelPullArgs = TypedDict("CancelPullArgs", {
    "p": "Pull",
})
"""The arguments of `cancel_pull`."""

DismissPullArgs = TypedDict("DismissPullArgs", {
    "p": "Pull",
})
"""The arguments of `dismiss_pull`."""

class Draft(Resource):
    """Draft — `tl.so.tilo.containers.Draft`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    mnoqvwjm: str  # name
    kc5y7sq2: str  # image
    gkz666im: str  # ports
    @property
    def _7khtp7xl(self) -> str:
        """env (`7khtp7xl` on the wire)."""
        return self.fields["7khtp7xl"]

    @_7khtp7xl.setter
    def _7khtp7xl(self, value: str) -> None:
        self.edit("7khtp7xl", value=value)
    utqr7b5l: str  # volumes
    @property
    def _3jkgyx2g(self) -> str:
        """command (`3jkgyx2g` on the wire)."""
        return self.fields["3jkgyx2g"]

    @_3jkgyx2g.setter
    def _3jkgyx2g(self, value: str) -> None:
        self.edit("3jkgyx2g", value=value)


class Pull(Resource):
    """Pull — `tl.so.tilo.containers.Pull`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    shb65p47: str  # image
    gubc3b62: str  # job
    hhqtcu4c: list[str]  # lines
    jgmjny63: bool  # done
    wektgccg: str  # failed


class Containers(Resource):
    """Containers — `tl.so.tilo.containers.Containers`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    svwiioos: Draft  # draft
    ilelnqwj: list[Entry[Pull]]  # pulls

    def clear_create(self) -> None:
        """Clears the new-container form."""
        return self.action("clear_create")

    def clear_create_many(self, items: list[ClearCreateArgs]) -> list[dict[str, Any]]:
        """Runs `clear_create` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_create", items)

    def fill(self,
        *,
        field: str,
        text: str,
    ) -> None:
        """Fills one field of the new-container form, as an agent or a script would: `name`, `image`, `ports`, `env`, `volumes` or `command`."""
        return self.action("fill", field=field, text=text)

    def fill_many(self, items: list[FillArgs]) -> list[dict[str, Any]]:
        """Runs `fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill", items)

    def create_container(self) -> None:
        """Runs an image as a new container from the draft, and clears the form when the engine took it."""
        return self.action("create_container")

    def make_volume(self, *, name: str) -> None:
        """Makes a volume."""
        return self.action("make_volume", name=name)

    def tag_image(self,
        *,
        image: str,
        tag: str,
    ) -> None:
        """Gives an image another reference."""
        return self.action("tag_image", image=image, tag=tag)

    def begin_pull(self, *, image: str) -> None:
        """Starts pulling an image, and keeps its output as it comes."""
        return self.action("begin_pull", image=image)

    def begin_pull_many(self, items: list[BeginPullArgs]) -> list[dict[str, Any]]:
        """Runs `begin_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_pull", items)

    def poll_pull(self) -> None:
        """Asks how far a pull has got; a finished or failed one says so and stays."""
        return self.action("poll_pull")

    def poll_pull_many(self, items: list[PollPullArgs]) -> list[dict[str, Any]]:
        """Runs `poll_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_pull", items)

    def cancel_pull(self, *, p: Pull) -> None:
        """Ends a pull and the engine's process with it."""
        return self.action("cancel_pull", p=p)

    def cancel_pull_many(self, items: list[CancelPullArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_pull", items)

    def dismiss_pull(self, *, p: Pull) -> None:
        """Takes a finished or failed pull out of the window."""
        return self.action("dismiss_pull", p=p)

    def dismiss_pull_many(self, items: list[DismissPullArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_pull", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"clear_create", "clear_create_many", "fill", "fill_many", "create_container", "make_volume", "tag_image", "begin_pull", "begin_pull_many", "poll_pull", "poll_pull_many", "cancel_pull", "cancel_pull_many", "dismiss_pull", "dismiss_pull_many"})


class PullAsk(Resource):
    """PullAsk — `tl.so.tilo.containers.PullAsk`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    tukmkidh: str  # image


class JobAnswer(Resource):
    """JobAnswer — `tl.so.tilo.containers.JobAnswer`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    znalhivd: str  # job


class JobState(Resource):
    """JobState — `tl.so.tilo.containers.JobState`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    h3ie7k7k: str  # state
    bzkmqs4p: list[str]  # lines
    a6sjrj6l: str  # error


class TiloContainers(Resource):
    """Containers — `tl.so.tilo.containers`."""
    __surface__ = "so.tilo.containers@823180d2717f563c"
    def open_shell(self, *, container: str) -> None:
        """Opens a shell in a running container: podman's, in the Tilo terminal."""
        return self.action("open_shell", container=container)

    def enter_box(self, *, box: str) -> None:
        """Opens a shell in a distrobox."""
        return self.action("enter_box", box=box)

    def prompt_run(self,
        *,
        prompt: str,
        typed: str,
    ) -> None:
        """Runs what the prompt asked, once the person pressed its verb."""
        return self.action("prompt_run", prompt=prompt, typed=typed)

    def confirm_run(self, *, confirm: str) -> None:
        """Does what the confirmation named."""
        return self.action("confirm_run", confirm=confirm)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_shell", "enter_box", "prompt_run", "confirm_run"})


