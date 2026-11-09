# Repository policy tooling

This directory owns fast, deterministic checks of repository structure and
cross-cutting contracts. Run the complete local and CI policy suite with:

```sh
tools/policy/run.sh
```

Build selection and execution helpers belong in `tools/ci/`; application
publication tooling belongs in `tools/apps/`.

A check prints its findings on stdout, one per line, `path:line: message` or
`path: message`, and everything else (headings, counts, hints, OK lines) on
stderr.

## A check that judges one file

Most rules are about a file's own text. Such a check takes `--staged`
(`judged.py`), which makes it judge the files a commit stages, read from the
index, instead of every file of the tree: `.githooks/pre-commit` runs those
with it, so a commit reads what it changes rather than the ten thousand files
around it. Without the flag a check judges the whole tree, as `run.sh` and CI
run it, and a violation anywhere is still a failure.

The mode is only sound for a rule whose verdict on a file depends on that file:
a commit cannot introduce a violation in a file it does not stage. A rule that
counts or compares across the tree keeps judging the tree, and says why. Keep
both modes reporting the same thing about the same files:
`tools/tests/test_staged_judgement.py` holds each check to it.

