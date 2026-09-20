"""Exercise upload ordering, catalogue conflicts and fail-closed responses."""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('submit_catalogue', ROOT / 'tools/apps/submit_catalogue.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CatalogueSubmissionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        directory = root / 'tilo-apps-x86_64'
        (directory / 'qpkg').mkdir(parents=True)
        (directory / 'qpkg/app.qpkg').write_bytes(b'fixture')
        (directory / 'index.json').write_text(json.dumps({'apps': [{'name': 'calculator', 'file': 'app.qpkg'}]}))
        self.args = argparse.Namespace(release=root, public_base='https://code.example/api/packages/qos/generic',
            registry='https://registry.example', version='sha-abc', owner_key='a' * 64,
            identity=root / 'identity', pkg_binary=root / 'tilo-pkg')
        self.change = {'entry': {'package_id': 'dev.tilo.calculator'}}

    def test_public_proxy_urls_preserve_development_version_identity(self):
        directory = self.args.release / 'tilo-apps-x86_64'
        filename = 'calculator-0.1.1-dev.10+gabc-x86_64.qpkg'
        (directory / 'qpkg' / filename).write_bytes(b'fixture')
        (directory / 'index.json').write_text(json.dumps({'apps': [{'name': 'calculator', 'file': filename}]}))
        artifacts = module.artifact_plan(self.args.release,
            'https://registry.podesta.ai/artifacts/qos/generic', 'sha-abc')
        self.assertEqual(artifacts[0]['url'],
            'https://registry.podesta.ai/artifacts/qos/generic/tilo-app-calculator/sha-abc/'
            'calculator-0.1.1-dev.10%2Bgabc-x86_64.qpkg')

    def test_conflict_refreshes_boundedly_and_acceptance_is_not_publication(self):
        responses = [(200, {'sequence': 1}), (409, None), (200, {'sequence': 2}),
                     (202, {'status': 'pending_review', 'id': 'dev.tilo.calculator'})]
        calls = []
        def transport(url, payload=None):
            calls.append(payload)
            return responses.pop(0)
        signed = subprocess.CompletedProcess([], 0, json.dumps([self.change]), '')
        with patch.object(module.subprocess, 'run', return_value=signed) as signer:
            result = module.submit(self.args, transport)
        self.assertEqual(result, {'status': 'queued', 'accepted_requests': 1})
        self.assertEqual(len(calls), 4)
        self.assertEqual(signer.call_count, 2)
        self.assertIn('--owner-key', signer.call_args.args[0])

    def test_permanent_conflict_and_invalid_signature_never_succeed(self):
        signed = subprocess.CompletedProcess([], 0, json.dumps([self.change]), '')
        def conflict(url, payload=None):
            return (200, {'sequence': 1}) if payload is None else (409, None)
        with patch.object(module.subprocess, 'run', return_value=signed) as signer:
            with self.assertRaisesRegex(ValueError, 'three attempts'):
                module.submit(self.args, conflict)
            self.assertEqual(signer.call_count, 3)
        calls = []
        def transport(url, payload=None):
            calls.append(payload)
            return 200, {'sequence': 1}
        with patch.object(module.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'verify')):
            with self.assertRaises(subprocess.CalledProcessError):
                module.submit(self.args, transport)
        self.assertEqual(calls, [None])

    def test_verifier_process_rejection_is_visible_without_identity_contents(self):
        root = self.args.release
        (root / 'identity').write_text('SECRET-MUST-NOT-APPEAR')
        (root / 'catalogue-entries').write_text(
            'import sys\nsys.stderr.write("authenticate catalogue baseline: expired snapshot\\n")\nsys.exit(2)\n')
        self.args.pkg_binary = Path(sys.executable)
        values = {key: str(value) for key, value in vars(self.args).items()}
        code = f"""import argparse, importlib.util
from pathlib import Path
spec = importlib.util.spec_from_file_location('submission', {str(ROOT / 'tools/apps/submit_catalogue.py')!r})
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
values = {values!r}
for key in ('release', 'pkg_binary', 'identity'):
    values[key] = Path(values[key])
module.submit(argparse.Namespace(**values), lambda url: (200, {{'sequence': 1}}))
"""
        result = subprocess.run([sys.executable, '-c', code], cwd=root, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('authenticate catalogue baseline: expired snapshot', result.stderr)
        self.assertNotIn('SECRET-MUST-NOT-APPEAR', result.stdout + result.stderr)

    def test_workflow_waits_for_upload_and_excludes_upload_credentials(self):
        workflow = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        job = workflow.split('  submit-catalogue:\n', 1)[1]
        self.assertIn("needs.publish.result == 'success'", job)
        self.assertIn("TILO_APP_CATALOGUE_SUBMISSION_ENABLED == 'true'", job)
        self.assertIn('needs: [plan, build, publish]', job)
        self.assertIn('persist-credentials: false', job)
        self.assertIn('    runs-on: runner\n', job)
        self.assertNotIn('enable-openid-connect', job)
        self.assertNotIn('TILO_PACKAGE_TOKEN', job)
        self.assertIn('unset QPKG_PUBLISHER_IDENTITY', job)
        self.assertIn('vars.TILO_APP_CATALOGUE_ARTIFACT_BASE_URL', job)
        self.assertNotIn('vars.TILO_PACKAGE_PUBLIC_BASE_URL', job)
        self.assertLess(job.index('catalogue-entries --help'), job.index('tools/apps/submit_catalogue.py'))
        producer = (ROOT / '.forgejo/workflows/canonical-verifier-bundle.yml').read_text()
        self.assertLess(producer.index('tilo-pkg catalogue-entries --help'),
                        producer.index('tools/canonical_verifier_bundle.py produce'))


    def test_rollback_and_unexpected_success_are_rejected(self):
        signed = subprocess.CompletedProcess([], 0, json.dumps([self.change]), '')
        for responses in [[(200, {'sequence': 2}), (409, None), (200, {'sequence': 1})],
                          [(200, {'sequence': 1}), (200, {'status': 'published'})]]:
            with patch.object(module.subprocess, 'run', return_value=signed):
                with self.assertRaises(ValueError):
                    module.submit(self.args, lambda url, payload=None: responses.pop(0))


if __name__ == '__main__':
    unittest.main()
