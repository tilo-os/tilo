# Native import experiment builds

Use `rcargo_source_build.py` for the `good-morning` package, including its native
importer binary. It reads the application owner's worktree without editing it.
Preparation copies only Git-listed native source/build roots into an owned,
stable mirror. Imported JSONL/database files, untracked payload JSON, credential
directories, `.env`, keys, Git metadata, worktrees and targets are excluded.
Symlinks and unexpected mirror files fail before egress. Native source and font
assets currently total about 197 MB; the preparation limit is 256 MiB/16,000
files and requires three times that budget in available disk space. File changes
during preparation fail with the affected source path.

First prepare and review the receipt; this command has no source egress or Cargo:

```sh
python3 tools/dev/rcargo_source_build.py \
  --source /tmp/tilo-good-morning-local-import-20261012 \
  --mirror "$PWD/target/ci-mass-import-build/source" \
  --receipt "$PWD/target/ci-mass-import-build/preparation.json"
```

For a required native build, repeat with `--build` and `RCARGO_IMAGE` set to the
already verified immutable **native** tilo-builder digest. Use the same mirror
path across builds. It runs only `rcargo --no-fallback build --locked -p
good-morning --bins`. Both native executables return; unrelated binary downloads
are disabled. Never set `CARGO_TARGET_DIR` or run local Cargo.

The host-only rcargo config stays outside the upload mirror. It retains the
configured host/user and existing cache bucket references, and omits instance ID,
security group, credentials and elevated container arguments. SSH is forced;
unreachable builders cannot trigger AWS start or firewall recovery. Approval
controls still apply to each source transfer; this tool does not override them.

Receipts record source snapshot identity, byte/file counts, immutable image,
exact package command, exit status, elapsed time and rcargo phase timings. Logs
stay outside the mirror and retain at most 4 MiB. Distinguish a cold worktree
from a cold shared compiler cache; a new mirror does not prove an empty cache.
Application compile failures remain the application owners' responsibility.
