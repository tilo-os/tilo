#!/usr/bin/env python3
"""Guard the live installer's enforced SELinux bridge boundary."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
POLICY = ROOT / "os/usr/share/tilo/selinux/tilo-installer.cil"
SETUP = ROOT / "os/usr/lib/systemd/system/tilo-installer-setup.service"
BACKEND = ROOT / "os/usr/libexec/tilo-install"


def active_policy() -> str:
    return "\n".join(line.split(";", 1)[0] for line in POLICY.read_text().splitlines())


class BridgeContract(unittest.TestCase):
    def test_installed_firstboot_files_get_final_etc_labels_before_rename(self):
        backend = BACKEND.read_text()
        helper = backend[backend.index('label_installed_tilo_path() {'):]
        helper = helper[:helper.index('\n}')]
        self.assertIn('matchpathcon -n -- "$installed"', helper)
        self.assertIn('chcon --no-dereference -- "$context" "$staged"', helper)

        # The deployed /etc tree is accessed through /run during installation.
        # Atomic rename retains the temporary inode's label, so each staged
        # first-boot input must be relabelled before publication.
        for staged, final, publication in (
            ('$DEPLOY_ETC/tilo', '/etc/tilo', 'INSTALL_MARKER='),
            ('$marker_tmp', '/etc/tilo/install-marker', 'mv -f -- "$marker_tmp" "$INSTALL_MARKER"'),
            ('$AUTOLOGIN_PENDING_TMP', '/etc/tilo/firstboot-autologin-pending', 'mv -f -- "$AUTOLOGIN_PENDING_TMP" "$AUTOLOGIN_PENDING"'),
            ('$CONF_TMP', '/etc/tilo/install-user.conf', 'mv -f "$CONF_TMP" "$CONF"'),
            ('$FULL_NAME_TMP', '/etc/tilo/install-user-full-name', 'mv -f "$FULL_NAME_TMP" "$FULL_NAME_CONF"'),
        ):
            with self.subTest(final=final):
                label = f'label_installed_tilo_path "{staged}" {final}'
                self.assertIn(label, backend)
                self.assertLess(backend.index(label), backend.index(publication))

        handoff = backend.index(
            '/usr/libexec/tilo-install-handoff-copy "$HANDOFF_SRC/etc" "$DEPLOY_ETC"'
        )
        wifi_label = backend.index(
            'label_installed_tilo_path "$DEPLOY_ETC/NetworkManager/system-connections/tilo-setup.nmconnection" '
            '/etc/NetworkManager/system-connections/tilo-setup.nmconnection'
        )
        self.assertLess(handoff, wifi_label)
        self.assertLess(wifi_label, backend.index('step "Closing the encrypted root"', wifi_label))

    def test_selinux_bridge_policy(self):
        with self.subTest(phase="policy_enforces_greeter_and_scopes_bridge"):
            policy = active_policy()
            compact = " ".join(policy.split())
            self.assertNotIn("(typepermissive", compact)
            self.assertNotIn("(allow xdm_t var_run_t", compact)
            self.assertIn("(allow xdm_t init_t (system (start)))", compact)
            self.assertIn('(filecon "/run/tilo-installer/bridge(/.*)?" any', compact)
            self.assertIn("(type tilo_installer_bridge_t)", compact)
            self.assertEqual(
                re.findall(r"\(allow xdm_t ([^\s)]+)", compact),
                ["init_t", "tilo_installer_bridge_t", "tilo_installer_bridge_t"],
            )
            self.assertIn(
                "(allow xdm_t tilo_installer_bridge_t (dir (getattr search read watch)))",
                compact,
            )
            self.assertIn(
                "(allow xdm_t tilo_installer_bridge_t (file (getattr open read write watch)))",
                compact,
            )
            self.assertIn("(allow init_t tilo_installer_bridge_t (dir (getattr search read write add_name remove_name create setattr rmdir)))", compact)
            self.assertIn("(allow init_t tilo_installer_bridge_t (file (getattr open read write append create setattr unlink)))", compact)
        with self.subTest(phase="policy_is_loaded_before_bridge_and_failures_stop_setup"):
            setup = SETUP.read_text()
            self.assertIn("ConditionKernelCommandLine=tilo.install", setup)
            self.assertIn("NoNewPrivileges=no", setup)
            policy = "ExecStart=/usr/bin/bash -ec 'if /usr/sbin/selinuxenabled; then /usr/sbin/semodule -i /usr/share/tilo/selinux/tilo-installer.cil; fi'"
            bridge = "ExecStart=/usr/bin/install -d -m 0700 /run/tilo-installer/bridge"
            label = "ExecStart=/usr/bin/bash -ec 'if /usr/sbin/selinuxenabled; then /usr/sbin/restorecon -RF /run/tilo-installer; fi'"
            for command in (policy, bridge, label):
                self.assertIn(command, setup)
                self.assertNotIn("ExecStart=-" + command[len("ExecStart="):], setup)
            self.assertLess(setup.index(policy), setup.index(bridge))
            self.assertLess(setup.index(bridge), setup.index(label))
        with self.subTest(phase="greeter_cannot_create_or_relabel_secret_files"):
            backend = BACKEND.read_text()
            prepare = backend.index('tilo-bridge-prepare-$$')
            verify = backend.index("installer runtime bridge permissions are unsafe")
            secret = backend.index('cat > "$INPUT"', prepare)
            install = backend.index('StandardInput=file:$INPUT', prepare)
            self.assertLess(prepare, verify)
            self.assertLess(verify, secret)
            self.assertLess(secret, install)
            self.assertIn("/usr/sbin/restorecon -RF --", backend)
            self.assertIn('matchpathcon -V "$BRIDGE" "$INPUT" "$OUTPUT"', backend)
            self.assertIn('tilo-bridge-clean-$$', backend)
            self.assertIn('systemd-run is required for the enforced installer backend', backend)
            self.assertNotIn('chcon --reference=/run/tilo-installer/bridge', backend)
            self.assertNotIn('mktemp -d /run/tilo-installer/bridge', backend)


if __name__ == "__main__":
    unittest.main()
