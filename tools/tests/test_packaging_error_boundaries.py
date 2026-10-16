"""Packaging failures must not become successful downloads or baseline reuse."""
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.append(str(Path(__file__).resolve().parents[1] / 'apps'))
import app_packaging_service as service
import assemble_verified_apps as assembly


class Response(io.BytesIO):
    headers = {'Content-Length': '4'}

    def geturl(self):
        return service.ORIGIN + '/api/v1/user'


class PackagingErrorBoundaries(unittest.TestCase):
    def test_download_rejects_premature_eof(self):
        transport = service.Transport('test-token')
        with tempfile.TemporaryDirectory() as temporary:
            for download in (False, True):
                with self.subTest(download=download), patch.object(
                        transport.opener, 'open', return_value=Response(b'12')):
                    with self.assertRaisesRegex(ValueError, 'truncated'):
                        transport.request('user', output=Path(temporary) / 'artifact' if download else None)
        # An absent length remains supported, as does an exact-length response.
        for raw, headers in ((b'1234', {'Content-Length': '4'}), (b'12', {})):
            response = Response(raw)
            response.headers = headers
            with patch.object(transport.opener, 'open', return_value=response):
                self.assertEqual(transport.request('user'), raw)

    def test_local_destination_errors_are_not_reported_as_api_failures(self):
        transport = service.Transport('test-token')
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'artifact'
            output.write_bytes(b'existing artifact')
            with patch.object(transport.opener, 'open', return_value=Response(b'1234')):
                with self.assertRaises(FileExistsError) as caught:
                    transport.request('user', output=output)
            self.assertEqual(caught.exception.filename, str(output))
            self.assertEqual(output.read_bytes(), b'existing artifact')

    def test_invalid_current_package_is_not_replaced_by_baseline(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            built, baseline = root / 'built', root / 'baseline'
            for tree in (built, baseline):
                (tree / 'tapp').mkdir(parents=True)
                (tree / 'runtime/tilo-runtime/1').mkdir(parents=True)
                (tree / 'runtime/tilo-runtime/1/bundle.toml').write_text('root_hash="same"')
            filename = 'app-1-x86_64.tapp'
            (baseline / 'tapp' / filename).write_bytes(b'old signed package')
            plan = {'catalog': [{'name': 'app', 'version': '1'}]}
            current = built / 'tapp' / filename
            for kind in ('directory', 'broken-link'):
                if kind == 'directory':
                    current.mkdir()
                else:
                    current.symlink_to('missing-package')
                with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, 'current build package'):
                    assembly.merge(plan, built, baseline, root / kind, 'x86_64', [])
                if kind == 'directory':
                    current.rmdir()
                else:
                    current.unlink()
            self.assertEqual(assembly.merge(plan, built, baseline, root / 'absent', 'x86_64', []), [filename])


if __name__ == '__main__':
    unittest.main()
