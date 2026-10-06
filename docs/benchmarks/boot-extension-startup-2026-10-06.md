# Offline extension activation startup

The production `tilo-extension activate` command improved from **77.98 ms to
48.21 ms median (38.2%)** across 40 native processes per version. Nearest-rank
p95 improved from **117.31 ms to 83.41 ms**. Every command exited successfully
with empty stdout/stderr. [All samples and boot observations](boot-extension-startup-2026-10-06.json)
are retained, including a rejected networking experiment.

## Change and security behavior

Boot activation loaded `tarfile` and constructed the HTTP opener even when no
extension was enabled. This unnecessarily imported the download/TLS stack.
The helper now imports archive support during RPM extraction and constructs a
cached HTTP opener on the first download. [Python's opener API](https://docs.python.org/3/library/urllib.request.html#urllib.request.build_opener)
and [cache API](https://docs.python.org/3/library/functools.html#functools.cache)
provide the existing handlers and subsequent reuse.

Download timeouts, range headers, HTTPS-only redirects, TLS verification,
publisher allowlists, signed-manifest verification and artifact hashes retain
their existing behavior. Activation still executes normally before greetd;
no verification or extension work was moved past login. Networking and firewall
ordering are unchanged.

## Matched full boots

Both baseline and script-only candidate disks were assembled through the same
`tilo-vm image disk` tooling, then cold booted with fresh private NVRAM and COW
disks. Run order was baseline, candidate, candidate, baseline, baseline, candidate.
The endpoint was the production greeter console reporting `accepts_input=true`,
measured on the host from invoking VM start. Console polling and request timeout
were both 250 ms. Read-only guest measurements followed readiness.

| Run | Variant | Helper execution, seconds | Start to input-ready, seconds |
| --- | --- | ---: | ---: |
| 0 | Baseline | 5.250 | 83.478 |
| 1 | Candidate | 4.368 | 78.615 |
| 2 | Candidate | 4.709 | 74.504 |
| 3 | Baseline | 5.472 | 86.371 |
| 4 | Baseline | 5.112 | 81.388 |
| 5 | Candidate | 3.807 | 78.449 |
| Median | Baseline | 5.250 | 83.478 |
| Median | Candidate | 4.368 | 78.449 |

Median activation CPU time also fell from 1.730 to 1.294 seconds. Every activation
exited zero before greetd started. Every boot had active networking and firewall,
with the firewall active before NetworkManager started, no failed system units,
and no observable ordering cycles.

These are **TCG software-emulation results**, four vCPUs, 4096 MiB RAM and a
1280x720 virtio display, with neither KVM nor a physical GPU. Shared host caches
were not flushed. Unchanged kernel/initrd timing varied substantially between
runs, so the observed 5.03-second readiness difference is not a causal estimate
of the import change, nor a prediction for hardware. The reliable native result
is approximately 30 ms saved in this boot stage.

The pinned developer image contains native binaries from
`12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`, held identical in both variants.
The helper baseline matches the source at
`88a46ec509a3acb3a572c3f2e2c18af28f175000`. This is not a current-main, signed
shipping-image or physical-device boot comparison. Image digests, helper hashes
and assembly-tool provenance are in the raw JSON. No device was rebooted or
updated for this experiment.

## Reproduction

Build two container images from the same pinned developer base, copying only
`os/usr/libexec/tilo-extension` into the candidate at `/usr/libexec/tilo-extension`
with mode 0755. The baseline digest is
`registry.tilo.so/tilo/tilo-vm@sha256:6487357d669c2a63392505ac13a214054047e6461c5dc2b5cd795c438e3f3c59`.
Run this Python body inside each image with `podman run --read-only --network=none`;
container startup is outside the timed interval:

```python
import json, pathlib, subprocess, time
assert not any(pathlib.Path('/var/lib/tilo/extensions').glob('*/enabled'))
samples = []
for _ in range(40):
    start = time.perf_counter_ns()
    child = subprocess.run(['/usr/libexec/tilo-extension', 'activate'],
                           capture_output=True, check=True)
    assert child.stdout == b'' and child.stderr == b''
    samples.append((time.perf_counter_ns() - start) / 1e6)
print(json.dumps(samples))
```

The native measurements ran baseline first, then candidate on the same Fedora
EPYC host, allowing warm file caches. This ordering and shared host load limit
precision. For boots, use `tilo-vm image disk` for both images and
`tilo-vm vm create --emulated` with the settings above. Use the existing
`os/dev/installer-console.py` request/unwrap protocol against the private console
socket, waiting for greeter input readiness. Record
`ExecMainStartTimestampMonotonic`, `ExecMainExitTimestampMonotonic` and
`CPUUsageNSec` of `tilo-extension-activate.service`, plus greetd start,
`systemd-analyze time`, failed units and ordering-cycle journal output. Missing
metrics must fail the run. Remove each owned VM through `tilo-vm vm rm` after
saving its observations. Never add a root access key to obtain measurements.

## Rejected networking change

A separate six-boot experiment moved network startup after user-session setup,
while preserving firewall-before-NetworkManager ordering. Greeter readiness
regressed from 82.884 to 89.672 seconds median. That candidate was reverted;
its raw observations are retained as a negative result. Removing a dependency
from a critical chain alone was insufficient evidence of a usable boot gain.

## Validation

- 57 extension unit/lifecycle tests passed, including actual empty activation in
  a fresh interpreter that refuses HTTP/TLS/archive imports. The new regression
  fails against the original helper and passes with the candidate.
- Four existing end-to-end tests passed using real RPMs and real P-256
  verification: signed install/remove, changed manifest, wrong signer and
  replaced artifact. No tests were skipped. The verifier came from the pinned
  image and was used only as a host validation tool.
- HTTPS downgrade rejection, permitted HTTPS redirects and opener reuse passed.
- `os/tests/sealed_extensions.sh` and `os/tests/system_extension_invariants.sh`
  passed.
- Remote `check --locked -p shell --tests -j8` passed.

Hardware boot, whole-app first paint and current-main full image measurements
were not run. The separately owned renderer and device-performance work was
not changed.
