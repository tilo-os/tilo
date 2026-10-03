#!/usr/bin/env python3
"""Guard the order and containment of the encrypted bootc copy scratch."""

from pathlib import Path
import unittest


INSTALL = (Path(__file__).resolve().parents[1] / "usr/libexec/tilo-install").read_text()


class EncryptedCopyScratchTest(unittest.TestCase):
    def test_encrypted_copy_scratch_is_ordered_contained_and_released(self):
        with self.subTest(phase="source integrity precedes destructive guided and prepare"):
            preflight = INSTALL.index("validate_disk_identity\n\nif [ \"$GUIDED_ALONGSIDE\"", INSTALL.index("if [ \"$PREFLIGHT_ALONGSIDE\""))
            guided = INSTALL.index('GUIDED_RESULT="$("$GUIDED_PARTITION"')
            prepare = INSTALL.index('PREP="$("$DISK_ENCRYPT" prepare-partition')
            copy = INSTALL.index('  copy_booted_image\n', prepare)
            self.assertLess(preflight, guided)
            self.assertLess(guided, prepare)
            self.assertLess(prepare, copy)
            self.assertIn("validate_disk_identity\n\n# --- 2. install", INSTALL[INSTALL.index("COPY_SCRATCH_MOUNTED=0"):prepare])

        with self.subTest(phase="export uses only reviewed LUKS root and is removed before install"):
            self.assertIn('COPY_SCRATCH="$ENC_MNT/.tilo-bootc-copy-tmp"', INSTALL)
            self.assertIn('install -d -m 1777 -- "$COPY_SCRATCH"', INSTALL)
            self.assertIn('MIN_BOOTC_SCRATCH_FREE_BYTES=25769803776', INSTALL)
            self.assertIn('scratch_available="$(df -B1 --output=avail "$ENC_MNT"', INSTALL)
            self.assertIn('mount --bind -- "$COPY_SCRATCH" /var/tmp', INSTALL)
            self.assertIn('if mountpoint -q /var/tmp; then COPY_SCRATCH_MOUNTED=1; fi', INSTALL)
            self.assertIn('--property=PrivateMounts=yes', INSTALL)
            copy = INSTALL.index('TMPDIR=/var/tmp bootc image copy-to-storage')
            release = INSTALL.index('release_copy_scratch || die "could not remove bootc scratch mount"', copy)
            install = INSTALL.index('"${ALONGSIDE_BOOTC_COMMAND[@]}" install to-filesystem', release)
            self.assertLess(copy, release)
            self.assertLess(release, install)
            self.assertIn('rm -rf --one-file-system -- "$COPY_SCRATCH"', INSTALL)
            self.assertNotIn('--replace=alongside', INSTALL)

        with self.subTest(phase="failure unmounts scratch before LUKS teardown"):
            cleanup = INSTALL.split('cleanup_encrypted_install() {', 1)[1].split('\n  }', 1)[0]
            self.assertLess(cleanup.index('release_copy_scratch'), cleanup.index('"$DISK_ENCRYPT" teardown'))
            self.assertIn('return 1', cleanup.split('"$DISK_ENCRYPT" teardown', 1)[0])
            self.assertIn('trap cleanup_encrypted_install EXIT', INSTALL)
            self.assertIn('validate_partition_identity "$ROOT_PARTITION" "$EXPECT_ROOT_PARTUUID"', INSTALL)
            self.assertIn('prepared EFI mount differs from the reviewed target', INSTALL)

if __name__ == "__main__":
    unittest.main()
