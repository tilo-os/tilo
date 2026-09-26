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

    def test_headless_audio_setup_swaps_only_in_ci_containers_and_verifies_the_result(self):
        with self.subTest(phase='swap only conflicting daemon preserves libraries'):
            result, args = self.run_setup()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(args[0], 'swap')
            self.assertEqual(args[-2:], ['pipewire-pulseaudio', 'pulseaudio'])
            self.assertIn('--exclude=fonts,mesa-dri-drivers,pulseaudio-libs', args)
            self.assertIn('--setopt=clean_requirements_on_remove=False', args)
            self.assertIn('--setopt=obsoletes=False', args)
        with self.subTest(phase='install when no conflicting daemon'):
            result, args = self.run_setup(PIPEWIRE_PRESENT='0')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(args[0], 'install')
        with self.subTest(phase='existing binary needs no package transaction'):
            result, args = self.run_setup(PULSE_PRESENT='1', IN_CONTAINER='0')
            self.assertEqual(result.returncode, 0)
            self.assertEqual(args, [])
        with self.subTest(phase='refuses to mutate host'):
            result, args = self.run_setup(IN_CONTAINER='0')
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(args, [])
        with self.subTest(phase='refuses non ci container'):
            result, args = self.run_setup(GITHUB_ACTIONS='false')
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(args, [])
        with self.subTest(phase='package failure fails setup'):
            result, _ = self.run_setup(DNF_FAIL='1')
            self.assertEqual(result.returncode, 7)
        with self.subTest(phase='successful transaction without binary still fails'):
            result, _ = self.run_setup(DNF_NO_BINARY='1')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('did not supply', result.stderr)


if __name__ == '__main__':
    unittest.main()
