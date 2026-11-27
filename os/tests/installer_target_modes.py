#!/usr/bin/python3
"""Exercise shared installer target promotion and mutable-state discovery."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

BACKEND = Path(__file__).resolve().parents[1] / 'usr/libexec/tilo-install'
SOURCE = BACKEND.read_text()


class InstallerTargetModeTests(unittest.TestCase):
    def test_signed_target_boot_mount_and_promotion(self):
        with self.subTest(phase="signed_boot_mount_is_required_before_any_disk_work"):
            guard = '[ "$ENCRYPT" -eq 1 ] || die "the signed target UKI requires encrypted TILO_BOOT layout"'
            self.assertIn(guard, SOURCE)
            self.assertLess(SOURCE.index(guard), SOURCE.index('validate_disk_identity\n'))
            self.assertNotIn('--no-encrypt)', SOURCE)
            self.assertIn('mkfs.ext4 -q -L TILO_BOOT "$boot_part"',
                          (BACKEND.parent / 'tilo-disk-encrypt').read_text())
            self.assertIn('mkfs.ext4 -q -L TILO_BOOT "$boot"',
                          (BACKEND.parent / 'tilo-disk-encrypt').read_text())
        with self.subTest(phase="encrypted_mode_promotes_the_signed_target"):
            self.assertIn('promote_target_uki "$ENC_MNT/boot/efi"', SOURCE)
            body = SOURCE.split('promote_target_uki() {', 1)[1].split('\n}\n', 1)[0]
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                esp = root / 'esp'
                uki = esp / ('EFI/Linux/bootc/bootc_composefs-' + 'a' * 128 + '.efi')
                uki.parent.mkdir(parents=True)
                uki.write_bytes(b'live installer')
                old = uki.with_name('bootc_composefs-' + 'b' * 128 + '.efi')
                old.write_bytes(b'old installed generation')
                bundle = root / 'image/boot/tilo-target'
                (bundle / 'predictions').mkdir(parents=True)
                (bundle / 'tilo-target.efi').write_bytes(b'signed target bytes')
                (bundle / 'predictions/generation-42.json').write_text('prediction')
                (bundle / 'predictions/generation-42.json.uki.sig').write_text('signature')
                # Replace only the external command-line parser with a fixture;
                # execute production copy/promotion/path logic on ordinary files.
                body = body.replace('/usr/libexec/tilo-uki-cmdline', 'fixture_cmdline')
                body = body.replace('/usr/libexec/tilo-verified-boot-install-preflight', 'fixture_preflight')
                script = '''set -euo pipefail
OFFLINE_TARGET=1
die() { echo "$*" >&2; exit 1; }
phase() { :; }
fixture_preflight() { return 0; }
fixture_cmdline() {
  case "$1" in
    show) printf 'root=tilo-auto rd.luks.options=tpm2-device=auto,fido2-device=auto tilo.mode=normal systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec composefs=%s\\n' "$FIXTURE_DIGEST" ;;
    boot-digest) printf '%s\\n' 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' ;;
    *) return 64 ;;
  esac
}
# The installed image's root, as the target's composefs repository mounts it.
mount_installed_image() { INSTALLED_IMAGE_ROOT="$FIXTURE_IMAGE"; }
unmount_installed_image() { INSTALLED_IMAGE_ROOT=""; }
promote_target_uki() {\n''' + body + '\n}\npromote_target_uki "$FIXTURE_ESP"\n'
                env = dict(os.environ, FIXTURE_IMAGE=str(root / 'image'), FIXTURE_ESP=str(esp), FIXTURE_DIGEST='a' * 128)
                result = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(uki.read_bytes(), b'signed target bytes')
                self.assertEqual(old.read_bytes(), b'old installed generation')
                self.assertEqual((esp / 'EFI/tilo/pcr-predictions/generation-42.json').read_text(), 'prediction')
                second = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
                self.assertNotEqual(second.returncode, 0)
                self.assertIn('already contains', second.stderr)

    def test_server_destination_creates_no_login_account(self):
        firstboot = (BACKEND.parent / 'tilo-firstboot-user').read_text()
        with self.subTest(phase="purpose_server_refuses_an_account_credential"):
            self.assertIn('if [ "$PURPOSE_VAL" = server ]; then\n  SERVER_DESTINATION=1', SOURCE)
            self.assertIn("--password-hash cannot seed a Tilo Server", SOURCE)
            self.assertIn("takes no --user", SOURCE)
            self.assertIn("takes no --full-name", SOURCE)
        with self.subTest(phase="the_installed_image_must_be_the_server_image"):
            guard = ('elif [ "$INSTALLED_FLAVOUR" = server ]; then\n'
                     '  die "the Tilo Server image has no login account; select the server destination to install it"')
            self.assertIn(guard, SOURCE)
            self.assertIn('/usr/lib/tilo/image-flavour', SOURCE)
            self.assertLess(SOURCE.index('INSTALLED_FLAVOUR="$(cat'),
                            SOURCE.index('stage_installed_firstboot_runtime / '),
                            'the image decides before anything account-shaped is written')
        with self.subTest(phase="no_credential_seed_or_login_session_is_written"):
            self.assertIn('if [ "$SERVER_DESTINATION" -eq 0 ]; then\n'
                          '  stage_installed_firstboot_runtime / "$DEPLOY_ETC"', SOURCE)
            # The shipped greeter config replaces the live installer's in both
            # cases; only a desktop target gets the one-boot account session.
            self.assertNotIn('if [ "$SERVER_DESTINATION" -eq 0 ]; then\n'
                             '  install -d -m 0755 "$DEPLOY_ETC/greetd"', SOURCE)
            self.assertIn('install -m 0644 /usr/share/tilo/greetd-config.toml "$GREETD_CONFIG_TMP"\n'
                          'if [ "$SERVER_DESTINATION" -eq 0 ]; then', SOURCE)
            self.assertNotIn('firstboot-autologin-pending', SOURCE.split(
                'install -m 0644 /usr/share/tilo/greetd-config.toml "$GREETD_CONFIG_TMP"', 1)[0])
            self.assertIn('rm -f "$DEPLOY_ETC/tilo/install-user.conf"', SOURCE)
            # The server's store belongs to the daemon account its image runs.
            self.assertIn('[ "$SERVER_DESTINATION" -eq 0 ] || STORE_OWNER=tilo', SOURCE)
        with self.subTest(phase="one_startup_secret_is_the_server_whole_secret"):
            self.assertIn('the Tilo Server\'s startup secret must be the one secret supplied on stdin',
                          SOURCE)
            self.assertIn('$SERVER_DESTINATION" -eq 0 ] && [ "$PASSWORD_IS_HASH" -eq 0 ]',
                          SOURCE)
        with self.subTest(phase="firstboot_refuses_a_seed_on_the_server_image"):
            self.assertIn('if [ "$(cat "${TILO_IMAGE_FLAVOUR_FILE:-/usr/lib/tilo/image-flavour}" 2>/dev/null || true)" = server ]; then',
                          firstboot)
            self.assertIn('the Tilo Server image has no login account; refusing $CONF', firstboot)
            self.assertLess(
                firstboot.index('= server ]; then'),
                firstboot.index('[ -e "$CONF" ] || [ -L "$CONF" ] || exit 0'),
                'the server guard comes before the seed is consumed')

    def test_target_etc_discovery(self):
        with self.subTest(phase="composefs_mutable_etc_discovery"):
            body = SOURCE.split('find_deploy_etc() {', 1)[1].split('\n}\n', 1)[0]
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                etc = root / 'state/deploy/digest/etc'
                etc.mkdir(parents=True)
                result = subprocess.run(['bash', '-c', 'find_deploy_etc() {\n' + body +
                    '\n}\nfind_deploy_etc "$1"', 'fixture', str(root)], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), str(etc))


if __name__ == '__main__':
    unittest.main()
