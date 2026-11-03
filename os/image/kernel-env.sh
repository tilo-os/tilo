#!/usr/bin/env bash
# The kernel one OS action installs from, as os/dev/build-image.sh takes it: KERNEL_RPMS_REF and
# TILO_KERNEL_TRUST. Sourced, not run, so a test can read what it decides (tools/tests/
# test_build_os.py) and the layer actions take it by one rule.
#
# The pinned signed kernel (os/kernel-snapshot.env) by default: nothing here changes it then.
# TILO_KERNEL_REF names another instead (tools/ci/build-os.sh --kernel-ref): a kernel built here
# has no registry signature or provenance, so it and the image are taken unsigned, as a local RPM
# snapshot is (os/image/build-layer.sh rpms_snapshot); a registry kernel by digest is verified as
# the pin is.
kernel_env() {
  [[ -n ${TILO_KERNEL_REF:-} ]] || return 0
  export KERNEL_RPMS_REF=$TILO_KERNEL_REF
  case $TILO_KERNEL_REF in
    registry.tilo.so/tilo/tilo-kernel@sha256:*) export TILO_KERNEL_TRUST=signed ;;
    *) export TILO_KERNEL_TRUST=unsigned ;;
  esac
}
