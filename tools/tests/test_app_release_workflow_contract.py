"""Execute the actual release invariant, including disconnected-builder negatives."""
from pathlib import Path
import hashlib
import re
import shlex
import subprocess
import tempfile
import textwrap
import unittest
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[2]


def bash_path(path: Path) -> str:
    """Return a path WSL bash and native Unix bash can both open."""
    rendered = path.resolve().as_posix()
    if re.match(r'^[A-Za-z]:/', rendered):
        return '/mnt/' + rendered[0].lower() + rendered[2:]
    return rendered


class AppReleaseWorkflowContract(unittest.TestCase):
    def test_each_job_checks_out_only_the_exact_event_commit(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        checkouts = source.split('uses: actions/checkout@')[1:]
        self.assertEqual(len(checkouts), 6)
        event_checkouts = 0
        main_checkouts = 0
        for checkout in checkouts:
            block = checkout.split('\n      - ', 1)[0]
            self.assertIn('fetch-depth: 1', block)
            self.assertNotIn('fetch-depth: 0', block)
            self.assertIn('persist-credentials: false', block)
            if 'ref: ${{ github.sha }}' in block:
                event_checkouts += 1
            elif 'ref: main' in block:
                main_checkouts += 1
            else:
                self.fail('checkout must select the exact event or fresh main tip')
        self.assertEqual(event_checkouts, 4)
        self.assertEqual(main_checkouts, 2)

        plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
        self.assertIn('    needs: coordinator-preguard', plan)
        self.assertIn('app_release_history.py \\\n              fetch-base "$1" "$GITHUB_SHA"', plan)
        self.assertNotIn('require-main-tip "$GITHUB_SHA"', plan)
        self.assertNotIn('+refs/heads/main:refs/remotes/origin/main', plan)
        builder = plan.index('      - name: Resolve one immutable builder for every architecture\n')
        preparation = plan.index('      - name: Prepare exact release comparison history\n')
        authorization = plan.index('      - name: Authorize current main for publication planning\n')
        guard = plan.index('      - name: Validate prepared nonpublishing coordinator qualification\n')
        selection = plan.index('      - name: Select only the affected apps or requested collection\n')
        self.assertLess(builder, preparation)
        self.assertLess(preparation, authorization)
        self.assertLess(authorization, guard)
        self.assertLess(preparation, guard)
        self.assertLess(guard, selection)
        preguard = source.split('\n  coordinator-preguard:\n', 1)[1].split('\n  plan:\n', 1)[0]
        self.assertIn('app_packaging_qualification.py --preflight', preguard)
        self.assertIn(
            'image: docker.io/library/python:3.13-alpine@sha256:'
            '1a63a53928ce53d2b0baf08092a703f4840ac5dfbd61fd48802dbf48e08c801e',
            preguard,
        )
        self.assertNotIn('public/runner-image', preguard)
        self.assertNotRegex(preguard, r'(?m)^\s+image: [^@\n]+:[^@\n]+$')
        self.assertNotRegex(preguard, r'(?m)^\s+credentials:\s*$')
        self.assertNotIn('REGISTRY_TOKEN', preguard)
        self.assertNotIn('secrets.', preguard)
        self.assertNotIn('fetch-base', preguard)
        self.assertNotIn('actions/checkout', preguard)
        self.assertNotIn('persist-credentials:', preguard)
        self.assertIn('SOURCE_TOKEN: ${{ github.token }}', preguard)
        self.assertEqual(preguard.count('\n      - name:'), 1)
        sanitized = preguard.split('          exec env -i \\\n', 1)[1]
        self.assertIn('app_packaging_qualification.py --preflight', sanitized)
        self.assertNotIn('SOURCE_TOKEN', sanitized)
        self.assertNotIn('github.token', sanitized)
        preparation_step = plan[preparation:authorization]
        self.assertIn("if: \"github.event_name != 'workflow_dispatch'", preparation_step)
        self.assertIn('SOURCE_TOKEN: ${{ github.token }}', preparation_step)
        self.assertIn('BASELINE_SHA: ${{ github.event.inputs.baseline_sha }}', preparation_step)
        self.assertNotIn('tools/', preparation_step)
        self.assertIn('/usr/bin/git fetch --no-tags --no-write-fetch-head', preparation_step)
        self.assertIn('GIT_CONFIG_KEY_0=http.https://code.sandbox.example/.extraheader', preparation_step)
        authorization_step = plan[authorization:guard]
        self.assertIn('SOURCE_TOKEN: ${{ github.token }}', authorization_step)
        self.assertNotIn('tools/', authorization_step)
        self.assertIn('/usr/bin/git ls-remote --exit-code --refs origin refs/heads/main', authorization_step)
        selection_step = plan[selection:]
        self.assertNotIn('SOURCE_TOKEN', selection_step)
        self.assertNotIn('github.token', selection_step)
        self.assertNotIn('if [[ $GITHUB_REF != refs/heads/main ]]; then', plan)
        publishing = plan.split('          if [[ ( $GITHUB_EVENT_NAME == push', 1)[1].split(
            "          else\n", 1
        )[0]
        self.assertNotIn('refs/tags/*', publishing)
        tag_validation = source.split('\n  tag-validation:\n', 1)[1].split(
            '\n  coordinator-preguard:\n', 1
        )[0]
        self.assertIn("if: startsWith(github.ref, 'refs/tags/')", tag_validation)
        self.assertIn('app tag publication is disabled', tag_validation)
        self.assertNotIn('actions/checkout', tag_validation)
        self.assertNotIn('tools/', tag_validation)
        self.assertNotIn('python', tag_validation)
        self.assertNotIn('secrets.', tag_validation)
        self.assertNotIn('enable-openid-connect', tag_validation)
        self.assertNotIn('container:', tag_validation)
        self.assertIn("  plan:\n    if: \"!startsWith(github.ref, 'refs/tags/')\"", source)
        authorization = source.split('\n  authorize-publish:\n', 1)[1].split(
            '\n  build:\n', 1
        )[0]
        self.assertIn("if: needs.plan.outputs.publish == 'true'", authorization)
        self.assertNotIn('secrets.', authorization)
        self.assertNotIn('enable-openid-connect', authorization)
        self.assertNotIn('container:', authorization)
        self.assertNotIn('tools/', authorization)
        self.assertIn('persist-credentials: false', authorization)
        self.assertIn('[[ $observed == "$GITHUB_SHA" ]]', authorization)
        final_authorization = source.split('\n  authorize-publish-final:\n', 1)[1].split(
            '\n  publish:\n', 1
        )[0]
        self.assertIn('needs: [plan, authorize-publish, build]', final_authorization)
        self.assertIn('persist-credentials: false', final_authorization)
        self.assertIn('[[ $observed == "$GITHUB_SHA" ]]', final_authorization)
        self.assertNotIn('tools/', final_authorization)
        self.assertNotIn('secrets.', final_authorization)
        self.assertNotIn('enable-openid-connect', final_authorization)
        build = source.split('\n  build:\n', 1)[1].split(
            '\n  final-collection:\n', 1
        )[0]
        revalidation = build.split(
            '      - name: Revalidate publication source immediately before signing\n', 1
        )[1].split('\n      - name:', 1)[0]
        self.assertIn('SOURCE_TOKEN: ${{ github.token }}', revalidation)
        self.assertNotIn('tools/', revalidation)
        self.assertIn('/usr/bin/git ls-remote --exit-code --refs origin refs/heads/main', revalidation)
        history_helper = (ROOT / 'tools/ci/app_release_history.py').read_text()
        self.assertNotIn('SOURCE_TOKEN', history_helper)
        self.assertNotIn('extraheader', history_helper)
        publish = source.split('\n  publish:\n', 1)[1]
        self.assertIn('needs: [plan, authorize-publish, build, authorize-publish-final]', publish)
        self.assertIn('persist-credentials: false', publish)
        self.assertNotIn('TAG_AUDIENCE', publish)
        self.assertIn('audience=$MAIN_AUDIENCE', publish)
        self.assertIn('git show -s --format=%ct "$GITHUB_SHA"', publish)

    def coordinator_bootstrap(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        script = source.split('# BEGIN coordinator preguard bootstrap\n', 1)[1].split(
            '          # END coordinator preguard bootstrap', 1)[0]
        namespace = {'__name__': 'contract_test'}
        exec(compile(textwrap.dedent(script), '<coordinator-bootstrap>', 'exec'), namespace)
        return namespace

    def test_coordinator_preguard_shell_is_syntactically_complete(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        preguard = source.split('\n  coordinator-preguard:\n', 1)[1].split('\n  plan:\n', 1)[0]
        script = textwrap.dedent(preguard.split('        run: |\n', 1)[1])
        subprocess.run(['bash', '-n'], input=script, text=True, check=True)

    def test_token_scoped_git_steps_are_syntactically_complete(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        for name in (
            'Prepare exact release comparison history',
            'Authorize current main for publication planning',
            'Revalidate publication source immediately before signing',
        ):
            with self.subTest(name=name):
                step = source.split(f'      - name: {name}\n', 1)[1].split(
                    '\n      - name:', 1
                )[0]
                script = textwrap.dedent(
                    step.split('        run: |\n', 1)[1]
                ).replace('\r', '')
                subprocess.run(['bash', '-n'], input=script.encode(), check=True)

    class Response:
        status = 200

        def __init__(self, url, payload):
            self.url = url
            self.payload = payload

        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def geturl(self):
            return self.url

        def read(self, size):
            return self.payload[:size]

    class Opener:
        def __init__(self, payload, response_url=None):
            self.payload = payload
            self.response_url = response_url
            self.request = None

        def open(self, request, timeout):
            self.request = request
            return AppReleaseWorkflowContract.Response(
                self.response_url or request.full_url, self.payload)

    def test_coordinator_bootstrap_manifest_matches_exact_runtime_closure(self):
        bootstrap = self.coordinator_bootstrap()
        self.assertEqual(
            [entry[0] for entry in bootstrap['FILES']],
            [
                'tools/app_packaging_qualification.py',
                'tools/app_packaging_contract.py',
                'tools/ci/__init__.py',
                'tools/ci/python.py',
                'tools/ci/resolve_builder.py',
            ],
        )
        for relative, size, digest in bootstrap['FILES']:
            payload = (ROOT / relative).read_bytes()
            self.assertEqual(len(payload), size, relative)
            self.assertEqual(hashlib.sha256(payload).hexdigest(), digest, relative)

    def test_coordinator_bootstrap_rejects_redirects(self):
        bootstrap = self.coordinator_bootstrap()
        with self.assertRaisesRegex(ValueError, 'redirects are forbidden'):
            bootstrap['NoRedirect']().redirect_request(None, None, 302, '', {}, 'https://elsewhere.invalid')

    def test_coordinator_bootstrap_rejects_oversize_source(self):
        bootstrap = self.coordinator_bootstrap()
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
            bootstrap['fetch_one'](
                self.Opener(b'ab'), Path(directory), 'a' * 40, 'token',
                'tools/example.py', 1, hashlib.sha256(b'a').hexdigest())

    def test_coordinator_bootstrap_rejects_hash_mismatch(self):
        bootstrap = self.coordinator_bootstrap()
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
            bootstrap['fetch_one'](
                self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                'tools/example.py', 1, hashlib.sha256(b'b').hexdigest())

    def test_coordinator_bootstrap_rejects_path_escape(self):
        bootstrap = self.coordinator_bootstrap()
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'invalid.*path'):
            bootstrap['fetch_one'](
                self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                'tools/../escape.py', 1, hashlib.sha256(b'a').hexdigest())

    def test_coordinator_bootstrap_requests_only_the_exact_ref(self):
        bootstrap = self.coordinator_bootstrap()
        payload = b'exact source'
        opener = self.Opener(payload)
        with tempfile.TemporaryDirectory() as directory:
            bootstrap['fetch_one'](
                opener, Path(directory), 'a' * 40, 'scoped-token',
                'tools/example.py', len(payload), hashlib.sha256(payload).hexdigest())
            parsed = urlparse(opener.request.full_url)
            self.assertEqual(parse_qs(parsed.query), {'ref': ['a' * 40]})
            self.assertEqual(opener.request.get_header('Authorization'), 'token scoped-token')
            self.assertEqual((Path(directory) / 'tools/example.py').read_bytes(), payload)

    def test_every_main_pr_plans_but_never_packages(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        self.assertIn('- "tools/apps/**"', source)
        pr = source.split('  pull_request:\n', 1)[1].split('\n  push:', 1)[0]
        self.assertRegex(pr, r'(?m)^    branches: \[main\]$')
        self.assertNotRegex(pr, r'(?m)^    (?:paths|paths-ignore):')
        plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
        self.assertIn("    if: \"!startsWith(github.ref, 'refs/tags/')\"", plan)
        self.assertIn('python3 -I tools/ci/python.py tools/apps/app_release.py "${args[@]}"', plan)
        build = source.split('\n  build:\n', 1)[1].split('\n  publish:\n', 1)[0]
        condition = re.search(r'(?m)^    if: (.+)$', build).group(1).strip('"')
        # Execute the production predicate, including both nonzero/zero plans.
        for event, ref_type, count, allowed in [
            ('pull_request', 'branch', '51', False), ('pull_request', 'branch', '0', False),
            ('workflow_dispatch', 'branch', '51', True), ('push', 'branch', '51', True),
            ('push', 'tag', '51', False), ('workflow_dispatch', 'branch', '0', False),
        ]:
            with self.subTest(event=event, ref_type=ref_type, count=count):
                predicate = condition.replace(
                    'github.event_name', shlex.quote(event)
                ).replace(
                    "!startsWith(github.ref, 'refs/tags/')",
                    shlex.quote(ref_type) + ' != "tag"',
                ).replace('needs.plan.outputs.count', shlex.quote(count))
                with tempfile.TemporaryDirectory(dir=ROOT) as directory:
                    script = Path(directory) / 'predicate.sh'
                    script.write_text(
                        '#!/usr/bin/env bash\n[[ ' + predicate + ' ]]\n', newline='\n'
                    )
                    result = subprocess.run(['bash', bash_path(script)])
                self.assertEqual(0 if allowed else 1, result.returncode)
        push = source.split('\n  push:\n', 1)[1].split('\n  workflow_dispatch:', 1)[0]
        self.assertRegex(push, r'(?m)^    branches: \[main\]$')
        self.assertIn('tags: ["apps-v*", "app-*-v*"]', push)

    def test_pr_plan_skips_builder_resolution_but_release_events_resolve(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
        builder = plan.split('        id: builder\n', 1)[1].split('\n      - ', 1)[0]
        self.assertIn('run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', builder)
        condition = re.search(r'(?m)^        if: (.+)$', builder).group(1).strip('"')
        # Execute the actual step predicate. Both publishing releases and the
        # nonpublishing coordinator dispatch still require immutable resolution.
        for event, ref_type, allowed in [('pull_request', 'branch', False),
                                         ('push', 'branch', True),
                                         ('push', 'tag', False),
                                         ('workflow_dispatch', 'branch', True)]:
            with self.subTest(event=event, ref_type=ref_type):
                predicate = condition.replace(
                    'github.event_name', shlex.quote(event)
                ).replace(
                    "!startsWith(github.ref, 'refs/tags/')",
                    shlex.quote(ref_type) + ' != "tag"',
                )
                with tempfile.TemporaryDirectory(dir=ROOT) as directory:
                    script = Path(directory) / 'predicate.sh'
                    script.write_text(
                        '#!/usr/bin/env bash\n[[ ' + predicate + ' ]]\n', newline='\n'
                    )
                    result = subprocess.run(['bash', bash_path(script)])
                self.assertEqual(0 if allowed else 1, result.returncode)
        selection = plan.split('        id: plan\n', 1)[1].split('\n      - ', 1)[0]
        self.assertNotIn('steps.builder.outputs', selection)
        provenance = plan.split('      - name: Retain trusted coordinator plan provenance\n', 1)[1]
        self.assertIn("if: github.event.inputs.coordinator_intent != ''", provenance)
        self.assertIn('RESOLVED_BUILDER: ${{ steps.builder.outputs.image }}', provenance)

    def test_signing_secrets_exist_only_on_publishing_builds(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        build = source.split('\n  build:\n', 1)[1].split('\n  publish:\n', 1)[0]
        self.assertIn("if: needs.plan.outputs.publish == 'true'", build)
        self.assertIn(
            "TAPP_PUBLISHER_IDENTITY: ${{ needs.plan.outputs.publish == 'true' && secrets.TAPP_PUBLISHER_IDENTITY || '' }}",
            build,
        )
        self.assertIn(
            "RUNTIME_BUNDLE_SIGNING_KEY: ${{ needs.plan.outputs.publish == 'true' && secrets.RUNTIME_BUNDLE_SIGNING_KEY || '' }}",
            build,
        )
        self.assertIn(
            '/usr/bin/git ls-remote --exit-code --refs origin refs/heads/main', build
        )

    def check(self, workflow):
        script = (ROOT / 'os/tests/release_pipeline_invariants.sh').read_text()
        start = script.index("grep -Fq 'dist/apps/app-collections.toml' \"$apps\"")
        end = script.index("\ngrep -Fq 'TILO_APP_COLLECTION_SHA256'", start)
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            path = Path(directory) / 'release-apps.yml'
            path.write_bytes(workflow.encode())
            contract = Path(directory) / 'contract.sh'
            contract.write_text(
                'set -euo pipefail\napps=' + shlex.quote(bash_path(path)) + '\n'
                'ok() { :; }\nbad() { exit 1; }\n' + script[start:end],
                newline='\n',
            )
            return subprocess.run(
                ['bash', bash_path(contract)], capture_output=True
            ).returncode

    def test_actual_immutable_builder_contract_passes(self):
        self.assertEqual(self.check((ROOT / '.forgejo/workflows/release-apps.yml').read_text()), 0)

    def test_crlf_and_large_job_do_not_create_false_failures(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        source = source.replace('    outputs:\n', '    # ' + 'x' * 131072 + '\n    outputs:\n', 1)
        self.assertEqual(self.check(source), 0)
        self.assertEqual(self.check(source.replace('\n', '\r\n')), 0)

    def test_publisher_does_not_receive_registry_token_in_steps(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        before, publish = source.split('\n  publish:\n', 1)
        publish = publish.replace('    steps:', '    steps:\n      - run: echo token\n        env:\n          FORBIDDEN: ${{ secrets.REGISTRY_TOKEN }}', 1)
        self.assertNotEqual(self.check(before + '\n  publish:\n' + publish), 0)

    def test_persisted_collection_urls_use_the_configured_public_origin(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        publish = source.split('\n  publish:\n', 1)[1]
        self.assertIn('TILO_PACKAGE_PUBLIC_BASE_URL: ${{ vars.TILO_PACKAGE_PUBLIC_BASE_URL }}', publish)
        self.assertIn('package_public_api=${TILO_PACKAGE_PUBLIC_BASE_URL%/}', publish)
        self.assertIn('bundle_url="$package_public_api/tilo-app-collections/', publish)
        self.assertIn('bundle_upload_url="$package_api/tilo-app-collections/', publish)
        self.assertIn('channel_url=$bundle_url', publish)
        self.assertIn('update_variable TILO_APP_COLLECTION_URL "$channel_url"', publish)
        self.assertNotIn('channel_url=$bundle_upload_url', publish)
        self.assertIn("$package_public_api != *'.svc.cluster.local'*", publish)

    def test_published_tapps_are_submitted_to_the_signed_registry(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        build, publish = source.split('\n  publish:\n', 1)
        self.assertIn('install -m 0755 target/release/tilo-app-package dist/app-release/tilo-app-package', build)
        self.assertIn('dist/app-release/tilo-app-package', build)
        self.assertIn(
            "TILO_REGISTRY_URL: ${{ vars.TILO_REGISTRY_URL || 'https://apps.tilo.so' }}",
            publish,
        )
        self.assertIn('TAPP_PUBLISHER_IDENTITY: ${{ secrets.TAPP_PUBLISHER_IDENTITY }}', publish)
        self.assertIn('publication_plan="$RUNNER_TEMP/tilo-registry-publication.tsv"', publish)
        self.assertIn("sort -t $'\\t' -k1,1 -k4,4", publish)
        self.assertIn("done < <(cut -f1 \"$publication_plan\" | uniq)", publish)
        self.assertIn('--metadata "$metadata" --action "$action" --url "$tapp_url"', publish)
        self.assertIn('args+=(--previous-entry "$aggregate")', publish)
        self.assertIn("jq -e -s '.[0] == .[1]'", publish)
        self.assertIn('tools/apps/registry_metadata.py build', publish)
        self.assertIn('tilo-app-metadata/$VERSION/$media', publish)
        self.assertIn('metadata="$registry_metadata/$name.json"', publish)
        self.assertIn('--metadata "$metadata"', publish)
        self.assertIn('--data-binary "@$request"', publish)
        self.assertIn('[[ $status == 202 ]]', publish)
        self.assertIn('$registry_url/artifacts/$owner/generic/tilo-app-$name/$VERSION/$file', publish)

    def test_verifier_package_secret_is_narrow_and_artifact_token_stays_job_scoped(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        binding = '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}'
        self.assertEqual(source.count(binding), 2)
        for occurrence in (0, 1):
            for replacement in ('${{ github.token }}', '${{ secrets.REGISTRY_TOKEN }}',
                                '${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN || secrets.REGISTRY_TOKEN }}'):
                with self.subTest(occurrence=occurrence, replacement=replacement):
                    parts = source.split(binding)
                    changed = binding.join(parts[:occurrence + 1]) + (
                        '          VERIFIER_BUNDLE_READ_TOKEN: ' + replacement
                    ) + binding.join(parts[occurrence + 1:])
                    self.assertNotEqual(self.check(changed), 0)
        artifact = '          FORGEJO_ARTIFACT_TOKEN: ${{ github.token }}'
        self.assertIn(artifact, source)
        self.assertNotEqual(self.check(source.replace(artifact,
            '          FORGEJO_ARTIFACT_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}')), 0)

    def test_disconnected_or_mutable_builder_fails(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        for before, after in (
            ('      builder_image: ${{ steps.builder.outputs.image }}', '      other: unused'),
            ('        id: builder', '        id: different'),
            ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', '        run: echo skipped'),
            ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py',
             '        run: python3 tools/ci/resolve_builder.py'),
            ('    needs: [plan, authorize-publish]', '    needs: other'),
            ('      image: ${{ needs.plan.outputs.builder_image }}',
             '      image: code.sandbox.example/qos/tilo-builder:latest'),
        ):
            with self.subTest(before=before):
                self.assertIn(before, source)
                self.assertNotEqual(self.check(source.replace(before, after, 1)), 0)

    def test_required_image_reference_in_comment_cannot_replace_build_input(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        reference = '      image: ${{ needs.plan.outputs.builder_image }}'
        self.assertNotEqual(self.check(source.replace(reference, '# ' + reference)), 0)


if __name__ == '__main__':
    unittest.main()
