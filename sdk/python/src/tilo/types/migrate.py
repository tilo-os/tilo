"""Types for Migrations (`so.tilo.migrate`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Item` is the app's `tl.so.tilo.migrate.Item` at `migrate/<resource>`, read with
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
    "RunState",
    "Item",
    "Count",
    "Run",
    "Migration",
    "Migrations",
    "Migrate",
    "ForgetItemsArgs",
    "UnreadableArgs",
    "ChooseKeysArgs",
    "BegunArgs",
    "LandedArgs",
    "EndedArgs",
    "ForgotRunArgs",
    "RefusedArgs",
]

RunState = Literal["reviewed", "running", "stopped", "failed", "complete"]
"""Where a migration's run stands: reviewed, with what the source holds waiting to be chosen; running, while the device that holds the data brings in what was chosen; stopped by a person, keeping what it brought until then; failed, its failure saying why; or complete. The device that holds the data moves it on as its run goes."""

ForgetItemsArgs = TypedDict("ForgetItemsArgs", {
    "keep": "list[str]",
})
"""The arguments of `forget_items`."""

UnreadableArgs = TypedDict("UnreadableArgs", {
    "what": "str",
})
"""The arguments of `unreadable`."""

ChooseKeysArgs = TypedDict("ChooseKeysArgs", {
    "keys": "list[str]",
    "chosen": "bool",
    "only": "bool",
})
"""The arguments of `choose_keys`."""

BegunArgs = TypedDict("BegunArgs", {
    "run": "str",
    "title": "str",
})
"""The arguments of `begun`."""

LandedArgs = TypedDict("LandedArgs", {
    "run": "str",
    "keys": "list[str]",
    "kind": "str",
    "count": "int",
    "already": "int",
})
"""The arguments of `landed`."""

EndedArgs = TypedDict("EndedArgs", {
    "state": "RunState",
    "failure": "str",
})
"""The arguments of `ended`."""

ForgotRunArgs = TypedDict("ForgotRunArgs", {
    "run": "str",
})
"""The arguments of `forgot_run`."""

RefusedArgs = TypedDict("RefusedArgs", {
    "why": "str",
})
"""The arguments of `refused`."""

class Item(Resource):
    """Item — `tl.so.tilo.migrate.Item`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"
    z7gblqe6: str  # key
    fvmu6lru: str  # kind
    ove3mkdj: str  # title
    @property
    def _7b7r6oxc(self) -> int:
        """count (`7b7r6oxc` on the wire)."""
        return self.fields["7b7r6oxc"]

    @_7b7r6oxc.setter
    def _7b7r6oxc(self, value: int) -> None:
        self.edit("7b7r6oxc", value=value)
    kz76r36d: str  # counts
    qubfwmfb: str  # within
    ghq4i3f7: bool  # chosen
    @property
    def _2ogrf3gp(self) -> str:
        """note (`2ogrf3gp` on the wire)."""
        return self.fields["2ogrf3gp"]

    @_2ogrf3gp.setter
    def _2ogrf3gp(self, value: str) -> None:
        self.edit("2ogrf3gp", value=value)


class Count(Resource):
    """Count — `tl.so.tilo.migrate.Count`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"
    @property
    def _3cttshux(self) -> str:
        """kind (`3cttshux` on the wire)."""
        return self.fields["3cttshux"]

    @_3cttshux.setter
    def _3cttshux(self, value: str) -> None:
        self.edit("3cttshux", value=value)
    imoqqond: int  # count


class Run(Resource):
    """Run — `tl.so.tilo.migrate.Run`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"
    wphp5gxk: str  # id
    e73pil4f: str  # title
    ifezl4lz: list[str]  # keys


class Migration(Resource):
    """Migration — `tl.so.tilo.migrate.Migration`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"
    jy4y5q35: str  # source
    d66kqgv4: str  # title
    @property
    def _6muhvdtw(self) -> str:
        """device (`6muhvdtw` on the wire)."""
        return self.fields["6muhvdtw"]

    @_6muhvdtw.setter
    def _6muhvdtw(self, value: str) -> None:
        self.edit("6muhvdtw", value=value)
    kxg2fplq: str  # handle
    @property
    def _5nrd34wr(self) -> list[Entry[Item]]:
        """items (`5nrd34wr` on the wire)."""
        return self.fields["5nrd34wr"]

    @_5nrd34wr.setter
    def _5nrd34wr(self, value: list[Entry[Item]]) -> None:
        self.edit("5nrd34wr", value=value)
    dwktqelj: list[str]  # unreadable
    cbtpcpsr: RunState  # state
    @property
    def _4tzslsk2(self) -> str:
        """run (`4tzslsk2` on the wire)."""
        return self.fields["4tzslsk2"]

    @_4tzslsk2.setter
    def _4tzslsk2(self, value: str) -> None:
        self.edit("4tzslsk2", value=value)
    c6gmooon: str  # run_title
    v6notall: str  # failure
    @property
    def _6tvxvli7(self) -> str:
        """refused (`6tvxvli7` on the wire)."""
        return self.fields["6tvxvli7"]

    @_6tvxvli7.setter
    def _6tvxvli7(self, value: str) -> None:
        self.edit("6tvxvli7", value=value)
    wtqlrste: list[Entry[Count]]  # brought
    hl5hwgex: int  # already_there
    g44f5aep: list[str]  # landed
    @property
    def _45kagnwx(self) -> list[Entry[Run]]:
        """runs (`45kagnwx` on the wire)."""
        return self.fields["45kagnwx"]

    @_45kagnwx.setter
    def _45kagnwx(self, value: list[Entry[Run]]) -> None:
        self.edit("45kagnwx", value=value)

    def review_item(self,
        *,
        key: str,
        kind: str,
        title: str,
        count: int,
        counts: str,
        within: str,
        note: str,
        chosen: bool,
    ) -> Item:
        """The review found the item `key`: it is kept as found, and one found before keeps whether a person chose it; a new one is chosen as `chosen` says."""
        return self.action("review_item", key=key, kind=kind, title=title, count=count, counts=counts, within=within, note=note, chosen=chosen)

    def forget_items(self, *, keep: list[str]) -> None:
        """A fresh review no longer found the items whose keys are not in `keep`: they go."""
        return self.action("forget_items", keep=keep)

    def forget_items_many(self, items: list[ForgetItemsArgs]) -> list[dict[str, Any]]:
        """Runs `forget_items` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_items", items)

    def unreadable(self, *, what: str) -> None:
        """The review could not read `what`."""
        return self.action("unreadable", what=what)

    def unreadable_many(self, items: list[UnreadableArgs]) -> list[dict[str, Any]]:
        """Runs `unreadable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unreadable", items)

    def choose(self,
        *,
        i: Item,
        chosen: bool | None = None,
    ) -> None:
        """Chooses an item of `g` to be brought in, or leaves it out: the device that holds the data keeps the choice, and the migration follows it there, as `choose_keys`."""
        args: dict[str, Any] = {}
        args["i"] = i
        if chosen is not None:
            args["chosen"] = chosen
        return self.action("choose", **args)

    def choose_keys(self,
        *,
        keys: list[str],
        chosen: bool,
        only: bool,
    ) -> None:
        """The device chose the items whose keys are `keys`, or left them out; with `only`, every other item is left out."""
        return self.action("choose_keys", keys=keys, chosen=chosen, only=only)

    def choose_keys_many(self, items: list[ChooseKeysArgs]) -> list[dict[str, Any]]:
        """Runs `choose_keys` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_keys", items)

    def begun(self,
        *,
        run: str,
        title: str,
    ) -> None:
        """The device began the run `run`, or resumed it: it is running. A new run starts its counts again; a resumed one goes on counting."""
        return self.action("begun", run=run, title=title)

    def begun_many(self, items: list[BegunArgs]) -> list[dict[str, Any]]:
        """Runs `begun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begun", items)

    def landed(self,
        *,
        run: str,
        keys: list[str],
        kind: str,
        count: int,
        already: int,
    ) -> None:
        """The run `run` brought in the items `keys`: `count` things of `kind`, and found `already` of them in the workspace before."""
        return self.action("landed", run=run, keys=keys, kind=kind, count=count, already=already)

    def landed_many(self, items: list[LandedArgs]) -> list[dict[str, Any]]:
        """Runs `landed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("landed", items)

    def ended(self,
        *,
        state: RunState,
        failure: str,
    ) -> None:
        """The run ended as `state`: stopped, failed with `failure`, or complete."""
        return self.action("ended", state=state, failure=failure)

    def ended_many(self, items: list[EndedArgs]) -> list[dict[str, Any]]:
        """Runs `ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ended", items)

    def forgot_run(self, *, run: str) -> None:
        """The device took out of the workspace what the run `run` brought in: its keys are no longer landed and the run is gone. Undoing the current run puts the migration back to reviewed."""
        return self.action("forgot_run", run=run)

    def forgot_run_many(self, items: list[ForgotRunArgs]) -> list[dict[str, Any]]:
        """Runs `forgot_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forgot_run", items)

    def refused(self, *, why: str) -> None:
        """The device refused a step asked of `g`, for `why`: nothing chosen to bring in, a source it cannot read, a run it cannot take back."""
        return self.action("refused", why=why)

    def refused_many(self, items: list[RefusedArgs]) -> list[dict[str, Any]]:
        """Runs `refused` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refused", items)

    def review(self) -> None:
        """Read `g`'s source again: what it holds now is what there is to choose from."""
        return self.action("review")

    def start(self) -> None:
        """Bring in what is chosen of `g`'s source: the device that holds the data runs it as one run, and the migration follows it there, as `begun`, `landed` and `ended`. The action lasts as long as the run, and stopping it stops the run."""
        return self.action("start")

    def stop(self) -> None:
        """Stop `g`'s run after the item it is on: what it brought stays, and starting again resumes it."""
        return self.action("stop")

    def undo(self, *, r: Run) -> None:
        """Take what the run `r` brought in back out of the workspace."""
        return self.action("undo", r=r)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"review_item", "forget_items", "forget_items_many", "unreadable", "unreadable_many", "choose", "choose_keys", "choose_keys_many", "begun", "begun_many", "landed", "landed_many", "ended", "ended_many", "forgot_run", "forgot_run_many", "refused", "refused_many", "review", "start", "stop", "undo"})


class Migrations(Resource):
    """Migrations — `tl.so.tilo.migrate.Migrations`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"
    l7nahbny: list[Entry[Migration]]  # migrations

    def reviewed(self,
        *,
        source: str,
        title: str,
        device: str,
        handle: str,
    ) -> Migration:
        """The device that reviewed `source` lists it: the migration for that source on that device, made the first time, with its title and the handle the review read. What it could not read is cleared; the review's items follow with `review_item`."""
        return self.action("reviewed", source=source, title=title, device=device, handle=handle)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reviewed"})


class Migrate(Resource):
    """Migrations — `tl.so.tilo.migrate`."""
    __surface__ = "so.tilo.migrate@bc4ea68fea10a391"

