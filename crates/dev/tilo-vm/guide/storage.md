# Storage

Layers accumulate: every build you swap is kept so you can roll back to it.
In-VM builds also keep one build dir per worktree.

```sh
tilo-vm status              # free space, layer bytes, build dir bytes
tilo-vm gc --dry-run        # what would go, and what stays and why
tilo-vm gc                  # remove it
tilo-vm gc --keep-targets 0 # also evict every build dir not in use
tilo-vm gc --min-age 0      # also unreferenced layers younger than an hour
```

`gc` keeps the current generation and the newest `keep_generations` (default
5) of every slot, then deletes layers no kept generation references. Layers
younger than an hour survive, because another agent may be about to swap one;
gc lists each one it kept that way, with its component and when it becomes
eligible. `--min-age <age>` (`0`, `30m`, `1h`, `2d`) changes that limit for
one run: `--min-age 0` clears your own orphaned layers (a build you never
swapped, a slot you removed) at once, but only use it when no other agent is
mid-swap on this VM.

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
of its cached disk, so keep that file while the VM exists. `tilo-vm vm rm <vm>`
deletes a VM with its disk, UEFI variables, logs and key, and keeps the cached
disk. Images imported from a network path (`tilo-vm guide platforms`) are
cached in `~/.cache/tilo-vm/imports/`; delete one once no VM is made from it.

`tilo-vm vm upgrade --from local` keeps one uncompressed copy of the VM's image
on the host (`~/.cache/tilo-vm/images/<vm>/`) and one in the VM
(`/var/lib/tilo-vm/image`), so the next upgrade sends only new layers. Either
can be deleted; the next upgrade then sends the whole image once
(`tilo-vm guide images`). On a Tilo OS VM or device, `tilo-cache-gc.timer` deletes the
in-VM copy once no upgrade has used it for 14 days (the `[vm_image]` policy in
`/etc/tilo/cache-gc.toml`), never while an upgrade holds `image.lock` or
`vm.lock`; `tilo os status` says when it went or when it will.
