# Positive native installer journeys

`native_journey_tests.py` executes the consent test and both positive main-story
selectors, one at a time against fresh GPT/Ext4 sources. It uses `owned_source`,
not caller-supplied disk facts. Run only on a dedicated disposable native builder:

```sh
sudo -n python3 native_journey_tests.py \
  --helper /root/artifacts/imager \
  --compiler-json /root/artifacts/helper-compiler.jsonl \
  --tests /root/artifacts/imager-test-binary \
  --tests-compiler-json /root/artifacts/tests-compiler.jsonl \
  --evidence /root/fresh-evidence
```

Prepare the evidence directory with mode 0700. Copy the **actual** standalone
helper and test executable from the same fresh Cargo build, and copy Cargo's
unchanged `--message-format=json` output. Inputs must be root-owned regular files,
not writable by other users. The helper wrapper verifies the compiler manifest
and source paths against its compiled-in source directory and authenticates the
helper hash. The coordinator checks matching helper/test compiler manifests and hashes all
four admitted inputs in evidence.

The host must already have NBD available with partition scanning enabled,
QEMU 10.2.2, the native filesystem/partition tools and OpenSSL development tools.
Provision the dedicated builder externally (for example, `modprobe nbd
nbds_max=16 max_part=16`); the fixture does not install software or load modules.
Native dynamic libraries must also match the actual compiled executables.

The coordinator holds the initial PID namespace handle before creating its
private mount/PID/network namespace. No desktop or compositor is started.
The root helper is the real standalone artifact, invoked by the owned nonroot
actor through the existing test-only elevation bridge, not a Polkit qualification.
Root-private leases serialize source fixtures without blocking genuine udev
probing. Udev obtains GPT and filesystem properties from the owned devices.

Each selector writes `stdout.log` and `stderr.log`; `results.json` records exact
execution, not merely test discovery. A nonzero selector is reported as failed.
Every successful source teardown disconnects and reaps the exact QEMU child,
releases active block openers while retaining an O_PATH node pin and exclusive
lease, observes kernel PID absence and zero capacity, and only then deletes its
owned backing image and records `release-complete`. Unknown teardown fails and
preserves the source image rather than claiming release.
