# Source-only remote build preparation

`remote_source_prepare.py` reads a Rust workspace without modifying it and
prepares one stable owned mirror for any remote cargo drop-in. It copies an
explicit Git-listed native source/build allowlist; imported JSONL/database
files, untracked payload JSON, credential directories, `.env`, keys, Git
metadata, worktrees and targets are excluded. Symlinks, concurrent changes
and unexpected mirror files fail before egress.

Preparation has a 256 MiB/16,000-file limit and requires three times that
budget in available disk space. Native source and font assets can require
about197MB. The receipt records snapshot identity, byte/file counts and the
fact that no build or upload ran.

```sh
python3 tools/dev/remote_source_prepare.py \
  --source /path/to/owner/worktree \
  --mirror "$PWD/target/native-source-build/source" \
  --receipt "$PWD/target/native-source-build/preparation.json"
```

Review that receipt before submitting the mirror through the existing,
authorized remote build tool. Keep the mirror path stable across builds,
select the required package/binaries with `--locked`, disable local fallback,
and use the already verified immutable native build image. Never set
`CARGO_TARGET_DIR`. Tool-specific configuration and credentials remain
outside the mirror and outside tracked repository files. Do not grant
permissions, start a builder or relax its network to make a build succeed.

Retain separate source sync, admission, compile and artifact-return timings
in owned bounded logs. Distinguish a cold worktree from a cold shared compiler
cache; a new mirror does not establish an empty cache. A failed application
build is not a successful cold/warm qualification. Preparation preserves the
owner's dirty and untracked source edits but does not change their app code.
