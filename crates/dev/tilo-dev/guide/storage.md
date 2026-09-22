# Storage

Layers accumulate: every build you swap is kept so you can roll back to it.

```sh
tilo-dev status          # layer bytes and free space in the VM
tilo-dev gc --dry-run    # what would go
tilo-dev gc              # remove it
```

`gc` keeps the current generation and the newest `keep_generations` (default
5) of every slot, then deletes layers no kept generation references. Layers
younger than an hour survive, because another agent may be about to swap one.

Limits live in `vms.toml`:

```toml
min_free_gb = 5        # swaps, builds and vm start refuse below this
warn_disk_gb = 80      # status warns when a VM disk file grows past this
keep_generations = 5
```

Build outputs in `target/` are per-worktree and disposable. The shared cache
is sccache; never point two worktrees at one target directory.
