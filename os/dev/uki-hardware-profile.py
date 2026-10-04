#!/usr/bin/env python3
"""Resolve an image-owned hardware profile before the UKI is measured/signed.

Usage: uki-hardware-profile.py ROOT KERNEL_VERSION ARCHITECTURE

Print the validated DTB path under ROOT, or nothing for a generic image. The
profile and DTB are immutable rootfs inputs, so their bytes are also covered by
the composefs digest. This is a build-time input, never a target-side path API.
"""

import sys
from pathlib import Path
from hardware_profile import PROFILE_PATH, resolve_profile


PROFILE = PROFILE_PATH



def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit(__doc__.splitlines()[2])
    try:
        dtb = resolve_profile(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except (OSError, ValueError, TypeError) as error:
        raise SystemExit(f"hardware profile rejected: {error}") from error
    if dtb is not None:
        print(dtb)


if __name__ == "__main__":
    main()
