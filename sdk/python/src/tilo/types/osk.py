"""Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `osk/<resource>`, read with
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
    "Notice",
    "Typing",
    "Taught",
    "SuggestAsk",
    "Offer",
    "Suggested",
    "CorrectAsk",
    "Corrected",
    "SwipeAsk",
    "Candidate",
    "Swiped",
    "TouchAsk",
    "Landed",
    "Touched",
    "AccentsAsk",
    "Accents",
    "Osk",
]

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Typing(Resource):
    """Typing — `tl.so.tilo.osk.Typing`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    m6tp67xz: int  # field
    xgezzsnq: str  # word
    kurpxlhy: str  # previous
    tduplb36: bool  # swiped
    nrfgg7zc: list[str]  # chips
    oli6hbi7: str  # typed
    kydmz4at: str  # tail
    yir2po5h: int  # shift
    @property
    def _64zexfdz(self) -> str:
        """page (`64zexfdz` on the wire)."""
        return self.fields["64zexfdz"]

    @_64zexfdz.setter
    def _64zexfdz(self, value: str) -> None:
        self.edit("64zexfdz", value=value)
    lfpmstnh: str  # query
    @property
    def _2tfifihw(self) -> int:
        """ctrl (`2tfifihw` on the wire)."""
        return self.fields["2tfifihw"]

    @_2tfifihw.setter
    def _2tfifihw(self, value: int) -> None:
        self.edit("2tfifihw", value=value)
    x6mgoh2l: int  # alt
    qbj7o6ze: bool  # sentence
    @property
    def _25ultnxs(self) -> bool:
        """parked (`25ultnxs` on the wire)."""
        return self.fields["25ultnxs"]

    @_25ultnxs.setter
    def _25ultnxs(self, value: bool) -> None:
        self.edit("25ultnxs", value=value)
    @property
    def _3mckgl7e(self) -> list[str]:
        """forms (`3mckgl7e` on the wire)."""
        return self.fields["3mckgl7e"]

    @_3mckgl7e.setter
    def _3mckgl7e(self, value: list[str]) -> None:
        self.edit("3mckgl7e", value=value)
    jf436se6: str  # forms_for


class Taught(Resource):
    """Taught — `tl.so.tilo.osk.Taught`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    ecaq5x2q: str  # context
    yybwwgav: str  # word
    htei6x6l: int  # count


class SuggestAsk(Resource):
    """SuggestAsk — `tl.so.tilo.osk.SuggestAsk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    u7ua3uwj: str  # word
    x64bsvli: str  # previous
    i2l5ongd: str  # language
    xyp2g7uo: list[str]  # personal
    btxnh6dk: bool  # personalization
    lbar2w74: list[Taught]  # learned
    @property
    def _3jxblazz(self) -> list[str]:
        """kept (`3jxblazz` on the wire)."""
        return self.fields["3jxblazz"]

    @_3jxblazz.setter
    def _3jxblazz(self, value: list[str]) -> None:
        self.edit("3jxblazz", value=value)
    oqjnl4cg: int  # limit


class Offer(Resource):
    """Offer — `tl.so.tilo.osk.Offer`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    sspaaf2o: str  # word
    ym3zbzik: float  # score
    o6ytofpt: bool  # personal
    zjsvehiu: bool  # emoji


class Suggested(Resource):
    """Suggested — `tl.so.tilo.osk.Suggested`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    @property
    def _7qgx3bze(self) -> list[Offer]:
        """completions (`7qgx3bze` on the wire)."""
        return self.fields["7qgx3bze"]

    @_7qgx3bze.setter
    def _7qgx3bze(self, value: list[Offer]) -> None:
        self.edit("7qgx3bze", value=value)
    txmmjsyz: list[Offer]  # next


class CorrectAsk(Resource):
    """CorrectAsk — `tl.so.tilo.osk.CorrectAsk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    avrdwkwu: str  # word
    xh7nuttw: str  # language
    rdht4mzn: str  # layout
    ip6lajuh: list[str]  # rows
    nyu5nrfl: list[str]  # kept


class Corrected(Resource):
    """Corrected — `tl.so.tilo.osk.Corrected`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    @property
    def _37fhak2q(self) -> str | None:
        """correction (`37fhak2q` on the wire)."""
        return self.fields["37fhak2q"]

    @_37fhak2q.setter
    def _37fhak2q(self, value: str | None) -> None:
        self.edit("37fhak2q", value=value)


class SwipeAsk(Resource):
    """SwipeAsk — `tl.so.tilo.osk.SwipeAsk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    vkdpd5r7: list[float]  # path
    qhrbdy4q: list[float]  # area
    p6t77uy7: list[str]  # rows
    udzknqo5: str  # layout
    yhplpjdc: str  # language
    pxdf2zfl: str  # shift
    mocqi47p: int  # limit
    a2o5rdfx: bool  # judged
    mvvgppgc: float  # aspect


class Candidate(Resource):
    """Candidate — `tl.so.tilo.osk.Candidate`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    zsr74dix: str  # word
    mybbluwr: float  # score


class Swiped(Resource):
    """Swiped — `tl.so.tilo.osk.Swiped`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    veop4uz2: bool  # swipe
    @property
    def _56adnkcz(self) -> list[Candidate]:
        """candidates (`56adnkcz` on the wire)."""
        return self.fields["56adnkcz"]

    @_56adnkcz.setter
    def _56adnkcz(self, value: list[Candidate]) -> None:
        self.edit("56adnkcz", value=value)


class TouchAsk(Resource):
    """TouchAsk — `tl.so.tilo.osk.TouchAsk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    ilse75gf: list[float]  # point
    ayis4zfh: list[float]  # area
    @property
    def _25lqxg2k(self) -> str:
        """layout (`25lqxg2k` on the wire)."""
        return self.fields["25lqxg2k"]

    @_25lqxg2k.setter
    def _25lqxg2k(self, value: str) -> None:
        self.edit("25lqxg2k", value=value)
    yjrtaqh2: list[str]  # rows
    j7nbu66f: str  # previous
    lb64yv45: str  # word
    @property
    def _6ofwzyyn(self) -> str:
        """language (`6ofwzyyn` on the wire)."""
        return self.fields["6ofwzyyn"]

    @_6ofwzyyn.setter
    def _6ofwzyyn(self, value: str) -> None:
        self.edit("6ofwzyyn", value=value)
    tfzfpcem: list[Any]  # offsets
    xl5fo2at: bool  # personalization


class Landed(Resource):
    """Landed — `tl.so.tilo.osk.Landed`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    gmprye6b: str  # ch
    @property
    def _7e2ewqea(self) -> float:
        """dx (`7e2ewqea` on the wire)."""
        return self.fields["7e2ewqea"]

    @_7e2ewqea.setter
    def _7e2ewqea(self, value: float) -> None:
        self.edit("7e2ewqea", value=value)
    xxbw2p3b: float  # dy
    hidcbudr: int  # samples


class Touched(Resource):
    """Touched — `tl.so.tilo.osk.Touched`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    uk2nkbvx: list[int] | None  # key
    gf3rqhji: Landed | None  # taught


class AccentsAsk(Resource):
    """AccentsAsk — `tl.so.tilo.osk.AccentsAsk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    erau6win: list[str]  # keys
    wbtiabii: str  # language
    @property
    def _6we4suoh(self) -> str:
        """shift (`6we4suoh` on the wire)."""
        return self.fields["6we4suoh"]

    @_6we4suoh.setter
    def _6we4suoh(self, value: str) -> None:
        self.edit("6we4suoh", value=value)


class Accents(Resource):
    """Accents — `tl.so.tilo.osk.Accents`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    cha7zqyr: dict[str, list[str]]  # forms


class Osk(Resource):
    """On-Screen Keyboard — `tl.so.tilo.osk`."""
    __surface__ = "so.tilo.osk@4286fa454a61820c"
    def pressed(self,
        *,
        k: Any,
        typing: Typing,
        index: int,
        at: list[float],
    ) -> Typing:
        """A key of the pad was pressed: `index` counts the keys shown, `at` is where the finger was, as fractions of the pad's box."""
        return self.action("pressed", k=k, typing=typing, index=index, at=at)

    def held(self,
        *,
        k: Any,
        typing: Typing,
        choice: list[str],
    ) -> Typing:
        """A long press chose one of a key's forms."""
        return self.action("held", k=k, typing=typing, choice=choice)

    def meant(self,
        *,
        k: Any,
        t: Typing,
        key: str,
        at: list[float],
    ) -> str:
        """The letter a tap meant. Where this person's taps tend to land and which letter may come next move the edge between two letters, never the keys drawn; what the tap taught of where they land is kept. Only in a field of words, and never a secret's."""
        return self.action("meant", k=k, t=t, key=key, at=at)

    def continues(self,
        *,
        k: Any,
        t: Typing,
        stem: str,
    ) -> bool:
        """Some word of the language goes on past `stem'`: `don'`, `l'`, `aujourd'`."""
        return self.action("continues", k=k, t=t, stem=stem)

    def letter(self,
        *,
        k: Any,
        typing: Typing,
        key: str,
    ) -> Typing:
        """A letter typed: composed into the word where the field takes words, else typed as it is. With Ctrl or Alt held, the key is a chord: pressed, not typed."""
        return self.action("letter", k=k, typing=typing, key=key)

    def press(self,
        *,
        k: Any,
        typing: Typing,
        key: str,
    ) -> Typing:
        """A key that is no letter: one of the keys' own, an editing key, or a mark."""
        return self.action("press", k=k, typing=typing, key=key)

    def commit(self,
        *,
        k: Any,
        typing: Typing,
        after: str,
        correct: bool,
    ) -> Typing:
        """The composing word goes into the field with `after` behind it: corrected first when `correct` asks and the field takes corrections, and learned from. With no word composing, `after` is typed alone."""
        return self.action("commit", k=k, typing=typing, after=after, correct=correct)

    def corrected(self,
        *,
        k: Any,
        t: Typing,
        word: str,
    ) -> str:
        """What the device would correct `word` to, or the word itself."""
        return self.action("corrected", k=k, t=t, word=word)

    def backspace(self,
        *,
        k: Any,
        typing: Typing,
    ) -> Typing:
        """Backspace: right after a correction it types the word back as the person typed it, and keeps it so; in a word it takes its last letter (a swiped word goes whole); elsewhere it reaches the field."""
        return self.action("backspace", k=k, typing=typing)

    def undo_correction(self,
        *,
        k: Any,
        typing: Typing,
    ) -> Typing:
        """The correction just made is taken back: the word goes in again as typed, and is never corrected again."""
        return self.action("undo_correction", k=k, typing=typing)

    def chord(self,
        *,
        k: Any,
        typing: Typing,
        name: str,
    ) -> Typing:
        """A key pressed with Ctrl or Alt held. A modifier latched for one key lets go after it."""
        return self.action("chord", k=k, typing=typing, name=name)

    def caret(self,
        *,
        k: Any,
        typing: Typing,
        steps: int,
    ) -> Typing:
        """The space bar was dragged: the caret moves by `steps` letters, after the word goes in."""
        return self.action("caret", k=k, typing=typing, steps=steps)

    def swiped(self,
        *,
        k: Any,
        typing: Typing,
        path: list[float],
        aspect: float,
    ) -> Typing:
        """A finger crossed the letters: the word it spelled composes, cased for Shift, and the other words it may have meant stand in the bar. A swiped word is never corrected."""
        return self.action("swiped", k=k, typing=typing, path=path, aspect=aspect)

    def offer(self,
        *,
        k: Any,
        typing: Typing,
    ) -> Typing:
        """What the bar offers now: the words the one composing may become, or the ones that may come next, the person's own first, then up to two emoji."""
        return self.action("offer", k=k, typing=typing)

    def took(self,
        *,
        k: Any,
        typing: Typing,
        chip: str,
    ) -> Typing:
        """One of the bar's offers was taken."""
        return self.action("took", k=k, typing=typing, chip=chip)

    def picked(self,
        *,
        k: Any,
        typing: Typing,
        emoji: str,
    ) -> Typing:
        """An emoji from the picker goes in after the word."""
        return self.action("picked", k=k, typing=typing, emoji=emoji)

    def tool(self,
        *,
        k: Any,
        typing: Typing,
        name: str,
    ) -> Typing:
        """One of the strip's commands."""
        return self.action("tool", k=k, typing=typing, name=name)

    def with_forms(self,
        *,
        k: Any,
        typing: Typing,
    ) -> Typing:
        """The forms a long press offers on every letter, asked of the device in the language typed."""
        return self.action("with_forms", k=k, typing=typing)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"pressed", "held", "meant", "continues", "letter", "press", "commit", "corrected", "backspace", "undo_correction", "chord", "caret", "swiped", "offer", "took", "picked", "tool", "with_forms"})


