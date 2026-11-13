#!/usr/bin/env python3
"""A failure in session, collaboration and app-host code is heard.

A message from a peer, a stored record, a surface file or a write that fails
is an error: the caller is told, the person is refused or notified, or the
failure is logged with enough context to act on. When it is thrown away
instead, the failure looks like an absent value or a default, and whatever
depended on it silently does nothing -- a shell whose restore failed to open
looked like an empty desk, a join sheet dropped a site it could not read, a
broker's empty delta went out as success.

This check finds, in the production sources of the crates listed in
``SCANNED``:

* a call whose signature returns ``Result`` whose error is discarded:
  ``let _ = fallible(..)``, ``fallible(..).ok();``, and
  ``fallible(..).unwrap_or_default()`` or ``.unwrap_or(..)``;
* the decode and parse errors the check always covered: ``.ok()`` anywhere in
  the chain, ``unwrap_or*``, ``let _ =``, ``if let Ok(..) = decode(..) { .. }``
  with no ``else``, a let-else whose ``else`` neither logs nor returns an
  error, and a ``match decode(..) { .. Err(_) => .. }`` whose error arm
  neither logs nor returns an error;
* an error branch that hears nothing: ``if let Err(..) = .. { .. }`` with no
  ``else``, a ``while let Err``, or a ``match .. { Err(_) => .. }`` whose body
  neither reports (logs, returns, breaks, refuses, answers) nor records the
  failure;
* a failure whose only account is a ``debug!``: an error branch, or the
  closure of an ``inspect_err``/``map_err``, that writes it where nothing
  reads it. A session gives its log to the journal, which takes warning,
  error and information: a ``debug!`` line never reaches it, so the failure
  is silent in a real session.

A failure that is genuinely fine to drop says why in code, in the call that
keeps it::

    let _ = ignored!("the receiver of a broadcast is gone when no desk listens", queue.send(part));

`tilo_failure::ignored!` is the expression it wraps, and its first argument is
the reason: a sentence a reader can check. A bare marker, or a word or two,
leaves the finding where it was. The reason is code the compiler keeps, so
tidying the comments around an expression never takes its reason away; this
check reads the call, never a comment.

What remains is counted per file in ``swallowed_failures_baseline.toml``: a
count may only fall, and a file that sheds some records it (``--update``).
"""

from __future__ import annotations

import re
import sys
import tomllib
from collections import Counter
from dataclasses import replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from judged import STAGED_FLAG, Judged, judged  # noqa: E402
from rust_source import Literal, blank_test_items, is_test_file, lex, matching  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path("tools/policy/swallowed_failures_baseline.toml")
# Where a failure is the person's or a peer's problem when it is dropped: every
# rule below holds here.
SCANNED = (
    Path("crates/collab/tilo-action-broker"),
    Path("crates/collab/tilo-peer-state"),
    Path("crates/collab/collaboration"),
    Path("crates/sdk/tilo-app"),
    Path("crates/session/session"),
    Path("crates/session/capture-studio"),
    Path("crates/session/desktop-search"),
)
# Where only the decode rule holds: the app-hosting code that reads stored
# surfaces and messages.
DECODE_ONLY = (
    Path("crates/platform/tilolang-host"),
    Path("crates/platform/tilolang-tilo"),
    Path("crates/platform/tilo-ui-host"),
    Path("crates/hosting/tilo-room-host"),
    Path("crates/runtime/runtime-portal"),
    Path("apps/tilolang"),
)
# The call a dropped failure names its reason in (`crates/platform/tilo-failure`),
# from its name to the bracket that closes it.
IGNORED = re.compile(r"\bignored\s*!\s*\(")
# The names that call is written by, which report no failure of their own: it
# is the expression it wraps.
REASONS = frozenset({"ignored"})
# The reason has to say why the failure is fine to drop. A call with a word or
# two ("fine", "n/a") is not an account a reader can check, so it leaves the
# finding where it was.
REASON_WORDS = 6

DECODE = re.compile(
    r"(?<![\w.])(?:[\w:]*::)?(?P<name>"
    r"decode\w*|\w+_decode|parse(?:_\w+)?|\w+_parse|from_str|from_slice|from_bytes|from_reader"
    r"|from_value|from_json\w*|from_toml\w*|deserialize\w*|read_message|read_packed_message"
    r"|from_utf8|get_root\w*|from_segments)"
    r"\s*(?:::\s*<[^;{}()]*?>\s*)?\("
    r"|\.(?P<method>parse|decode\w*|deserialize\w*|get_root\w*)\s*(?:::\s*<[^;{}()]*?>\s*)?\("
)
# Writes and reads outside these crates that hand back a `Result` and whose
# error a person or a peer would miss. Deliberately short: what the crates
# themselves define is read from their signatures instead, and a name that is
# usually infallible (a channel `send`, a map `insert`) is not listed.
EXTERNAL_FALLIBLE = frozenset({
    "write_all", "write_all_vectored", "sync_all", "sync_data", "flush", "set_permissions",
    "set_len", "rename", "create_dir_all", "create_dir", "hard_link", "symlink", "copy",
    "read_to_string", "read_exact", "set_read_timeout", "set_write_timeout", "try_clone",
    "finish", "stopped",
})
METHOD = re.compile(r"\s*\??\s*\.\s*(\w+)\s*(?:::\s*<[^;{}()]*?>\s*)?(\()?")
CALL = re.compile(r"(?<![\w.])([A-Za-z_]\w*)\s*(?:::\s*<[^;{}()]*?>\s*)?\(")
METHOD_CALL = re.compile(r"\.\s*([A-Za-z_]\w*)\s*(?:::\s*<[^;{}()]*?>\s*)?\(")
SWALLOWING = {"ok", "unwrap_or_default", "unwrap_or"}
CLOSE = {"(": ")", "[": "]", "}": "}"}
# What an error branch does when it hears the failure. `debug!` and `trace!`
# are not here: a session's journal takes warn, error and info only, so a
# failure logged at debug is one nobody reads.
HEARD = re.compile(
    r"\b(?:log|logging|tracing|eprintln|panic|unreachable|bail|anyhow|Err|reject\w*|report\w*|"
    r"fail\w*|refus\w*|notice\w*|answer\w*|unanswer\w*|unavailable\w*|not_saved|record\w*|"
    r"note\w*|cancel\w*|abandon\w*|withdraw\w*|problem\w*|stop|stop_\w*|break|continue|return|"
    r"send\w*|warn|error|info|status\w*|set\w*|push|mark\w*|raise|"
    r"shed\w*|dropped|gone|\w*_(?:failed|lost|refused|unavailable|exhausted))\b"
)


def hears(body: str) -> bool:
    """Whether an error branch's body tells someone about the failure.

    A bare `return;` hands the caller the absence it already had, so it says
    nothing.
    """
    inner = body.strip()
    if inner.startswith("{") and inner.endswith("}"):
        inner = inner[1:-1]
    if re.fullmatch(r"\s*return\s*(?:\(\s*\))?\s*;?\s*", inner):
        return False
    return bool(HEARD.search(inner))


LOGGED = re.compile(r"tracing::(?:warn|error|info)!|eprintln!|\blog::(?:warn|error|info)!")


def reported(text: str) -> bool:
    """Whether the chain ``text`` logs the failure it then drops."""
    for call in re.finditer(r"\.\s*(?:inspect_err|map_err)\s*\(", text):
        open_paren = text.index("(", call.start())
        if LOGGED.search(text[open_paren : matching(text, open_paren)]):
            return True
    return False


def discarded_name(text: str, fallible: set[str]) -> str | None:
    """The fallible call whose result a statement throws away.

    A method call counts only when its name is one of the writes and reads in
    ``EXTERNAL_FALLIBLE``: `.send(..)`, `.insert(..)` and their like are
    usually the receiver's own answer, not an error. `ignored!` does not count:
    it is the expression it wraps, so the failure it accounts for is still the
    one to find.
    """
    calls = [
        (match.start(), match.group(1), False)
        for match in CALL.finditer(text)
        if match.group(1) not in REASONS
    ]
    calls += [(match.start(), match.group(1), True) for match in METHOD_CALL.finditer(text)]
    if not calls:
        return None
    # The outermost call is the one the statement's value comes from; a name
    # inside its argument is somebody else's answer.
    _, name, method = min(calls)
    if method:
        return name if name in EXTERNAL_FALLIBLE else None
    return name if name in fallible else None


def statement_start(code: str, at: int) -> int:
    """Start of the statement or expression the offset ``at`` sits in."""
    depth = 0
    index = at - 1
    while index >= 0:
        char = code[index]
        if char in ")]}":
            depth += 1
        elif char in "([{":
            if depth == 0:
                return index + 1
            depth -= 1
        elif char == ";" and depth == 0:
            return index + 1
        index -= 1
    return 0


def chain_swallows(code: str, end: int) -> bool:
    """Whether the method chain starting at ``end`` discards the error."""
    while match := METHOD.match(code, end):
        name = match.group(1)
        if match.group(2) is None:
            # A field access or `.await`, the chain goes on.
            end = match.end()
            continue
        close = matching(code, match.end() - 1)
        if name in SWALLOWING:
            # `.ok()?` hands the absence to the caller as its own answer, a
            # contract it decides on; `.ok()` alone drops the failure here.
            if name == "ok" and re.match(r"\s*\?", code[close:]):
                return False
            return True
        if name == "inspect_err":
            # A chain that logs the failure before moving on with a default
            # has reported it.
            body = code[match.end() - 1 : close + 1]
            if re.search(r"tracing::(?:warn|error)!|eprintln!|\blog::(?:warn|error)!", body):
                return False
        if name == "unwrap_or_else" and re.match(r"\(\s*\|\s*_\s*\|", code[match.end() - 1 : close]):
            # A closure that panics or logs is where the failure is reported.
            body = code[match.end() - 1 : close + 1]
            if not re.search(r"\b(?:panic|unreachable|expect|abort)\w*!\s*\(|tracing::(?:warn|error)!", body):
                return True
        end = close
    return False


def converted_later(code: str, at: int) -> bool:
    """Whether the expression around the call turns the absence back into an
    error (`.and_then(|s| s.parse().ok()).ok_or_else(|| invalid(..))?`)."""
    depth = 0
    index = at
    while index < len(code):
        char = code[index]
        if char in "([{":
            if char == "{" and depth <= 0:
                return False
            depth += 1
        elif char in ")]}":
            depth -= 1
            if depth < -4:
                return False
        elif char == ";" and depth <= 0:
            return False
        elif char == "." and depth <= 0 and re.match(r"\.\s*ok_or(?:_else)?\s*\(", code[index:]):
            return True
        index += 1
    return False


def block_after(code: str, at: int) -> tuple[int, int] | None:
    """The block that follows the pattern ending at ``at``.

    A struct literal in the pattern (`if let Err(e) = read(Ask { site })`) has
    braces of its own; the block is the `{` outside every bracket.
    """
    depth = 0
    for index in range(at, len(code)):
        char = code[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "{" and depth <= 0:
            return index, matching(code, index)
    return None


def pattern_swallows(code: str, prefix: str, call_end: int, match_arm: bool = True) -> bool:
    """`if let Ok(..) = decode() { }` with no else, or a let-else that hears nothing."""
    if re.search(r"\b(?:if|while)\s+let\s+Ok\s*\(", prefix):
        block = block_after(code, call_end)
        if block is None:
            return False
        rest = code[block[1] :]
        return not re.match(r"\s*else\b", rest)
    if re.search(r"(?:^|[\s;{}])let\s+Ok\s*\(", prefix):
        tail = re.match(r"[^;{]*?\belse\s*\{", code[call_end:])
        if not tail:
            return False
        brace = call_end + tail.end() - 1
        return not hears(code[brace : matching(code, brace)])
    if match_arm and re.search(r"\bmatch\s*$", prefix):
        block = block_after(code, call_end)
        if block is None:
            return False
        body = code[block[0] + 1 : block[1] - 1]
        for arm in re.finditer(r"\bErr\s*\(\s*(?:_|\.\.)\s*\)\s*(?:if\b[^=]*)?=>", body):
            if not hears(arm_body(body, arm.end())):
                return True
    return False


def arm_body(body: str, at: int) -> str:
    """The expression of the match arm whose `=>` ends at ``at``."""
    depth = 0
    for index in range(at, len(body)):
        char = body[index]
        if char in "([{":
            depth += 1
        elif char in ")]}":
            if depth == 0:
                return body[at:index]
            depth -= 1
            if depth == 0 and char == "}" and not body[at:index].strip().startswith(("Some", "Ok")):
                if body[at:index + 1].lstrip().startswith("{"):
                    return body[at : index + 1]
        elif char == "," and depth == 0:
            return body[at:index]
    return body[at:]


def fallible_names(files: Judged) -> set[str]:
    """The functions these crates define that hand back a `Result`."""
    names: set[str] = set()
    for top in SCANNED:
        for relative, text in files.sources(top, errors="strict"):
            if "target" in relative.parts or "examples" in relative.parts or is_test_file(relative):
                continue
            if "Result" not in text:
                continue
            code, _ = lex(text)
            for signature in re.finditer(r"\bfn\s+([A-Za-z_]\w*)", code):
                end = re.compile(r"[;{]", re.DOTALL).search(code, signature.end())
                if end and "Result" in code[signature.end() : end.start()]:
                    names.add(signature.group(1))
    return names


def judged_names(files: Judged) -> set[str]:
    """Every name the judged files call, used to narrow which files define one."""
    names: set[str] = set()
    for top in SCANNED + DECODE_ONLY:
        for _, text in files.sources(top, errors="strict"):
            names.update(match.group(1) for match in CALL.finditer(text))
            names.update(match.group(1) for match in METHOD_CALL.finditer(text))
    return names


def exemptions(code: str, literals: list[Literal]) -> list[tuple[int, int]]:
    """The spans `ignored!("<why>", <expr>)` covers, each from the call's name to
    the bracket that closes it.

    The reason is the call's first argument: a finding the call sits in, or that
    sits inside it, is the failure the reason accounts for. A call whose reason
    says too little covers nothing.
    """
    spans: list[tuple[int, int]] = []
    for found in IGNORED.finditer(code):
        open_paren = found.end() - 1
        close = matching(code, open_paren)
        reason = next((literal for literal in literals if open_paren < literal.start < close), None)
        if reason is None or len(reason.value.split()) < REASON_WORDS:
            continue
        spans.append((found.start(), close))
    return spans


def covered(spans: list[tuple[int, int]], start: int, end: int) -> bool:
    """Whether the failure reported at ``start``..``end`` is one an `ignored!`
    call accounts for: the call sits in it, or it sits in the call."""
    return any(left < end and start < right for left, right in spans)


def silent_arm(code: str, at: int, found: list[tuple[int, str, int, int]]) -> None:
    """Report an error branch at ``at`` whose body hears nothing."""
    line = code.count("\n", 0, at) + 1
    match = re.match(r"if\s+let\s+Err\s*\(", code[at:])
    if not match:
        match = re.match(r"while\s+let\s+Err\s*\(", code[at:])
    if match:
        block = block_after(code, at + match.end() - 1)
        if block is None:
            return
        rest = code[block[1] :]
        if re.match(r"\s*else\b", rest):
            return
        body = code[block[0] + 1 : block[1] - 1]
        if not hears(body):
            found.append(
                (line, "error branch hears nothing (`if let Err` with an empty body)", at, block[1])
            )
        return
    arm = re.match(r"Err\s*\(\s*(?:_|\.\.)\s*\)\s*(?:if\b[^=]*)?=>", code[at:])
    if not arm:
        return
    body = arm_body(code[at + arm.end() :], 0)
    if hears(body):
        return
    found.append(
        (line, "error branch hears nothing (`Err(_) =>` drops it)", at, at + arm.end() + len(body))
    )


# The logging that a session's journal carries: `debug!` and `trace!` are
# filtered out on the way to it, so they say nothing to whoever reads it.
JOURNAL = re.compile(r"\b(?:warn|error|info)!\s*\(|\beprintln!|\blog::(?:warn|error|info)!")


def debug_only_failure(code: str, found: list[tuple[int, str, int, int]]) -> None:
    """Report an error branch whose only account of the failure is a `debug!`.

    A session gives its log to the journal, which takes warn, error and info:
    a failure written at `debug` is one nobody reads. The branch says so
    differently -- logs it where it is read, tells the person, or names the
    reason with `ignored!` for a failure that is fine to drop.
    """

    def audited(body: str, start: int, end: int) -> None:
        if not re.search(r"\b(?:debug|trace)!\s*\(", body):
            return
        if JOURNAL.search(body):
            return
        found.append(
            (
                code.count("\n", 0, start) + 1,
                "failure logged only at debug (a session journal does not carry it)",
                start,
                end,
            )
        )

    # The closure of `inspect_err`/`map_err`: the failure's one account.
    for call in re.finditer(r"\.\s*(?:inspect_err|map_err)\s*\(", code):
        open_paren = code.index("(", call.start())
        close = matching(code, open_paren)
        audited(code[call.start() : close], call.start(), close)
    # An `Err(..) => ..` arm of a match.
    for arm in re.finditer(r"\bErr\s*\(", code):
        close = matching(code, arm.end() - 1)
        arrow = re.match(r"\s*(?:if\b[^=]*)?=>", code[close + 1 :])
        if not arrow:
            continue
        start = close + 1 + arrow.end()
        body = arm_body(code[start:], 0)
        audited(body, start, start + len(body))
    # The body of `if let Err(..) = ..` and `while let Err(..) = ..`.
    for branch in re.finditer(r"\b(?:if|while)\s+let\s+Err\s*\(", code):
        block = block_after(code, branch.end() - 1)
        if block is None:
            continue
        audited(code[block[0] + 1 : block[1] - 1], block[0], block[1])


def findings_in(text: str, fallible: set[str], failure_shapes: bool = True) -> list[tuple[int, str]]:
    fallible = fallible | EXTERNAL_FALLIBLE
    code, literals = lex(text)
    code = blank_test_items(code)
    found: list[tuple[int, str, int, int]] = []
    if failure_shapes:
        debug_only_failure(code, found)
        for branch in re.finditer(r"\bif\s+let\s+Err\s*\(|\bwhile\s+let\s+Err\s*\(|\bErr\s*\(\s*(?:_|\.\.)\s*\)\s*=>", code):
            silent_arm(code, branch.start(), found)
    for call in DECODE.finditer(code):
        name = call.group("name") or call.group("method")
        before = code[max(0, call.start() - 4) : call.start()]
        if re.search(r"\bfn\s*$", code[max(0, call.start() - 8) : call.start()]):
            continue
        call_end = matching(code, call.end() - 1)
        start = statement_start(code, call.start())
        prefix = code[start : call.start()]
        how = None
        if chain_swallows(code, call_end) and not converted_later(code, call_end):
            how = "error discarded"
        elif re.match(r"\s*let\s+_\s*(?::[^=]*)?=\s*$", prefix) and not before.endswith("."):
            how = "result bound to `_`"
        elif pattern_swallows(code, prefix, call_end, match_arm=not failure_shapes):
            how = "error branch ignored"
        if how is None:
            continue
        line = code.count("\n", 0, call.start()) + 1
        found.append((line, f"{name}: {how}", call.start(), call_end))
    for statement in re.finditer(r"let\s+_\s*(?::[^=]*)?=\s*([^;]*);", code):
        if not failure_shapes:
            continue
        rhs = statement.group(1)
        name = discarded_name(rhs, fallible)
        if name is None:
            continue
        line = code.count("\n", 0, statement.start()) + 1
        found.append((line, f"{name}: result bound to `_`", statement.start(), statement.end()))
    for dropped in re.finditer(r"\.\s*ok\s*\(\s*\)\s*;", code):
        if not failure_shapes:
            continue
        start = statement_start(code, dropped.start())
        # A chain that logs the failure before it is turned into an absent
        # value has reported it.
        if reported(code[start : dropped.start()]):
            continue
        name = discarded_name(code[start : dropped.start()], fallible)
        if name is None:
            continue
        line = code.count("\n", 0, dropped.start()) + 1
        found.append((line, f"{name}: error discarded (`ok()` dropped)", start, dropped.end()))
    for defaulted in re.finditer(r"\.\s*unwrap_or(?:_default|_else)?\s*\(", code):
        if not failure_shapes:
            continue
        start = statement_start(code, defaulted.start())
        name = discarded_name(code[start : defaulted.start()], EXTERNAL_FALLIBLE)
        if name is None:
            continue
        line = code.count("\n", 0, defaulted.start()) + 1
        found.append((line, f"{name}: error turns into a default", start, defaulted.end()))
    # Two rules can meet on one statement (`let _ = Frame::decode(..)` is both a
    # decode discard and a discarded result); that is one failure, not two.
    return list(
        dict.fromkeys(
            (line, message)
            for line, message, start, end in found
            if not covered(exemptions(code, literals), start, end)
        )
    )


def definitions_of(files: Judged) -> Judged:
    """The crates' files that may define a name the judged files call.

    A name the judged files never call cannot change their verdict, so only the
    files that can define one of the names they do call are read. The
    expression is a loose one -- every file holding ``fn <name>``, whatever
    surrounds it -- because it only narrows what is read: the files it keeps are
    parsed by the same rules as ever.
    """
    names = judged_names(files)
    if not names:
        return replace(files, _only=set())
    pattern = "fn[[:space:]]+(" + "|".join(sorted(names)) + ")"
    return files.holding(pattern, *SCANNED)


def findings_of_file(
    relative: Path, text: str, fallible: set[str], failure_shapes: bool
) -> list[str]:
    if "target" in relative.parts or "examples" in relative.parts or is_test_file(relative):
        return []
    return [
        f"{relative.as_posix()}:{line}: swallowed failure ({message})"
        for line, message in findings_in(text, fallible, failure_shapes)
    ]


def audit(root: Path = ROOT, staged: bool = False) -> tuple[list[str], set[Path]]:
    """Every swallowed failure, and the files it was found by judging."""
    files = judged(root, staged)
    scoped = [(top, files.files(top)) for top in SCANNED + DECODE_ONLY]
    judged_here = {relative for _, paths in scoped for relative in paths}
    if not judged_here:
        return [], judged_here
    fallible = fallible_names(files if not staged else definitions_of(files))
    found: list[str] = []
    for top, paths in scoped:
        for relative in paths:
            found += findings_of_file(
                relative, files.read(relative, "strict"), fallible, top in SCANNED
            )
    return found, judged_here


def load_baseline(root: Path) -> dict[str, int]:
    path = root / BASELINE
    if not path.exists():
        return {}
    return {name: int(count) for name, count in tomllib.loads(path.read_text()).get("files", {}).items()}


def write_baseline(root: Path, counts: Counter[str]) -> None:
    lines = [
        "# Failures whose error is discarded, per file, in session, collaboration and",
        "# app-hosting code. Counts may only fall; tools/policy/check_swallowed_failures.py",
        "# --update records a file that shed some. Report the error, or keep it",
        '# with `tilo_failure::ignored!("<why>", <expr>)`.',
        "[files]",
    ]
    lines += [f'"{name}" = {count}' for name, count in sorted(counts.items())]
    (root / BASELINE).write_text("\n".join(lines) + "\n")


def main(argv: list[str]) -> int:
    root = ROOT
    # Recording the baseline counts the whole tree; a commit's files say
    # nothing about the counts of the rest.
    update = "--update" in argv
    staged = STAGED_FLAG in argv and not update
    found, judged_here = audit(root, staged=staged)
    counts = Counter(entry.split(":", 1)[0] for entry in found)
    if update:
        write_baseline(root, counts)
        print(f"recorded {sum(counts.values())} swallowed failure(s)", file=sys.stderr)
        return 0
    if "--list" in argv:
        print("\n".join(found))
        return 0
    baseline = load_baseline(root)
    grown = [entry for entry in found if counts[entry.split(":", 1)[0]] > baseline.get(entry.split(":", 1)[0], 0)]
    # A count can only fall in a file whose code changed, so a baseline entry
    # the judged files do not include keeps the count it was recorded with.
    shed = sorted(
        name
        for name, count in baseline.items()
        if (not staged or Path(name) in judged_here) and counts.get(name, 0) < count
    )
    if grown:
        print("\n".join(grown))
        print(
            "\na failure is discarded: propagate it, refuse or tell the person, or log it at warn "
            "with what failed; a genuinely ignorable one says why with "
            '`tilo_failure::ignored!("<why>", <expr>)`.',
            file=sys.stderr,
        )
        return 1
    if shed:
        for name in shed:
            print(f"{name}: swallowed failures fell below {BASELINE}; record it with "
                  "tools/policy/check_swallowed_failures.py --update")
        return 1
    print(f"Swallowed failures contract OK ({sum(counts.values())} left, none new)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
