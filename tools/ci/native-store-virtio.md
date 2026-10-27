# Disposable virtio native-store diagnostics

`disposable-native-vm.py` creates its own rootless libvirt VM through `tilo-vm`,
with a private registry, a copy-on-write system disk and two newly created sparse
1 GiB auxiliary disks. It never starts or stops an existing configured VM. The
base must be a published qcow2 already cached by `tilo-vm`, with a matching SHA256
sidecar. QEMU opens the backing image read-only; the cached base is kept.

From the repository root, for example:

```sh
python3 tools/ci/disposable-native-vm.py \
  --base "$HOME/.cache/tilo-vm/disks/<tag>/<published-disk>.qcow2" \
  -- python3 -c 'import os; print(os.environ["TILO_FS_NATIVE_STORES"])'
```

Guest execution and fixture transfer use only `tilo device vm ssh`. No new daemon or
privileged-exec capability is installed. An older image's daemon protocol mismatch
is tolerated only for this diagnostic SSH route, not as qualification evidence.

The harness supplies a root-owned private provenance file binding each store to
its unique virtio serial, size, VM identity and verified base hash. The opt-in
`TILO_NATIVE_STORE_VIRTIO_PROVENANCE` branch in `native-stores.sh` checks this
provenance. Without it the existing loop lifecycle is unchanged.

Before any disk write, the helper rejects partitions, holders, mounted/root/swap
devices and serial/size mismatches. It claims the block device with kernel
`O_EXCL`, and reads its entire extent to require zeros. Because `mkfs.btrfs`
reopens devices exclusively, formatting is staged in a private regular file and
installed through the still-exclusively-held device descriptor. The disks are
mounted at subvolume 5. Reformatting a previous attempt is refused. Teardown
checks the serial-bound mount source before unmounting (Btrfs mountinfo device
numbers are anonymous, not the underlying block-device numbers).

Normal failures and catchable cancellation tear down the owned VM before deleting
its auxiliary storage. If domain removal fails, backing files are retained rather
than unlinked under a live VM. SIGKILL and host power loss cannot run cleanup;
the printed VM name and private directory identify retained resources.

**A successful diagnostic is not current-source qualification.** Genuine Broker
Review must still pass on the virtio Btrfs source; current installed SDK roles,
read-only publisher pins, the native-feature Root-source proof and the existing
Root UI, app-grant and codec tests remain required. An old frozen candidate or
published base cannot qualify a changed source. No producer receipts, source
ledgers, installed roles or production `BackingWitness` checks are rewritten by
this tool.

Refusal and lifecycle tests:

```sh
python3 -m unittest discover -s tools/ci -p 'test_native_store_virtio.py'
```
