#!/usr/bin/env python3
"""Guard where the encrypted install takes its image from, and in what order."""

from pathlib import Path
import unittest


INSTALL = (Path(__file__).resolve().parents[1] / "usr/libexec/tilo-install").read_text()


class InstallSourceTest(unittest.TestCase):
    def test_the_source_follows_the_reviewed_preparation(self):
        preflight = INSTALL.index("validate_disk_identity\n\nif [ \"$GUIDED_ALONGSIDE\"", INSTALL.index("if [ \"$PREFLIGHT_ALONGSIDE\""))
        guided = INSTALL.index('GUIDED_RESULT="$("$GUIDED_PARTITION"')
        prepare = INSTALL.index('PREP="$("$DISK_ENCRYPT" prepare-partition')
        source = INSTALL.index('  select_install_source\n', prepare)
        install = INSTALL.index('  bootc_install_to \\\n', source)
        self.assertLess(preflight, guided)
        self.assertLess(guided, prepare)
        self.assertLess(prepare, source)
        self.assertLess(source, install)
        selection = INSTALL.split('select_install_source() {', 1)[1].split('\n}', 1)[0]
        self.assertIn('validate_disk_identity', selection)

    def test_the_booted_image_is_copied_from_its_own_repository(self):
        selection = INSTALL.split('select_install_source() {', 1)[1].split('\n}', 1)[0]
        self.assertIn('BOOTC_SOURCE_ARGS=(--source-booted)', selection)
        self.assertNotIn('copy-to-storage', INSTALL)
        self.assertNotIn('podman run', INSTALL)
        runner = INSTALL.split('bootc_install_to() {', 1)[1].split('\n}', 1)[0]
        self.assertIn('unshare --mount --propagation slave', runner)
        self.assertIn('"${BOOTC[@]}" install to-filesystem "${BOOTC_SOURCE_ARGS[@]}"', runner)
        self.assertIn('--property=PrivateMounts=yes', INSTALL)
        self.assertNotIn('--replace=alongside', INSTALL)

    def test_the_boot_bundle_comes_from_the_installed_image(self):
        promote = INSTALL.split('promote_target_uki() {', 1)[1].split('\n}', 1)[0]
        mount = promote.index('mount_installed_image')
        copy = promote.index('cp -R -- "$INSTALLED_IMAGE_ROOT/boot/tilo-target" "$esp/"')
        unmount = promote.index('unmount_installed_image', copy)
        self.assertLess(mount, copy)
        self.assertLess(copy, unmount)
        mounting = INSTALL.split('mount_installed_image() {', 1)[1].split('\n}', 1)[0]
        self.assertIn('bootc internals cfs --repo "$ENC_MNT/composefs" oci mount "$manifest" "$root"', mounting)
        self.assertLess(mounting.index('oci mount'), mounting.index('INSTALLED_IMAGE_ROOT="$root"'))
        self.assertIn('the target must hold exactly one deployment', mounting)

    def test_failure_unmounts_the_installed_image_before_luks_teardown(self):
        cleanup = INSTALL.split('cleanup_encrypted_install() {', 1)[1].split('\n  }', 1)[0]
        self.assertLess(cleanup.index('unmount_installed_image'), cleanup.index('"$DISK_ENCRYPT" teardown'))
        self.assertIn('return 1', cleanup.split('"$DISK_ENCRYPT" teardown', 1)[0])
        self.assertIn('trap cleanup_encrypted_install EXIT', INSTALL)
        self.assertIn('validate_partition_identity "$ROOT_PARTITION" "$EXPECT_ROOT_PARTUUID"', INSTALL)
        self.assertIn('prepared EFI mount differs from the reviewed target', INSTALL)


if __name__ == "__main__":
    unittest.main()
