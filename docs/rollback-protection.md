# Bounded rollback protection

The last few OS generations may unseal the disk. Older ones may not. A downgrade
produces a machine that boots and then asks for the recovery key, rather than a
running system with a known hole in it.

This document covers what an operator needs, and the contract the boot chain has
to satisfy. The reasoning behind the design lives with the code, in
`crates/system/qos-rollback/src/lib.rs`.

## The two mechanisms, and why neither is enough alone

| | Lives in | Defeated by | Defeats |
|---|---|---|---|
| Retained keyslots | the LUKS2 header | `luksHeaderBackup` before an update, `luksHeaderRestore` after | nothing on its own |
| NV floor counter | the TPM | nothing available to software or to disk surgery | header restore, downgrade, image relabelling |

Unlock requires **both**: the token for the running generation must be in the
header, *and* the NV policy must pass. `crates/system/qos-rollback/tests/mechanism.rs`
performs the header-restore attack and shows the resurrected token still cannot
unseal.

## When the floor moves

```
bootc upgrade          stage generation N, seal N's keyslot (floor unchanged)
reboot                 boot N; N-1 is still fully able to unseal
+10 min, healthy       qos-rollback-confirm.timer fires
qos-rollback confirm   prove N unseals, prune the window, THEN raise the floor
```

Never at install time. An update that boots and then fails never reaches its own
confirmation, so the previous generation keeps working — which is the only reason
an automatic rollback is survivable.

## Operator commands

```bash
# What this TPM's lockout actually permits.
qos-rollback tpm-status

# Set the dictionary-attack reset authorisation and tighten the parameters.
# Refuses unless you confirm the recovery key exists.
qos-rollback harden --i-have-a-recovery-key

# First-time enrollment. The recovery key is verified before anything is touched.
qos-rollback enroll --device /dev/vda3 --recovery-key-file -

# Seal the keyslot for an incoming generation, before rebooting into it.
qos-rollback enroll --device /dev/vda3 --generation 42 --recovery-key-file -

# Where the floor is and which generations it permits.
qos-rollback status --device /dev/vda3

# A signed statement of the floor, for a peer to verify.
qos-rollback certify --nonce "$(openssl rand -hex 16)" --out-dir /tmp/att
```

`--retain N` sets the window (default 3: the current generation plus the two
before it). `--floor-at-least N` on `confirm` jumps the floor past a
known-vulnerable generation instead of leaving it inside the window.

`QOS_ROLLBACK_DEBUG=1` prints the failing TPM tool's own diagnostics, which is
what you want when a boot could not unseal. It never prints a secret — the PIN and
passphrases are always passed by file reference, never on `argv`.

## Build side

Signing a PCR prediction needs the private key, which is deliberately **not** in
the image:

```bash
qos-rollback sign-prediction \
    --private-key keys/tpm2-pcr-signing.private.pem \
    --generation 42 \
    --pcr-values predicted-pcrs.bin \
    --out-dir os/pcr-predictions
```

The results ship in the image at `/usr/share/qos/pcr-predictions/`. A generation
with no prediction cannot be enrolled — the enrollment path refuses rather than
sealing a keyslot that can never open.

Several predictions per generation are allowed and tried in turn, so one image can
cover "before the firmware update" and "after" at once. Replacing a prediction is
a metadata write: the sealed blob is untouched, which is what stops a legitimate
firmware update from turning into a recovery-key prompt.

## What the boot chain has to provide

1. **`qos.generation=<N>` on the sealed UKI command line.** Inside the signature,
   measured into PCR 11. Not assembled by the bootloader, not read from a file
   under `/boot` — either would make the number attacker-controlled and the whole
   feature decorative.
2. **`N` dense and monotonic**: `+1` per published image. Set at build time via
   the `QOS_GENERATION` build argument, which also writes `/usr/lib/qos/generation`
   so a staged deployment's generation can be read before it has booted. A
   timestamp is monotonic and still wrong: the retention window is arithmetic on
   the number, and moving the floor costs one NV write per step.
3. **Early userspace that calls the unlock helper** before
   `systemd-cryptsetup@<volume>.service`:

   ```
   qos-rollback unlock --device <luks> --output /run/cryptsetup-keys.d/<volume>.key
   ```

   `systemd-cryptsetup` picks that path up as an implicit key file, so no patched
   systemd, cryptsetup plugin, or keyscript is needed. The same invocation leaves
   the secret in `/run/qos-rollback/proof.key` for the confirmation later in the
   boot; without it, unattended confirmation cannot authorise a keyslot change,
   because nothing on a rebooted machine knows the PIN.
4. **PCR 7 and 11 measured as usual**, and a prediction for them signed per
   generation.

## Recovery

The recovery key is an independent path and nothing here ever touches its keyslot.
It is what unlocks the disk when:

- the floor has passed every enrolled generation (a downgrade, or a mis-declared
  floor),
- the firmware changed and no signed prediction covers the new state,
- the TPM was cleared or replaced,
- the PIN was forgotten and the lockout is exhausted.

That last one is worth stating plainly, because it is circular by construction:
the dictionary-attack reset authorisation lives on the encrypted root, so a
locked-out TPM cannot be reset until the disk is open, and opening it needs the
recovery key. That is the design working as intended — the TPM keyslot is the
convenience, the recovery key is the guarantee — but it means a user who discards
the recovery key and then forgets their PIN has lost the data, and nothing in this
subsystem can change that.
