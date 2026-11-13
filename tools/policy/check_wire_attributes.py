#!/usr/bin/env python3
"""A type a compact format carries writes every field it declares.

postcard and bincode name a struct's field by its place in the bytes and an
enum's variant by its index: nothing in them says which field is which. A serde
attribute that changes which fields a value writes (``skip``,
``skip_serializing``, ``skip_serializing_if``, ``skip_deserializing``,
``flatten``) or how a reader finds its variant (``untagged``, ``tag = …``,
``content = …``) therefore makes the reader take another field's bytes, or the
frame not decode at all. A roster row that left out its absent role shifted its
name, avatar, colour and join time, and every app with someone in its roster
lost its collaboration at start (docs/policies.md).

What it judges is what a compact format carries: every type reachable from the
roots of an ``interface.toml`` whose ``encoding`` is ``postcard`` or ``bincode``
(``check_wire_enums.carried_types``, read with ``check_interfaces.py``, so
"wire type" has one definition in the tree). A type only a JSON interface
carries is exempt, because that file says ``encoding = "json"``: those bytes
name their fields, so leaving one out is what ``skip_serializing_if`` is for. A
type both kinds of interface carry is judged, since the positional reader is
the one that breaks.

``#[serde(default)]`` is not refused: it says what an absent field reads as and
never changes what is written, and a positional format only omits a field at
the end of a frame. Neither are ``rename``/``rename_all`` (a positional format
writes no names), ``with``/``serialize_with`` (which write the field the same
way on both sides), or ``transparent``.

This is an absolute judgement of a tree, not of a change: it runs over the whole
tree in the pre-commit hook and in the repository policy suite, so a commit made
with ``--no-verify`` is judged by the suite on the tree that would land.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_interfaces import read_items, tokenize  # noqa: E402
from check_wire_enums import carried_types  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]

REFUSED_FLAGS = ("flatten", "skip", "skip_deserializing", "skip_serializing", "untagged")
REFUSED_VALUES = ("content", "skip_serializing_if", "tag")

WHY = (
    "postcard and bincode read a struct's fields by position and an enum's variant by its "
    "index, so the reader takes another field's bytes and the frame does not decode; write "
    "the field whole (absent ones as `None`), or carry the shape in a type of its own"
)


def refused(serde) -> list[str]:
    """The serde attributes of `serde` that leave part of a value out of the bytes."""
    return [flag for flag in REFUSED_FLAGS if flag in serde.flags] + [
        name for name in REFUSED_VALUES if name in serde.values
    ]


def placed(item) -> list[tuple[str, object]]:
    """`(where, serde attributes)` for `item` itself, its fields and its variants' fields."""
    places = [(item.name, item.serde)]
    places += [(f"{item.name}.{field.name}", field.serde) for field in item.fields]
    for variant in item.variants:
        places.append((f"{item.name}::{variant.name}", variant.serde))
        places += [
            (f"{item.name}::{variant.name}.{field.name}", field.serde)
            for field in variant.fields
        ]
    return places


def problems(root: Path = ROOT) -> list[str]:
    """What the types `root`'s compact interfaces carry get wrong, one line each."""
    # Resolved here, as `carried_types` does: an interface the tree cannot read is this
    # check's answer too, not an uncaught traceback.
    from check_interfaces import InterfaceError

    try:
        carried = carried_types(root)
    except InterfaceError as error:
        return [f"{error}; the types a compact format carries cannot be read"]
    found = []
    for source, names in sorted(carried.items()):
        items = []
        read_items(tokenize((root / source).read_text()), source, items)
        for item in items:
            if item.name not in names:
                continue
            for where, serde in placed(item):
                names_refused = refused(serde)
                if names_refused:
                    found.append(
                        f"{source}: {where}: #[serde({', '.join(names_refused)})] leaves part "
                        f"of the value out of the bytes; {WHY}"
                    )
    return found


def main(argv: list[str] | None = None) -> int:
    parser_args = list(sys.argv[1:] if argv is None else argv)
    root = Path(parser_args[0]).resolve() if parser_args else ROOT
    found = problems(root)
    for problem in found:
        print(f"::error::{problem}", file=sys.stderr)
    if found:
        print(
            f"{len(found)} serde attribute(s) omit part of a value a compact format carries",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
