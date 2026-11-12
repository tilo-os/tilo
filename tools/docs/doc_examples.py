#!/usr/bin/env python3
"""Check and run the code examples of the developer documentation at tilo.sh/docs.

`docs/tilo-sh/pages.toml` lists the pages published there (the manifest
tools/docs/developer_docs.py reads), and its `[examples].languages` names the
languages every example has to be written in. A runnable example is a fenced
code block whose info string is `<lang> example=<id>`. Consecutive blocks with
one id are that example's tabs, and each of them is the whole program its
language needs: a Python program importing `tilo`, a TypeScript module
importing `tilo-sdk`, a shell program running the `tilo` command. A block
whose info string says `fragment` is a partial snippet and is not run.

`check` enforces the conventions and runs nothing. `list` prints what the pages
carry. `run` executes every block against a local Tilo: `--door` names the
`door-on-relay` example of `tilo-agent-participant`, which serves a host with
its API and runs the program with `TILO_DOOR` and `TILO_API_KEY` set, so an
example reaches the API the way the published one reaches tilo.sh. Python runs
from `sdk/python` with uv, TypeScript resolves `tilo-sdk` to the tree's
`sdk/typescript` and runs with bun, and `sh` runs the `tilo` command `--tilo`
names.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import tomllib
from collections.abc import Iterable, Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "docs/tilo-sh/pages.toml"
# The languages the convention marks examples with. `curl` is one the pages may
# use once there is an HTTP endpoint to show; see `command_for`.
LANGUAGES = ("python", "typescript", "sh", "curl")
# The languages a block is a whole program in: one without an example id and
# without `fragment` is a program nobody runs, so it is a problem.
PROGRAMS = ("python", "typescript", "curl")
FILENAMES = {"python": "example.py", "typescript": "example.ts", "sh": "example.sh"}
TYPESCRIPT_CONFIG = "tsconfig.json"


def _load_developer_docs() -> ModuleType:
    """tools/docs/developer_docs.py, which owns the manifest and the fence grammar."""
    spec = importlib.util.spec_from_file_location(
        "developer_docs", Path(__file__).with_name("developer_docs.py")
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot read tools/docs/developer_docs.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


developer_docs = _load_developer_docs()
DocsError = developer_docs.DocsError


@dataclass(frozen=True)
class Block:
    """One fenced block: a tab of an example, or a snippet beside one."""

    page: str
    line: int
    lang: str
    example: str | None
    fragment: bool
    code: str

    @property
    def location(self) -> str:
        return f"{self.page}:{self.line}"


@dataclass(frozen=True)
class Example:
    """The blocks of one example, one per language: its tabs."""

    id: str
    blocks: tuple[Block, ...]

    @property
    def location(self) -> str:
        return self.blocks[0].location

    @property
    def languages(self) -> tuple[str, ...]:
        return tuple(block.lang for block in self.blocks)


@dataclass(frozen=True)
class Book:
    """The examples the pages of one book carry, and what is wrong with them."""

    root: Path
    manifest_path: str
    languages: tuple[str, ...]
    examples: tuple[Example, ...]
    problems: tuple[str, ...]

    @property
    def blocks(self) -> tuple[Block, ...]:
        return tuple(block for example in self.examples for block in example.blocks)


def closing_fence(line: str) -> bool:
    """Whether `line` is a fence with nothing after it, closing the block it ends."""
    match = developer_docs.FENCE.match(line)
    return match is not None and not line[match.end():].strip()


def fenced_runs(markdown: str) -> Iterator[list[tuple[int, str]]]:
    """The lines of each fenced block, its own fences included, as (line index, line)."""
    run: list[tuple[int, str]] = []
    for index, line, in_code in developer_docs.prose_lines(markdown):
        if in_code:
            run.append((index, line))
        elif run:
            yield run
            run = []
    if run:
        yield run


def blocks_of(page: str, markdown: str) -> Iterator[Block]:
    for run in fenced_runs(markdown):
        opening = developer_docs.FENCE.match(run[0][1])
        assert opening is not None
        words = run[0][1][opening.end():].split()
        lang = words[0] if words else ""
        attributes = words[1:]
        ids = [word.partition("=")[2] for word in attributes if word.partition("=")[0] == "example"]
        lines = run[1:-1] if len(run) > 1 and closing_fence(run[-1][1]) else run[1:]
        yield Block(
            page=page,
            line=run[0][0] + 1,
            lang=lang,
            example=ids[0] if ids else None,
            fragment="fragment" in attributes,
            code="\n".join(line for _, line in lines),
        )


def groups_of(blocks: Sequence[Block]) -> Iterator[list[Block]]:
    """Consecutive blocks sharing one example id, which are that example's tabs."""
    current: list[Block] = []
    for block in blocks:
        if block.example is None:
            if current:
                yield current
                current = []
            continue
        if current and block.example != current[0].example:
            yield current
            current = []
        current.append(block)
    if current:
        yield current


def block_problems(block: Block) -> list[str]:
    """What one block's info string breaks, whichever example it belongs to."""
    if "=" in block.lang:
        return [f"{block.location}: a block names its language first, then example=<id>"]
    if block.example is not None:
        if not block.example:
            return [f"{block.location}: example= names no id"]
        if block.fragment:
            return [f"{block.location}: a block is an example or a fragment, not both"]
        if block.lang not in LANGUAGES:
            return [
                f"{block.location}: example '{block.example}' is written in unknown language "
                f"'{block.lang}' ({', '.join(LANGUAGES)})"
            ]
        return []
    if block.lang in PROGRAMS and not block.fragment:
        return [
            f"{block.location}: a {block.lang} block is a program, so it needs example=<id>; "
            "mark it fragment when it is only part of one"
        ]
    return []


def example_problems(example: Example, languages: Iterable[str], firsts: dict[str, str]) -> list[str]:
    """What one example's tabs break: its id and the languages it has to have."""
    problems = []
    if not developer_docs.SLUG.fullmatch(example.id):
        problems.append(f"{example.location}: example '{example.id}' is not lowercase kebab case")
    if example.id in firsts:
        problems.append(
            f"{example.location}: example '{example.id}' starts again here; its blocks are one group "
            f"and its id is unique across the pages (first at {firsts[example.id]})"
        )
    seen: list[str] = []
    for block in example.blocks:
        if block.lang in seen:
            problems.append(f"{block.location}: example '{example.id}' has two {block.lang} blocks")
        seen.append(block.lang)
    missing = [lang for lang in languages if lang not in seen]
    if missing:
        problems.append(
            f"{example.location}: example '{example.id}' has no {', '.join(missing)} block, "
            "which [examples].languages requires"
        )
    return problems


def example_languages(text: str, manifest_path: str) -> tuple[str, ...]:
    """The languages every example must have, as the book's `[examples]` names them."""
    try:
        data = tomllib.loads(text)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise DocsError(f"{manifest_path}: {error}") from error
    declared = data.get("examples", {}).get("languages", [])
    if not isinstance(declared, list) or not all(isinstance(lang, str) for lang in declared):
        raise DocsError(f"{manifest_path}: [examples].languages must be a list of languages")
    return tuple(declared)


def load(root: Path, manifest_path: str = MANIFEST) -> Book:
    """Every example of the book's pages, and every problem the conventions find."""
    if not (root / manifest_path).exists():
        raise DocsError(f"{manifest_path} does not exist")
    manifest = developer_docs.load_manifest(root, manifest_path)
    tree = developer_docs.SourceTree(root)
    languages = example_languages(tree.read_text(manifest_path), manifest_path)
    examples: list[Example] = []
    problems: list[str] = []
    firsts: dict[str, str] = {}
    for page in manifest.pages:
        if page.generator:
            # Written from code at build time; its generator marks every
            # signature it shows a fragment.
            continue
        if not tree.exists(page.source):
            problems.append(f"{page.source}: does not exist")
            continue
        blocks = list(blocks_of(page.source, tree.read_text(page.source)))
        for block in blocks:
            problems.extend(block_problems(block))
        for group in groups_of(blocks):
            example = Example(id=group[0].example or "", blocks=tuple(group))
            problems.extend(example_problems(example, languages, firsts))
            firsts.setdefault(example.id, example.location)
            examples.append(example)
    return Book(
        root=root,
        manifest_path=manifest_path,
        languages=languages,
        examples=tuple(examples),
        problems=tuple(problems),
    )


def typescript_configuration(repository: Path) -> str:
    """A tsconfig.json resolving the TypeScript SDK's package, by the name its package.json
    publishes it under, to this checkout's own sources."""
    package = repository / "sdk/typescript"
    name = json.loads((package / "package.json").read_text(encoding="utf-8"))["name"]
    sources = package / "src"
    paths = {
        name: [str(sources / "index.ts")],
        f"{name}/types": [str(sources / "types/index.ts")],
    }
    return json.dumps({"compilerOptions": {"paths": paths}}, indent=2) + "\n"


def command_for(block: Block, repository: Path, tilo: Path | None) -> list[str]:
    """The program that runs one block, as a reader would run what the block shows.

    The SDKs are this checkout's, whatever book the examples come from: an example is run against
    the SDK in the tree the check belongs to, not the one beside the page.
    """
    if block.lang == "python":
        return [
            "uv", "run", "--frozen", "--offline", "--project", str(repository / "sdk/python"),
            "python", FILENAMES["python"],
        ]
    if block.lang == "typescript":
        return ["bun", "run", FILENAMES["typescript"]]
    if block.lang == "sh":
        if tilo is None or not tilo.exists():
            raise DocsError(
                f"{block.location}: a sh example runs the tilo command; --tilo names one "
                "(cargo build -p tilo-cli)"
            )
        return ["bash", "-euo", "pipefail", FILENAMES["sh"]]
    if block.lang == "curl":
        raise DocsError(
            f"{block.location}: curl examples cannot run yet; this check runs python, typescript and sh"
        )
    raise DocsError(f"{block.location}: cannot run a {block.lang} block")


@dataclass
class Outcome:
    ok: bool
    output: str
    seconds: float


def execute(block: Block, repository: Path, door: Path, tilo: Path | None, timeout: float) -> Outcome:
    """Run one block with a door of its own, so it sees a fresh Tilo and a key of its own."""
    command = command_for(block, repository, tilo)
    with tempfile.TemporaryDirectory(prefix=f"docs-example-{block.example or block.lang}.") as scratch:
        directory = Path(scratch)
        (directory / FILENAMES[block.lang]).write_text(block.code + "\n", encoding="utf-8")
        if block.lang == "typescript":
            (directory / TYPESCRIPT_CONFIG).write_text(typescript_configuration(repository), encoding="utf-8")
        environment = dict(os.environ)
        if block.lang == "sh":
            # `tilo` on PATH, as the command is installed.
            assert tilo is not None
            environment["PATH"] = f"{tilo.parent}{os.pathsep}{environment.get('PATH', '')}"
        started = time.monotonic()
        try:
            process = subprocess.Popen(
                [str(door), "--", *command],
                cwd=directory,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                errors="replace",
                start_new_session=True,
            )
        except OSError as error:
            raise DocsError(f"{block.location}: cannot start {command[0]}: {error}") from error
        try:
            output = process.communicate(timeout=timeout)[0]
            ok = process.returncode == 0
        except subprocess.TimeoutExpired:
            # The door runs the program in a process of its own: end the group, not the door alone.
            os.killpg(process.pid, signal.SIGKILL)
            output = process.communicate()[0] + f"\ntimed out after {timeout:g}s\n"
            ok = False
        return Outcome(ok=ok, output=output, seconds=time.monotonic() - started)


def print_examples(book: Book) -> None:
    for example in book.examples:
        print(f"{example.id}  {example.location}  {', '.join(example.languages)}")
    print(f"{len(book.examples)} examples, {len(book.blocks)} blocks")


def report(problems: Sequence[str]) -> int:
    for problem in problems:
        print(f"docs examples: {problem}", file=sys.stderr)
    print(f"FAIL: {len(problems)} problem(s) in the examples", file=sys.stderr)
    return 1


def check(book: Book) -> int:
    if book.problems:
        return report(book.problems)
    print(f"docs examples OK ({len(book.examples)} examples, {len(book.blocks)} blocks)")
    return 0


def run(book: Book, door: Path, tilo: Path | None, only: Sequence[str], timeout: float) -> int:
    """Run every selected block, naming each by its example, language and place in the page."""
    if book.problems:
        return report(book.problems)
    if not door.exists():
        raise DocsError(f"{door}: no door to serve the examples with")
    selected = book.examples
    if only:
        known = {example.id for example in book.examples}
        unknown = sorted(set(only) - known)
        if unknown:
            raise DocsError(f"--only {', '.join(unknown)}: no such example")
        selected = [example for example in book.examples if example.id in known.intersection(only)]
    passed = failed = 0
    failures: list[str] = []
    for example in selected:
        for block in example.blocks:
            try:
                outcome = execute(block, ROOT, door, tilo, timeout)
            except DocsError as error:
                outcome = Outcome(ok=False, output=f"{error}\n", seconds=0.0)
            state = "ok  " if outcome.ok else "FAIL"
            print(f"{state} {example.id} [{block.lang}] {block.location} ({outcome.seconds:.1f}s)")
            sys.stdout.flush()
            if outcome.ok:
                passed += 1
                continue
            failed += 1
            failures.append(f"{example.id} [{block.lang}] {block.location}")
            print(textwrap.indent(outcome.output.strip(), "     "))
    print(f"docs examples: {passed} passed, {failed} failed")
    for failure in failures:
        print(f"failed: {failure}", file=sys.stderr)
    return 1 if failed else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--manifest", default=MANIFEST, help="the book whose pages carry the examples")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="print every example and the languages it has")
    commands.add_parser("check", help="fail on an example that breaks the conventions")
    running = commands.add_parser("run", help="run every example against a local Tilo")
    running.add_argument(
        "--door", type=Path, required=True,
        help="the door-on-relay example: it serves a host with its API and sets TILO_DOOR and TILO_API_KEY",
    )
    running.add_argument("--tilo", type=Path, help="the tilo command sh examples run (cargo build -p tilo-cli)")
    running.add_argument("--only", action="append", default=[], metavar="ID", help="run one example (repeatable)")
    running.add_argument("--timeout", type=float, default=180.0, help="seconds one block may take (180)")
    args = parser.parse_args(argv)
    try:
        book = load(args.root.resolve(), args.manifest)
        if args.command == "list":
            print_examples(book)
            return 0
        if args.command == "check":
            return check(book)
        return run(book, args.door.resolve(), args.tilo.resolve() if args.tilo else None, args.only, args.timeout)
    except DocsError as error:
        print(f"docs examples: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
