"""Packaging failures must not become successful downloads or baseline reuse."""
import io
import tarfile
import zipfile
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.append(str(Path(__file__).resolve().parents[1] / 'apps'))
import app_packaging_service as service
import assemble_verified_apps as assembly
import accept_app_artifacts as acceptance


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

    def test_standalone_download_checks_declared_length(self):
        api = assembly.ReadOnlyAPI('test-token')
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'artifact'
            for download in (False, True):
                for raw, headers, valid in (
                        (b'{}', {'Content-Length': '4'}, False),
                        (b'{}', {'Content-Length': '2'}, True),
                        (b'{}', {}, True)):
                    response = Response(raw)
                    response.headers = headers
                    with self.subTest(download=download, headers=headers), patch(
                            'urllib.request.OpenerDirector.open', return_value=response):
                        if valid:
                            result = api.get('/actions/runs/1', output if download else None)
                            if download:
                                self.assertEqual(output.read_bytes(), raw)
                                output.unlink()
                            else:
                                self.assertEqual(result, {})
                        else:
                            with self.assertRaisesRegex(ValueError, 'truncated'):
                                api.get('/actions/runs/1', output if download else None)
                            self.assertFalse(output.exists())

    def test_failed_downloads_allow_retry_without_removing_existing_files(self):
        transport = service.Transport('test-token')
        api = assembly.ReadOnlyAPI('test-token')
        clients = (
            lambda output: transport.request('user', maximum=4, output=output),
            lambda output: api.get('/actions/artifacts/1/zip', output),
        )
        with tempfile.TemporaryDirectory() as temporary, patch.object(assembly, 'MAX_BYTES', 4):
            output = Path(temporary) / 'artifact'
            for client in clients:
                for failure in ('truncated', 'read-error', 'interrupted', 'oversized'):
                    response = Response(b'12')
                    if failure == 'oversized':
                        response = Response(b'12345')
                        response.headers = {}
                    elif failure != 'truncated':
                        error = OSError('connection lost') if failure == 'read-error' else KeyboardInterrupt()
                        # Write a prefix before failing, as a real broken stream does.
                        response.read = Mock(side_effect=[b'12', error])
                        response.read1 = response.read
                    with self.subTest(client=client, failure=failure), patch(
                            'urllib.request.OpenerDirector.open', return_value=response):
                        with self.assertRaises((ValueError, OSError, KeyboardInterrupt)):
                            client(output)
                        self.assertFalse(output.exists())
                    with patch('urllib.request.OpenerDirector.open', return_value=Response(b'1234')):
                        client(output)
                    self.assertEqual(output.read_bytes(), b'1234')
                    with patch('urllib.request.OpenerDirector.open', return_value=Response(b'1234')):
                        with self.assertRaises(FileExistsError):
                            client(output)
                    self.assertEqual(output.read_bytes(), b'1234')
                    output.unlink()

    def test_standalone_slow_progress_cannot_extend_download_deadline(self):
        api = assembly.ReadOnlyAPI('test-token')
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'artifact'
            for destination in (None, output):
                response = Response(b'{}')
                response.headers = {}
                response.read1 = Mock(side_effect=[b'{', b'}'])
                with self.subTest(destination=destination), patch(
                        'urllib.request.OpenerDirector.open', return_value=response), patch(
                            'time.monotonic', side_effect=[0, 299, 301]):
                    with self.assertRaisesRegex(ValueError, 'deadline'):
                        api.get('/actions/runs/1', destination)
                self.assertFalse(output.exists())

    def test_rejected_extractions_are_retryable_and_preserve_existing_trees(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tar_path = root / 'collection.tar.gz'
            with tarfile.open(tar_path, 'w:gz') as stream:
                member = tarfile.TarInfo('tapp/good')
                member.size = 4
                stream.addfile(member, io.BytesIO(b'good'))
                member = tarfile.TarInfo('runtime/link')
                member.type = tarfile.SYMTYPE
                member.linkname = '/outside'
                stream.addfile(member)
            zip_path = root / 'artifact.zip'
            with zipfile.ZipFile(zip_path, 'w') as stream:
                stream.writestr('tilo-apps-x86_64.tar.gz', b'archive')
                stream.writestr('final-publication-receipt.json', b'{}')
                stream.writestr('assembly-provenance.json', b'{}')
            clients = (
                lambda output: assembly.extract_tar(tar_path, output),
                lambda output: acceptance.extract_artifact(
                    zip_path, output, 'x86_64', ('0' * 64, '1' * 64, 7)),
            )
            for client in clients:
                output = root / 'output'
                for attempt in range(2):
                    with self.subTest(client=client, attempt=attempt), self.assertRaises(ValueError):
                        client(output)
                    self.assertFalse(output.exists())
                output.mkdir()
                (output / 'existing').write_bytes(b'preserved')
                with self.assertRaises(FileExistsError):
                    client(output)
                self.assertEqual((output / 'existing').read_bytes(), b'preserved')
                (output / 'existing').unlink()
                output.rmdir()

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
