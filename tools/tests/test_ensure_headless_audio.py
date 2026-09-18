"""Exercise the production setup script with an isolated package-manager fixture."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HeadlessAudioSetupTests(unittest.TestCase):
    def run_setup(self, **settings):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp)
            (fixture / 'env.sh').write_text(r'''
command() {
    if [[ $1 == -v && $2 == pulseaudio ]]; then
        [[ ${PULSE_PRESENT:-0} == 1 || -f "$FIXTURE/installed" ]]
    else builtin command "$@"; fi
}
test() {
    if [[ ${2:-} == /.dockerenv || ${2:-} == /run/.containerenv ]]; then
        [[ ${IN_CONTAINER:-1} == 1 ]]
    else builtin test "$@"; fi
}
rpm() {
    if [[ $1 == -qa ]]; then
        printf '%s\n' mesa-dri-drivers pulseaudio-libs fonts
        if [[ ${PIPEWIRE_PRESENT:-1} == 1 ]]; then echo pipewire-pulseaudio; fi
    else [[ ${PIPEWIRE_PRESENT:-1} == 1 ]]; fi
}
dnf() {
    printf '%s\n' "$@" > "$FIXTURE/dnf-args"
    if [[ ${DNF_FAIL:-0} == 1 ]]; then return 7; fi
    if [[ ${DNF_NO_BINARY:-0} != 1 ]]; then touch "$FIXTURE/installed"; fi
}
''')
            env = dict(os.environ, BASH_ENV=str(fixture / 'env.sh'),
                       FIXTURE=tmp, GITHUB_ACTIONS='true')
            env.update(settings)
            result = subprocess.run(['bash', str(ROOT / 'tools/ensure-headless-audio.sh')],
                                    env=env, capture_output=True, text=True)
            args = fixture / 'dnf-args'
            return result, args.read_text().splitlines() if args.exists() else []

    def test_swap_only_conflicting_daemon_preserves_libraries(self):
        result, args = self.run_setup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args[0], 'swap')
        self.assertEqual(args[-2:], ['pipewire-pulseaudio', 'pulseaudio'])
        self.assertIn('--exclude=fonts,mesa-dri-drivers,pulseaudio-libs', args)
        self.assertIn('--setopt=clean_requirements_on_remove=False', args)
        self.assertIn('--setopt=obsoletes=False', args)

    def test_install_when_no_conflicting_daemon(self):
        result, args = self.run_setup(PIPEWIRE_PRESENT='0')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args[0], 'install')

    def test_existing_binary_needs_no_package_transaction(self):
        result, args = self.run_setup(PULSE_PRESENT='1', IN_CONTAINER='0')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(args, [])

    def test_refuses_to_mutate_host(self):
        result, args = self.run_setup(IN_CONTAINER='0')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(args, [])

    def test_refuses_non_ci_container(self):
        result, args = self.run_setup(GITHUB_ACTIONS='false')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(args, [])

    def test_package_failure_fails_setup(self):
        result, _ = self.run_setup(DNF_FAIL='1')
        self.assertEqual(result.returncode, 7)

    def test_successful_transaction_without_binary_still_fails(self):
        result, _ = self.run_setup(DNF_NO_BINARY='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('did not supply', result.stderr)


if __name__ == '__main__':
    unittest.main()
