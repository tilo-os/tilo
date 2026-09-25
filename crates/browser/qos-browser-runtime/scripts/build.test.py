import json, os, pathlib, subprocess, tempfile, unittest

SCRIPT = pathlib.Path(__file__).with_name('build.sh').resolve()

class BuildProfileTests(unittest.TestCase):
    def test_profiles_select_matching_cargo_and_bindgen_artifact(self):
        for profile, directory, extra in [('release','release',[]),('dev','debug',['--profile','dev'])]:
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                binary = root/'bin'; binary.mkdir()
                (binary/'cargo').write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$LOG"\n')
                (binary/'rustc').write_text('#!/bin/sh\necho rustc-test\n')
                (binary/'wasm-bindgen').write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo wasm-bindgen-test; exit; fi\nprintf "%s\\n" "$*" >> "$LOG"\nmkdir -p "$5"\nprintf wasm > "$5/qos_browser_runtime_bg.wasm"\nprintf glue > "$5/qos_browser_runtime.js"\n')
                for file in binary.iterdir(): file.chmod(0o755)
                environment = dict(os.environ, PATH=str(binary)+':'+os.environ['PATH'], LOG=str(root/'log'), CARGO_TARGET_DIR=str(root/'target'))
                result = subprocess.run(['bash', str(SCRIPT), str(root/'out'), *extra], env=environment, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                log = (root/'log').read_text()
                self.assertIn('--profile '+profile, log)
                self.assertIn('/'+directory+'/qos_browser_runtime.wasm', log)
                metadata = json.loads((root/'out/build.json').read_text())
                self.assertEqual(metadata['profile'], profile)
                self.assertEqual(metadata['rustc'], 'rustc-test')
                self.assertIn('pkg/qos_browser_runtime_bg.wasm', metadata['files'])
    def test_z_invalid_profile_fails_before_compiling(self):
        result = subprocess.run(['bash', str(SCRIPT), '/does-not-exist', '--profile','fast'], env=dict(os.environ, PATH='/usr/bin:/bin'), capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('profile', result.stderr)

if __name__ == '__main__': unittest.main()
