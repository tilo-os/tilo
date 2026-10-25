# Bounded rollback protection

The last few OS generations may unseal the disk. Older ones may not. A downgrade
produces a machine that boots and then asks for the recovery key, rather than a
running system with a known hole in it.

This document covers what an operator needs, and the contract the boot chain has
to satisfy. The reasoning behind the design lives with the code, in
`crates/system/tilo-rollback/src/lib.rs`.

## The two mechanisms, and why neither is enough alone

| | Lives in | Defeated by | Defeats |
|---|---|---|---|
| Retained keyslots | the LUKS2 header | `luksHeaderBackup` before an update, `luksHeaderRestore` after | nothing on its own |
| NV floor counter | the TPM | nothing available to software or to disk surgery | header restore, downgrade, image relabelling |

Unlock requires **both**: the token for the running generation must be in the
header, *and* the NV policy must pass. `crates/system/tilo-rollback/tests/mechanism.rs`
performs the header-restore attack and shows the resurrected token still cannot
unseal.

## When the floor moves

```
bootc upgrade          stage generation N, seal N's keyslot (floor unchanged)
reboot                 boot N; N-1 is still fully able to unseal
+10 min, healthy       tilo-rollback-confirm.timer fires
tilo-rollback confirm   prove N unseals, prune the window, THEN raise the floor
```

Never at install time. An update that boots and then fails never reaches its own
confirmation, so the previous generation keeps working — which is the only reason
an automatic rollback is survivable.

## Advancing the floor needs a signed, single-use approval

The counter is defined with `policywrite` and no `authwrite`, so nothing can
increment it on authority of an empty password. Its `authPolicy` is

```
PolicyOR(
    PolicyAuthorize(floor authority key, policyRef = "tilo-rollback-floor"),
    PolicyNvWritten(clear) -> PolicyCommandCode(TPM2_NV_Increment),
)
```

The second branch materialises the counter on a device that holds no signing key.
It works exactly once: the write it performs sets `TPMA_NV_WRITTEN`, after which
`PolicyNvWritten(clear)` can never be satisfied again.

Every later increment needs an approval, which is a signature over
`PolicyCommandCode(TPM2_NV_Increment) -> PolicyNV(counter == target - 1)`. The TPM
enforces all three properties:

- **single-use** — after the increment the counter is `target`, so the same
  approval no longer applies. Not "we delete the file"; the TPM refuses it.
- **bound to one target** — an approval for `N` cannot reach `N + 5`.
- **bound to one command** — it authorises `TPM2_NV_Increment` and nothing else.

### Issuing approvals

Approvals are per-device, because the operand is that device's absolute counter
value. So it is a request/response exchange, and it works offline over a file:

```bash
# On the device.
tilo-rollback floor-request --device /dev/vda3 --out req.json

# On the authority, which holds the private key.
tilo-rollback approve-floor --private-key keys/tpm2-pcr-signing.private.pem     --request req.json --out-dir approvals/

# Back on the device.
cp approvals/*.json /var/lib/tilo/rollback/approvals/
```

`approve-floor` refuses to issue past the newest generation that has a sealed
keyslot. That bound is what keeps a stash of approvals from becoming a denial of
service: approvals are signatures rather than secrets, so a device holding them can
spend them, and burning the whole batch then lands the floor exactly where a
legitimate confirmation would have put it — with the newest enrolled generation
still able to unseal. Going past it needs
`--i-accept-stranding-this-machine`.

The bound is computed from the device's own request, so a device that lies about
its state can be issued approvals that strand it. A device that lies is already
compromised; the bound exists so that a *later* compromise cannot turn a stash of
approvals into a bricked machine.

### If the authority is unavailable

**The floor stops advancing. The disk keeps unlocking.** Unsealing only reads the
counter, and `TPM2_PolicyNV` needs no approval. Confirmation still prunes the
keyslot window, reports the steps it could not take, and leaves the counter alone;
the protection degrades to the keyslot-retention half rather than failing closed.

Losing the authority key permanently freezes the floor where it is. Recovering
means re-enrolling against a new authority key, which defines a fresh index and
re-seals the tokens, with the recovery key as the unlock path throughout. Nothing
is bricked.

## Operator commands

```bash
# What this TPM's lockout actually permits.
tilo-rollback tpm-status

# Set the dictionary-attack reset authorisation and tighten the parameters.
# Refuses unless you confirm the recovery key exists.
tilo-rollback harden --i-have-a-recovery-key

# First-time enrollment. The recovery key is verified before anything is touched.
tilo-rollback enroll --device /dev/vda3 --recovery-key-file -

# Seal the keyslot for an incoming generation, before rebooting into it.
tilo-rollback enroll --device /dev/vda3 --generation 42 --recovery-key-file -

# A system volume slot that opens by itself, on a boot this machine can prove it
# approved. Refused unless Secure Boot is on, lockdown is active, and db holds only
# the authorities recorded here.
tilo-rollback enroll --device /dev/vda3 --recovery-key-file - --automatic

# Is this boot verified, and would a PIN-less slot open here?
tilo-rollback boot-status

# Where the floor is and which generations it permits.
tilo-rollback status --device /dev/vda3

# A signed statement of the floor, for a peer to verify.
tilo-rollback certify --nonce "$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')" --out-dir /tmp/att
```

The installed system schedules that one-time hardening ceremony five seconds
after boot. It remains early, but TPM probing, LUKS discovery, and key generation
do not hold `multi-user.target` or the first greeter open. Its service conditions
skip later boots once the authorization is durably recorded.

The reset authorization and the lockout parameters belong to the whole TPM, not to
one operating system. Windows sets them itself whenever it starts and finds the
reset authorization empty. If Tilo took them first, Windows could neither
provision its TPM nor reset a lockout; its only remedy, "Clear TPM", also erases
Tilo's sealed keyslots and its floor. So when a firmware boot entry is Windows'
boot manager (`\EFI\Microsoft\Boot\bootmgfw.efi`), `harden` leaves an empty
authorization to Windows and records nothing, and the next boot checks again.
`tpm-status` reports `shared_with_windows`. Until Windows has started once, it
also says a short PIN is not yet defensible. An authorization someone else already
set is never touched, on any machine.

Three things stay shared regardless:
- **Failure counter:** wrong PINs count against both systems.
- **Clearing the TPM:** a clear from either side erases both systems' keys.
- **Firmware updates:** an update Windows installs changes the measurements Tilo
  is sealed to, so Tilo's next boot unlocks with the passphrase or the recovery key.

`--retain N` sets the window (default 3: the current generation plus the two
before it). `--floor-at-least N` on `confirm` jumps the floor past a
known-vulnerable generation instead of leaving it inside the window.

`--floor-authority-key` selects the key that authorises floor advances; it defaults
to the PCR-prediction key because both are the same trust domain. Production
custody should separate them — a prediction signature says "these are the expected
measurements", while a floor approval permanently narrows what a machine will boot.

`TILO_ROLLBACK_DEBUG=1` prints the failing TPM tool's own diagnostics, which is
what you want when a boot could not unseal. It never prints a secret — the PIN and
passphrases are always passed by file reference, never on `argv`.

## A keyslot with no PIN

A PIN is a factor, and typing one on every boot is what the short-PIN design
reduced it to. On a machine that can prove its own boot, the same seal works
without one: `enroll --automatic` seals a generation with no auth value
(`TPM2_NoAuth`), and early userspace opens the volume with nothing typed.

What stops that from being a machine that opens for anybody is not the PIN — it
is the policy. The seal is the same one:

```text
PolicyPCR(prediction)                      PCR 11, the signed UKI measurement
  -> PolicyAuthorize(generation)           only a prediction the authority signed
    -> PolicyNV(counter <= base+generation) the rollback floor still permits it
      -> PolicyAuthValue                    only when a PIN was sealed
```

`PolicyAuthValue` is the one step that is absent, and everything above it is
unchanged, so a PIN-less slot is exactly as strong against a *replayed* image as
a PIN-sealed one: an old generation cannot unseal it, because `PolicyAuthorize`
binds the generation and `PolicyNV` binds the floor. What it is weaker at is the
boot itself, which nothing but the boot can vouch for. Hence three conditions,
all required:

| Condition | Why | Refused as |
|---|---|---|
| Secure Boot on | with it off, PCR 7 still holds a value and a prediction still matches, but nothing checked the boot image | `verified::Unverified::SecureBootOff` |
| `db` holds only authorities this machine trusts | anything else in `db` boots under the same PCR 7 | comparing the recorded digest |
| kernel lockdown active | otherwise early userspace can reach kernel memory and lift the seal from the inside | `verified::Unverified::LockdownInactive` |

The `db` condition is the one that needs a record rather than a reading. A
signature database that gained a Microsoft third-party CA — which signs shims for
drivers and other vendors — admits boots this machine never chose, and PCR 7 alone
cannot tell the two policies apart from a legitimate firmware update. So
enrollment writes the digest of the authorities in `db` (and nothing else: an
image hash can admit one binary and sign nothing) beside the rollback state, and
every unlock compares against it. A `db` that changed since enrollment turns the
quiet unlock into the typed one, which is how the machine asks rather than either
silently trusting a boot it no longer recognises or leaving a person with an
unbootable disk.

The generation is sealed the same way as a PIN-sealed one, with one difference
that is not optional: the PIN-less token is offered *last* in the initramfs,
after any PIN-sealed generation token, so an install that has both does not lose
the stronger one. `tilo-rollback boot-status` prints the whole decision —
Secure Boot state, lockdown level, the `db` digest, the recorded one, and whether a
PIN-less slot would open this boot — without touching a device.

Repair mode is unaffected and stays gated on a typed secret. Its command line
measures differently, so both kinds of generation token refuse it
([repair-mode.md](repair-mode.md), "Why generation-bound unlock is still
stronger").

## Build side

Signing a PCR prediction needs the private key, which is deliberately **not** in
the image:

```bash
tilo-rollback sign-prediction \
    --private-key keys/tpm2-pcr-signing.private.pem \
    --generation 42 \
    --pcr-values predicted-pcrs.bin \
    --out-dir os/pcr-predictions
```

The results do not ship in the image tree: `/usr/share/tilo/pcr-predictions/`
exists but is deliberately empty, because the final composefs-resealed UKI they
measure does not exist until after that layer is built (`os/Containerfile`).
`os/dev/sign-target-prediction.sh` measures that exact UKI and adds the signed
predictions in a `/boot`-only layer (`boot/tilo-target/predictions` and
`boot/EFI/tilo/pcr-predictions`); the installer enrolls from the former and copies
them to `EFI/tilo/pcr-predictions` on the target ESP (`tilo-install`), where
`tilo-deployment` looks for them. A generation
with no prediction cannot be enrolled — the enrollment path refuses rather than
sealing a keyslot that can never open.

Several predictions per generation are allowed and tried in turn, so one image can
cover "before the firmware update" and "after" at once. Replacing a prediction is
a metadata write: the sealed blob is untouched, which is what stops a legitimate
firmware update from turning into a recovery-key prompt.

## What the boot chain has to provide

1. **`tilo.generation=<N>` on the sealed UKI command line.** Inside the signature,
   measured into PCR 11. Not assembled by the bootloader, not read from a file
   under `/boot` — either would make the number attacker-controlled and the whole
   feature decorative.
2. **`N` dense and monotonic**: `+1` per published image. Set at build time via
   the `TILO_GENERATION` build argument, which writes both
   `/usr/lib/bootc/kargs.d/02-generation.toml` — the copy `bootc container ukify`
   seals — and `/usr/lib/tilo/generation`, so a staged deployment's generation can be
   read before it has booted. A timestamp is monotonic and still wrong: the
   retention window is arithmetic on the number, and moving the floor costs one NV
   write per step.

   **Where the number comes from.** The signed published series is its own record:
   every image carries a `gen<N>` tag, transparency-logged signature, and signed
   SLSA provenance binding the image digest to N. `os/dev/next-generation.sh`
   requires a dense history, verifies the highest tag and provenance, and requires
   signed `latest` to name the same digest before returning `N+1`. Registry tag text
   alone is attacker-controlled input. `os/dev/build-image.sh` uses the result and
   the release job promotes `gen<N>` only after signing and attestation, so a number
   is consumed exactly when a verified image becomes reachable.

   This detects unsigned injection, deletion gaps, tag/provenance mismatch and a
   `latest`/generation split. A registry can still freeze a brand-new machine on a
   complete older signed view; an installed machine's TPM floor prevents that view
   from unlocking once its floor advanced. Closing freshness for new installations
   requires an independent signed witness/checkpoint, tracked as a priority gap in
   [threat-model.md](threat-model.md).

   There is no default, and nothing falls back to one when the registry cannot be
   read. A constant is the failure this replaces: it produced images that all
   claimed generation 0, and a floor that can never move is a machine that stops
   unsealing a few updates later, long after the cause.

   A commit count is the tempting wrong answer. It is dense in itself but sparse per
   *image*: the gap between two releases is however many commits landed between
   them, "the last three generations" would then name one image, and advancing the
   floor across such a gap costs that many `TPM2_NV_Increment` calls —
   `MAX_INCREMENTS_PER_ADVANCE` (64) refuses it outright rather than performing
   thousands of NV writes.
3. **Early userspace that calls the unlock helper** before
   `systemd-cryptsetup@<volume>.service`:

   ```
   tilo-rollback unlock --device <luks> --output /run/cryptsetup-keys.d/<volume>.key
   ```

   `systemd-cryptsetup` picks that path up as an implicit key file, so no patched
   systemd, cryptsetup plugin, or keyscript is needed. The same invocation leaves
   the secret in `/run/tilo-rollback/proof.key` for the confirmation later in the
   boot; without it, unattended confirmation cannot authorise a keyslot change,
   because nothing on a rebooted machine knows the PIN.
4. **PCR 7 and 11 measured as usual**, and a prediction for them signed per
   generation.

## Testing it

```bash
# Mechanism, against an emulated TPM and a real LUKS2 header. No root, no VM.
cargo test -p tilo-rollback

# The shell helpers the systemd units use.
./os/tests/rollback-wrappers.sh

# Boot-side, inside a booted guest — the only place the rest can be established.
./os/dev/vm.sh boot --tpm
#   ... then in the guest:
sudo /usr/libexec/tilo-rollback-boot-check
```

The boot-side check is the one that establishes what the host cannot: that the
generation number arrived on the sealed command line, that PCR 7 and 11 hold
meaningful measurements, that early userspace produced the key file before
`systemd-cryptsetup` wanted it, and that the units fired. It passes on a VM booted
from an image `os/dev/build-image.sh` produced. The unlock hand-off is reported
`n/a` on an unencrypted disk and asserted on an enrolled encrypted one. The
initramfs module now calls `tilo-rollback unlock` before the matching cryptsetup
unit and yields to its independent recovery prompt on failure. Signed target
predictions and installer enrollment ship (see [boot-chain.md](boot-chain.md),
"Target-independent enrollment"); the check reports an enrolled volume without a
working hand-off as a failure rather than a skip.

**A check that only ever passes is not evidence**, so there are two ways to make it
fail on purpose:

```bash
# At build time: a UKI that measures normally and carries no generation.
TILO_SEAL_GENERATION=0 ./os/dev/build-image.sh

# On an installed machine: blank the generation out of the UKI in place. Same
# length, so the PE layout is untouched and the boot still measures — into a
# different PCR 11, which is the point.
sudo /usr/libexec/tilo-uki-cmdline show
sudo /usr/libexec/tilo-uki-cmdline blank tilo.generation
```

Either way the check must fail on the generation assertions specifically, not on
"nothing was measured" — those two look identical from inside the guest, and telling
them apart is the only thing that makes a pass mean something.

## The other half: getting off a bad generation

This document is about which generations may *unseal the disk*. What notices that
a generation is bad and gets the machine off it is separate, and lives in
[../recovery.md](../recovery.md): systemd-boot try counters, a health check
before `boot-complete.target`, and a repair-mode boot entry.

The two meet at one point, and it is the reason confirmation is late rather than
at update time. An automatic rollback is only survivable because the floor has
not moved yet — `tilo-rollback-confirm.timer` fires ten minutes in, and an update
that boots and then fails never reaches its own confirmation. So the generation
the boot loader falls back to is still able to unseal.

They also disagree about strictness, deliberately.
`rollback_system_is_healthy` here requires `systemctl is-system-running` =
`running`, because it gates **destroying** the fallback and refusing costs
nothing. `tilo-boot-health` requires only that the session came up (a greeter
that presented a frame and held it without greetd restarting), because it
gates **keeping the current generation**, and the same strictness there would
fail every boot of every generation over one unrelated broken unit and walk the
retention window down to nothing.

## Recovery

The recovery key is an independent path and nothing here ever touches its keyslot.
It is what unlocks the disk when:

- the floor has passed every enrolled generation (a downgrade, or a mis-declared
  floor),
- the firmware changed and no signed prediction covers the new state,
- the TPM was cleared or replaced,
- the PIN was forgotten and the lockout is exhausted,
- the boot stopped being one the machine can prove it approved — Secure Boot
  turned off, lockdown off, or a `db` that changed since enrollment — so a
  PIN-less slot is not tried and the typed secret is asked for,
- the NV index was destroyed, or recreated with a different floor authority key
  (its TPM Name changes, and every token is sealed against the old Name —
  `tilo-rollback enroll` reports this by name rather than letting it surface as an
  unexplained failure to unseal).

The dictionary-attack reset authorisation lives on the encrypted root, so a
locked-out TPM cannot be reset until the disk is open, and opening it needs the
recovery key. The TPM keyslot provides convenience; the recovery key is the
independent recovery path. A user who discards the recovery key and then forgets
their PIN has lost the data, and nothing in this subsystem can change that.
