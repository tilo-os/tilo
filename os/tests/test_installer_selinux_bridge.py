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
    def test_policy_enforces_greeter_and_scopes_bridge(self):
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

    def test_policy_is_loaded_before_bridge_and_failures_stop_setup(self):
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

    def test_greeter_cannot_create_or_relabel_secret_files(self):
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
