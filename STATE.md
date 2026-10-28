# signin-e2e speed work: state

Branch `ci/signin-e2e-speed`, based on `99d149cb84`. Nothing has landed on main. None of these
commits has had a passing signin-e2e-x86_64 run yet: every attempt was stopped by the t outage.

## Commits

- `21bddef134` ci(signin-e2e): the two variants (one-secret, separate-disk-secret) become t
  `shards`, each with its outputs under `signin-e2e/<variant>`. A run of one variant reports only
  the items it owns (`VARIANT_ITEMS`). The install wait blocks in the guest on the installer
  process (`tail --pid`) instead of sleeping 20 s between looks. The power-off wait blocks on
  QEMU's pidfd instead of polling `vm.sh status`.
- `569c14d2b5` ci(installer): dev-installer-medium no longer uses `git = true`, so t caches it by
  the image digest instead of per commit. Its packages are a `setup` layer
  (tools/ci/packages/dev-installer-medium.txt). The installer-only bootc is a new cached check,
  `bootc-alongside-helper-{arch}`; the medium tags that binary as the image
  build-custom-installer.sh looks for, so nothing under os/ changes.
  Regenerated through tools/ci/products.toml.
- `370a0d33d1` ci(signin-e2e): keys, typing and screenshots go over one persistent QEMU monitor
  connection per boot, not a vm.sh plus socat call per key (each such call waited an extra half
  second). Frames are read as PPM; a PNG is written only for the screens kept.
  os/dev/greeter-acceptance.py is added to the check's inputs.
- `64524d66e1` ci(signin-e2e): the installer's output lines are logged with timestamps while the
  install runs.
- `017f321ecc` ci(installer): kmod is added to the medium's package list. It is needed for
  modprobe, and it also gave the setup layer a new key, which got past a hung setup state in t.

## Measured

- Baseline, main run `reec808e65e`: signin-e2e-x86_64 49m18s, dev-installer-medium-x86_64
  22m19s.
  - one-secret: about 30 min (live boot 2 min, unattended install 21 min, four boots about 6 min).
  - separate-disk-secret: about 16 min (install about 10 min, presumably with the medium
    already in page cache; four boots about 4 min).
- The medium: about 1 min dnf, 5–6 min bootc helper build, and two full disk-image passes. The
  first pass exists only to learn the composefs digest.
- bootc-alongside-helper-aarch64 passed in `r85a1b4de8e` (5m24s, plus setup 1m13s).
- t-toml passed in `r4452a6f145`.

## Unproven

- Everything in signin-e2e: the shards, the event waits, the monitor connection and the install
  log.
- The x86 medium step with the cached helper and the setup layer. Runs `r4452a6f145` and
  `r9f4d189af3` hung on the medium's setup layer (a t fault); run `rb33576c640` ended "could not
  run" when the workers went away.

## Next steps

1. Run signin-e2e-x86_64 on this branch and land the commits that pass.
2. Read the timestamped install log. The install (10–21 min) is what keeps a shard above 15 min;
   it needs to come down to about 6 min. Try, in this order: more guest CPUs and memory (the job
   reserves 8 cores, the guest uses 4); an opt-in `cache=unsafe` for the throwaway disks in
   os/dev/vm.sh; unpacking the compressed medium before booting it, if a cold read dominates.
3. Medium step: skip the first disk-image pass by computing the composefs digest directly. That
   changes os/dev/build-custom-installer.sh and disk-image.sh.
