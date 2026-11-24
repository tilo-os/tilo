# Repository policy tooling

This directory owns fast, deterministic checks of repository structure and
cross-cutting contracts. `t` carries the suite on a build server, one step per
job, and CI carries it whole as its quality job:

```sh
t test policy            # the whole suite
t test policy/<step>     # one step
```

[`run.sh`](run.sh) is the same steps side by side, for CI. On a development machine
it refuses and names the way to hand the suite to `t`; its `--local` flag takes it
here anyway, for a machine with no server to hand it to. Dozens of checks at once
take the machine they are on, and that work belongs on the server.

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

Three rules judge a set that is fixed or derivable from the change, and take
`--staged` without `judged.py`, because their verdict still depends on the
files themselves: `check_image_interpreters.py` judges the overlay scripts and
installs the commit adds or changes; `check_workspace_members.py` judges the
crate manifests it stages; and `check_session_copy_contract.py` judges the copy
files the commit touches. The whole-tree half of the latter two — a crate no
manifest stages, a copy file left alone — is an inventory the suite keeps.

The rest compare files with each other (app copy resolution, the session loop's
blocking rules, wire attributes, interface versions), so they judge the whole
tree and the hook leaves them to the suite.

`check_catalogue_coverage.py` takes `--staged` through `judged.py` and still
reads more than the files it reports: a key a commit adds to a source catalogue
drifts against the translation of its unit the commit leaves alone, so it reads
each unit the commit stages as the commit leaves it, and HEAD's text of that
unit's other catalogues. No unit the commit does not stage is read.

The same check takes `--range BASE..HEAD`, which the pre-push hook runs over
every push to main: `.githooks/pre-push` judges the pushed range as one change,
from the range's own blobs, because a cherry-pick or a rebase writes commits the
commit hook never runs for. It reads no file on disk.

