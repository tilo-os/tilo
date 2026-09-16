# Compatibility sandbox validation

## Private temporary storage

Wine and Proton capsule plans create a private Bubblewrap tmpfs at `/tmp`.
Their Landlock rules must also allow that mount to be written. Previously,
the mount existed but writes were denied, preventing runtimes from creating
temporary files and local socket state. The planner now includes the private
mount in its writable rules; it does not bind the host's temporary directory.

The regression test parses the actual generated launcher-shim arguments for
both capsule layers. It requires writable `/tmp`, a private tmpfs rather than
a host bind, active Landlock, and the existing network restriction. It failed
before the fix; all 48 compatibility-service tests passed afterward on ARM.
The native shell integration check also passed with locked dependencies.

## Kernel-level enforcement

Run the reusable probe on the native build architecture:

```sh
cargo build -p launcher --bin qos-launcher --locked
bash os/tests/kernel-landlock-private-tmp.sh \
  /path/to/kernel target/debug/qos-launcher /path/to/new-results
```

The script builds a small initramfs containing the real launcher and a file
access probe. QEMU uses TCG, no network, and no attached disk. The init process
mounts a private tmpfs and runs the launcher with and without the writable rule.
It requires `/tmp` writes to succeed only with the rule, and unrelated writes
to remain denied in both cases. Its init mode refuses to run outside PID 1.

The checked-in probe passed on the built qOS AArch64 7.2.5 kernel. Landlock
reported ABI 10 and both restrictions were fully enforced. The original
policy returned `EACCES` for temporary and unrelated files; the corrected rule
allowed the temporary file while retaining `EACCES` outside it.

Validated SHA-256 values:

- Kernel: `f88ab7ad6eae32417a437ff119c4ed8e8f8ddd5edaca920d82678fab1298ab7a`.
- ARM launcher: `fd7e1f314206395ce9ceebadbb756992a8edd8c7a7565c0b5ae995495f6ec05a`.

This validates Landlock enforcement for the changed permission. It does not
replace complete Wine/FEX launches inside the final capsule sandbox, seccomp
acceptance, or final installer-image testing. WSL's ABI 3 kernel cannot validate
the qOS ABI 5 requirement; that is why this check uses the built qOS kernel.
