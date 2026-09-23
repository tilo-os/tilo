# Storage

Layers accumulate: every build you swap is kept so you can roll back to it.
In-VM builds also keep one build dir per worktree.

```sh
tilo-vm status              # free space, layer bytes, build dir bytes
tilo-vm gc --dry-run        # what would go
tilo-vm gc                  # remove it
tilo-vm gc --keep-targets 0 # also evict every build dir not in use
```

`gc` keeps the current generation and the newest `keep_generations` (default
5) of every slot, then deletes layers no kept generation references. Layers
younger than an hour survive, because another agent may be about to swap one.

Build dirs (`/var/lib/tilo-vm/src/<worktree>/target`) are evicted least
recently used first: all but the newest `keep_targets` (default 2), and more
while the VM is below `min_free_gb`. A build dir mid-build is never evicted.
The next build of an evicted worktree starts cold but reuses the shared
sccache. Never point two worktrees at one target directory.

Limits live in `vms.toml`:

```toml
min_free_gb = 5        # swaps, builds and vm start refuse below this
warn_disk_gb = 80      # doctor warns when a VM disk file grows past this
keep_generations = 5
keep_targets = 2
```

Downloaded dev disks are cached in the platform cache dir
(`~/.cache/tilo-vm/disks/` on Linux). A VM's disk is a copy-on-write child
of its cached disk, so keep that file while the VM exists.
