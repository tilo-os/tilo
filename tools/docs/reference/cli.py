#!/usr/bin/env python3
"""Write the command-line reference from the `tilo` command itself.

The page is what `tilo help-markdown` prints: the command walks its own clap
tree, the one a computer with everything installed has, so every verb, its
usage, its arguments and its help text come from the definition the parser
reads. A command added, renamed or reworded reaches the page with nothing to
keep in step by hand.

    cli.py --root <repository> --out <page.md>

The command is built and run with `cargo`; `--tilo PATH` runs one already built.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))
import page  # noqa: E402  (beside this script, not on the isolated path)


LEAD = """\
`tilo` is Tilo's command: it opens apps, does what the desktop can do, connects
agents to it, and looks after your devices and your hosted workspaces. This page
is generated from the command's own definition. What `tilo --help` lists is what
works on the computer it runs on; a command for something that computer does not
have answers with one sentence saying what it needs.
"""


def generate(arguments: argparse.Namespace) -> str:
    body = help_markdown(arguments.root.resolve(), arguments.tilo)
    return page.page("Command line", LEAD, [body])


def help_markdown(root: Path, tilo: Path | None) -> str:
    """The sections, as the command prints them for a machine with everything."""
    command = [str(tilo)] if tilo is not None else [
        "cargo", "run", "--locked", "-q", "-p", "tilo-cli", "--bin", "tilo", "--",
    ]
    result = subprocess.run(
        [*command, "help-markdown"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        reason = (result.stderr or result.stdout).strip().splitlines()
        raise page.ReferenceError(
            "the command did not print its help: "
            + (reason[-1] if reason else f"it exited {result.returncode}")
        )
    body = result.stdout.strip()
    if not body.startswith("## "):
        raise page.ReferenceError("the command printed no sections: is it a `tilo` that has help-markdown?")
    return body


if __name__ == "__main__":
    sys.exit(page.main(
        generate,
        __doc__ or "Write the command-line reference.",
        (("--tilo", "a built `tilo` to run, instead of building one with cargo"),),
    ))
