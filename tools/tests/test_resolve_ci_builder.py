import importlib.util
import errno
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import socket
import ssl
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError, URLError

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('resolver', ROOT / 'tools/ci/resolve_builder.py')
resolver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resolver)


class BuilderResolutionTests(unittest.TestCase):
    def test_direct_execution_does_not_shadow_python_standard_library(self):
        env = dict(os.environ, REQUESTED_BUILDER='not-an-immutable-reference')
        result = subprocess.run(
            [sys.executable, str(ROOT / 'tools/ci/resolve_builder.py')],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, '::error::CI builder resolution failed (ValueError)\n')
        self.assertNotIn('Traceback', result.stderr)

    def test_index_is_read_anonymously_from_the_registry_without_a_redirect(self):
        self.assertEqual(resolver.MANIFEST_URL,
                         'https://registry.tilo.so/v2/tilo/tilo-builder/manifests/latest')
        self.assertEqual(resolver.IMAGE, 'registry.tilo.so/tilo/tilo-builder')

    def test_digest_is_the_hash_of_the_served_index(self):
        payload = json.dumps(self.index()).encode()
        self.assertEqual(resolver.resolve(fetch=lambda: payload),
                         resolver.IMAGE + '@sha256:' + hashlib.sha256(payload).hexdigest())
        changed = payload + b' '
        self.assertEqual(resolver.resolve(fetch=lambda: changed),
                         resolver.IMAGE + '@sha256:' + hashlib.sha256(changed).hexdigest(),
                         'a newly published builder must not be replaced by a cached digest')

    def test_ambiguous_or_invalid_manifest_fails_closed(self):
        valid = self.index()
        for document in (None, {}, [], {**valid, 'manifests': []},
                         {**valid, 'mediaType': 'application/vnd.oci.image.manifest.v1+json'},
                         {k: v for k, v in valid.items() if k != 'mediaType'}):
            with self.subTest(document=document), self.assertRaises(ValueError):
                resolver.resolve(fetch=lambda: json.dumps(document).encode())
        for payload in (b'{', b'\xff', 'text'):
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                resolver.resolve(fetch=lambda: payload)

    @staticmethod
    def index():
        return {'schemaVersion': 2, 'mediaType': 'application/vnd.oci.image.index.v1+json',
                'manifests': [{'digest': 'sha256:' + '1' * 64, 'platform': {'architecture': 'amd64', 'os': 'linux'}}]}

    def test_manual_override_never_calls_latest_api(self):
        image = resolver.IMAGE + '@sha256:' + 'b' * 64
        def unexpected_fetch():
            self.fail('manual immutable override queried mutable latest')
        self.assertEqual(resolver.resolve(image, fetch=unexpected_fetch), image)

    def test_workflow_routes_native_consumers_through_resolved_output(self):
        workflow = (ROOT / '.forgejo/workflows/check.yml').read_text()
        self.assertIn('run: python3 tools/ci/resolve_builder.py', workflow)
        self.assertEqual(workflow.count('image: ${{ needs.builder.outputs.image }}'), 3)
        security = workflow.split('\n  security:', 1)[1].split('\n  suites:', 1)[0]
        self.assertIn('image: registry.tilo.so/tilo/ci-fast@sha256:', security)
        self.assertNotIn('image=registry.tilo.so/tilo/tilo-builder:latest', workflow)


class MetadataTransportTests(unittest.TestCase):
    token = 'secret-token-sentinel'
    valid = json.dumps(BuilderResolutionTests.index()).encode()
    digest = hashlib.sha256(valid).hexdigest()

    def response(self, value=None):
        return io.BytesIO(self.valid if value is None else json.dumps(value).encode())

    def http_error(self, code):
        return HTTPError('https://secret-url-sentinel/', code, 'secret-body-sentinel',
                         {'Authorization': self.token}, io.BytesIO(b'secret-body-sentinel'))

    def test_transient_fault_sequences_retry_only_same_bounded_get(self):
        faults = [URLError(TimeoutError(self.token)), URLError(OSError(errno.ECONNRESET, self.token)),
                  URLError(socket.gaierror(socket.EAI_AGAIN, self.token)), self.http_error(429),
                  self.http_error(503), http.client.IncompleteRead(b'secret-body-sentinel')]
        for fault in faults:
            with self.subTest(fault=type(fault).__name__):
                opener = Mock()
                opener.open.side_effect = [fault, self.response()]
                with patch.object(resolver, 'build_opener', return_value=opener), \
                        patch.object(resolver.time, 'sleep') as sleep, \
                        patch('sys.stderr', new_callable=io.StringIO) as error_output:
                    self.assertEqual(resolver.resolve(), resolver.IMAGE + '@sha256:' + self.digest)
                self.assertEqual(opener.open.call_count, 2)
                sleep.assert_called_once_with(1)
                requests = [call.args[0] for call in opener.open.call_args_list]
                self.assertEqual([(r.full_url, r.header_items()) for r in requests[:1]],
                                 [(r.full_url, r.header_items()) for r in requests[1:]])
                self.assertEqual(requests[0].full_url, resolver.MANIFEST_URL)
                self.assertIsNone(requests[0].get_header('Authorization'))
                self.assertEqual(requests[0].get_method(), 'GET')
                self.assertIsNone(requests[0].data)
                self.assertEqual([call.kwargs for call in opener.open.call_args_list], [{'timeout': 30}]*2)
                for secret in (self.token, 'secret-url-sentinel', 'secret-body-sentinel'):
                    self.assertNotIn(secret, error_output.getvalue())

    def challenge(self, realm='https://registry.tilo.so/token',
                  scope='repository:tilo/tilo-builder:pull'):
        header = f'Bearer realm="{realm}",service="registry.tilo.so",scope="{scope}"'
        return HTTPError(resolver.MANIFEST_URL, 401, 'Unauthorized', {'WWW-Authenticate': header},
                         io.BytesIO(b''))

    def test_write_endpoint_challenge_takes_an_anonymous_pull_token(self):
        opener = Mock()
        opener.open.side_effect = [self.challenge(), io.BytesIO(b'{"token":"pull-token"}'),
                                   self.response()]
        with patch.object(resolver, 'build_opener', return_value=opener):
            self.assertEqual(resolver.resolve(), resolver.IMAGE + '@sha256:' + self.digest)
        token_request, retry = (call.args[0] for call in opener.open.call_args_list[1:])
        self.assertEqual(token_request.full_url, 'https://registry.tilo.so/token?service=registry.tilo.so'
                         '&scope=repository%3Atilo%2Ftilo-builder%3Apull')
        self.assertIsNone(token_request.get_header('Authorization'))
        self.assertEqual(retry.full_url, resolver.MANIFEST_URL)
        self.assertEqual(retry.get_header('Authorization'), 'Bearer pull-token')

    def test_foreign_token_realm_or_scope_is_refused(self):
        for fault in (self.challenge(realm='https://attacker.invalid/token'),
                      self.challenge(scope='repository:tilo/tilo-builder:push'),
                      self.http_error(401)):
            with self.subTest(fault=fault.headers):
                opener = Mock()
                opener.open.side_effect = [fault]
                with patch.object(resolver, 'build_opener', return_value=opener), \
                        patch.object(resolver.time, 'sleep') as sleep, \
                        self.assertRaisesRegex(ValueError, 'challenge'):
                    resolver.resolve()
                opener.open.assert_called_once()
                sleep.assert_not_called()

    def test_retry_limit_and_final_safe_http_category(self):
        opener = Mock()
        faults = [self.http_error(503) for _ in range(3)]
        opener.open.side_effect = faults
        with patch.object(resolver, 'build_opener', return_value=opener), \
                patch.object(resolver.time, 'sleep') as sleep, \
                patch.dict(os.environ, {'REQUESTED_BUILDER': ''}), \
                patch('sys.stderr', new_callable=io.StringIO) as errors:
            self.assertEqual(resolver.main(), 1)
        self.assertEqual(opener.open.call_count, 3)
        self.assertEqual([call.args for call in sleep.call_args_list], [(1,), (2,)])
        self.assertIn('CI builder resolution failed (HTTP_503)', errors.getvalue())
        self.assertEqual(errors.getvalue().count('Builder metadata retry'), 2)
        for secret in (self.token, 'secret-url-sentinel', 'secret-body-sentinel', 'Authorization'):
            self.assertNotIn(secret, errors.getvalue())
        self.assertTrue(all(error.fp.closed for error in faults))

    def test_interrupted_body_is_closed_and_retried_from_start(self):
        broken = io.BytesIO(b'partial-secret-body')
        opener = Mock()
        opener.open.side_effect = [broken, self.response()]
        with patch.object(broken, 'read', side_effect=ConnectionResetError(errno.ECONNRESET, self.token)), \
                patch.object(resolver, 'build_opener', return_value=opener), \
                patch.object(resolver.time, 'sleep') as sleep, patch('sys.stderr', new_callable=io.StringIO):
            self.assertEqual(resolver.fetch_manifest(), self.valid)
        self.assertTrue(broken.closed)
        self.assertEqual(opener.open.call_count, 2)
        sleep.assert_called_once_with(1)

    def test_auth_cert_redirect_and_unknown_transport_errors_never_retry(self):
        faults = [self.http_error(401), self.http_error(403), self.http_error(404), self.http_error(302),
                  URLError(ssl.SSLCertVerificationError(1, self.token)),
                  URLError(ssl.SSLError(1, self.token)),
                  URLError(socket.gaierror(socket.EAI_NONAME, self.token)),
                  URLError(self.token), OSError(errno.EACCES, self.token), ValueError(self.token)]
        for fault in faults:
            with self.subTest(fault=type(fault).__name__):
                opener = Mock()
                opener.open.side_effect = fault
                with patch.object(resolver, 'build_opener', return_value=opener), \
                        patch.object(resolver.time, 'sleep') as sleep, \
                        patch.dict(os.environ, {'REQUESTED_BUILDER': ''}), \
                        patch('sys.stderr', new_callable=io.StringIO) as errors:
                    self.assertEqual(resolver.main(), 1)
                opener.open.assert_called_once()
                sleep.assert_not_called()
                for secret in (self.token, 'secret-url-sentinel', 'secret-body-sentinel', 'Authorization'):
                    self.assertNotIn(secret, errors.getvalue())
        with self.assertRaisesRegex(ValueError, 'redirects are not accepted'):
            resolver.NoRedirect().redirect_request(None, None, 302, self.token, {}, 'https://secret/')

    def test_invalid_json_manifest_and_oversized_response_never_retry(self):
        for response in (io.BytesIO(b'{secret-body-sentinel'), self.response([]),
                         self.response({'schemaVersion': 2, 'manifests': []}),
                         io.BytesIO(b'x' * (resolver.MAX_RESPONSE + 1))):
            opener = Mock()
            opener.open.return_value = response
            with patch.object(resolver, 'build_opener', return_value=opener), \
                    patch.object(resolver.time, 'sleep') as sleep:
                with self.assertRaises(ValueError):
                    resolver.resolve()
            opener.open.assert_called_once()
            sleep.assert_not_called()

    def test_safe_errno_category_and_immutable_pin_never_opens_network(self):
        self.assertEqual(resolver.failure(URLError(OSError(errno.ECONNRESET, self.token))), ('OS_ECONNRESET', True))
        for name in ('ECONNRESET', 'ECONNREFUSED', 'ECONNABORTED', 'ENETUNREACH', 'EHOSTUNREACH', 'EPIPE'):
            with self.subTest(errno=name):
                self.assertEqual(resolver.failure(URLError(OSError(getattr(errno, name), self.token))),
                                 ('OS_' + name, True))
        self.assertEqual(resolver.failure(URLError(OSError(errno.ETIMEDOUT, self.token))), ('TIMEOUT', True))
        self.assertEqual(resolver.failure(URLError(ssl.SSLCertVerificationError(1, self.token))),
                         ('TLS_CERTIFICATE', False))
        with patch.object(resolver, 'build_opener') as build:
            image = resolver.IMAGE + '@sha256:' + 'a'*64
            self.assertEqual(resolver.resolve(requested=image), image)
            build.assert_not_called()


if __name__ == '__main__':
    unittest.main()
