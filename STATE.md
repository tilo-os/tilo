# os-acceptance wall time: state

Goal: `os-acceptance-x86_64` and `os-acceptance-aarch64` at 15 min or less each, warm (they took
21–23 min, and up to 31 min). Branch `ci/os-acceptance-shards`, based on 99d149cb84, not rebased
onto current main and not on main.

## Commits

- `74e8efcf1fc` ci: os-acceptance gates run as t shards and fail fast on a silent guest
  - t.toml: `shards = ["desktop", "nearby", "reach", "joined", "splash"]`, with outputs per
    `{shard}`.
  - tools/ci/publish/os-acceptance.sh: runs the shards in `T_SHARDS`. When several share one VM,
    they run side by side as far as its memory holds their guests, longest first. Desktop boots
    alone until it signs in, because its boot and keystroke budgets assume a quiet machine. Each
    shard ends with a `t-shard:` line, and every log line shows the seconds since its shard
    started. Joined uses SSH port 2234, since nearby's third guest takes 2233.
  - os/dev/test-nearby-pairing.sh: guest C boots together with A and B instead of after the
    pairing.
  - os/dev gate scripts: every SSH probe is bounded (`TILO_SSH_BOUND=20`), reach waits for SSH by
    a 300 s deadline, and evidence collection after a failure skips its SSH reads when the guest
    no longer answers.
  - crates/session/shell/tests/native_atspi_smoke.sh: waits end by the clock (120 s, or 360 s for
    pairing completion) instead of after 400 or 1200 attempts.

## Measured (t runs)

- Baseline r79970c2452: main at 99d149cb84, unchanged apart from per-line timestamps.
  - aarch64: the first two boots never reached the greeter within 300 s; failure evidence then
    took about 230 s more, and the device gates ran one after another.
  - Desktop and nearby fail `oobe-finish-focused`, a product regression from b1e83c8de7.
- Sharded rb9450cfdea: an earlier version of the commit, without the bounded probes, the early
  boot of C or the clock-based waits.
  - x86_64, all five shards packed into one VM:

    | Shard | Result |
    |---|---|
    | joined | passed, 1m12s |
    | splash | passed, 2m09s |
    | reach | passed, 4m35s |
    | desktop | failed the boot budget (33.8 s against 20 s), with every shard booting at once; this led to the desktop-first start |
    | nearby | failed `AT-SPI oobe-actions tree never exposed` after a 17 min attempt-counted wait; this led to the clock-based waits |

  - aarch64: joined finished at 117 s and splash at 186 s. Desktop and nearby failed
    `oobe-finish-focused`. Reach hung silent for over 40 min on an unbounded SSH probe.
- Run r7378921a7e has the whole commit and never ran: t lost all its workers.

## Unproven

- Everything in the commit beyond rb9450cfdea's version: the bounded probes, the desktop-first
  start, the early boot of C and the clock-based AT-SPI waits.
- Full desktop and nearby durations. The onboarding regression stops both, so the 15 min target
  isn't shown for either.

## Next steps

1. Rebase onto main and run both checks on whatever runs CI now.
2. Confirm the desktop boot budget passes with desktop starting first, and that failures end
   fast with their evidence.
3. Once the `oobe-finish-focused` fix is in, measure full desktop and nearby twice, warm.
4. If one shard alone is over 15 min, split it. Nearby with its unlock reboots is the likely
   candidate; the split costs an extra pairing setup in the second shard.
5. Land on main after the affected checks pass.

Iteration cost to know about: any change under `os/dev/**` rebuilds os-dev-disk.
