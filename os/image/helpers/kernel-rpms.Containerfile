# ---- Custom kernel repo ----------------------------------------------------
# Same "OCI image with /repo" shape as the frozen RPM repo (see
# os/Containerfile.rpms), but its content is a rebuilt Fedora kernel with
# our config fragment applied (static storage/net/USB/HID + PREEMPT +
# HZ=1000 + THP=always + trimmed debuginfo).  Built by
# .forgejo/workflows/build-kernel.yml and pinned by digest via
# os/kernel-snapshot.env — the runtime stage below swaps kernel-core for
# tilo-kernel-core out of it.
FROM ${KERNEL_RPMS_REF} AS kernel-rpms
