#!/usr/bin/env python3
"""Return success only when this host can create a KVM VM for the guest arch."""

import fcntl
import os
import platform
import sys

KVM_GET_API_VERSION = 0xAE00
KVM_CREATE_VM = 0xAE01
KVM_API_VERSION = 12


def normalize_arch(value):
    return {'arm64': 'aarch64', 'amd64': 'x86_64'}.get(value.lower(), value.lower())


def available(guest_arch, device='/dev/kvm'):
    if normalize_arch(guest_arch) != normalize_arch(platform.machine()):
        return False
    try:
        kvm = os.open(device, os.O_RDWR | getattr(os, 'O_CLOEXEC', 0))
    except OSError:
        return False
    try:
        if fcntl.ioctl(kvm, KVM_GET_API_VERSION, 0) != KVM_API_VERSION:
            return False
        vm = fcntl.ioctl(kvm, KVM_CREATE_VM, 0)
        os.close(vm)
        return True
    except OSError:
        return False
    finally:
        os.close(kvm)


if __name__ == '__main__':
    raise SystemExit(0 if len(sys.argv) == 2 and available(sys.argv[1]) else 1)
