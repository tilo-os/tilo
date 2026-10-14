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
PUBLISH_SCRIPT = ROOT / 'tools/apps/publish-collections.sh'


def bash_path(path: Path) -> str:
    """Return a path WSL bash and native Unix bash can both open."""
    rendered = path.resolve().as_posix()
    if re.match(r'^[A-Za-z]:/', rendered):
        return '/mnt/' + rendered[0].lower() + rendered[2:]
    return rendered


class AppReleaseWorkflowContract(unittest.TestCase):
    def test_collection_address_identifies_assembled_bytes_for_the_same_source(self):
        source = PUBLISH_SCRIPT.read_text()
        start = source.index('  bundle_sha=$(sha256sum "$bundle"')
        end = source.index('  if [[ ${VERIFY_FINAL_RECEIPT', start)
        address = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / 'tilo-apps-x86_64.tar.gz'

            def urls(content):
                bundle.write_bytes(content)
                script = address + '\nprintf "%s\\n%s\\n" "$bundle_upload_url" "$bundle_url"\n'
                return subprocess.check_output(
                    ['bash', '-eu', '-c', script], text=True,
                    env={'PATH': '/usr/bin:/bin', 'bundle': bash_path(bundle),
                         'VERSION': '0.0.0-' + 'a' * 40,
                         'package_api': 'https://code.tilo.so/api/packages/tilo/generic',
                         'package_public_api': 'https://registry.tilo.so/artifacts/tilo/generic'},
                ).splitlines()

            first = urls(b'first selected app cohort')
            second = urls(b'second selected app cohort')
            self.assertNotEqual(first, second)
            self.assertEqual(first, urls(b'first selected app cohort'))
            for url in first:
                self.assertIn(hashlib.sha256(b'first selected app cohort').hexdigest(), url)
            self.assertNotIn('tilo-app-collections/$VERSION/', source)
            self.assertEqual(source.count('tilo-app-collections/$collection_version/'), 5)

    def test_jobs_check_out_the_exact_commit_with_complete_shell_steps(self):
        with self.subTest(phase='each job checks out only the exact event commit'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            checkouts = source.split('uses: actions/checkout@')[1:]
            # Signal adds one compilation and one publication checkout.
            self.assertEqual(len(checkouts), 11)
            for checkout in checkouts:
                block = checkout.split('\n      - ', 1)[0]
                self.assertIn('fetch-depth: 1', block)
                self.assertNotIn('fetch-depth: 0', block)
                self.assertIn('persist-credentials: false', block)
                if 'path: original-source' in block:
                    self.assertIn('ref: 919dfbd901dff103b79e3d0fdef9f4af4b2e24c5', block)
                    self.assertIn("if: github.event.inputs.publish == 'true'", block)
                else:
                    self.assertIn('ref: ${{ github.sha }}', block)
                self.assertNotIn('ref: main', block)

            plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
            self.assertIn('    needs: coordinator-preguard', plan)
            self.assertIn('app_release_history.py \\\n              fetch-base "$1" "$GITHUB_SHA"', plan)
            self.assertNotIn('require-main-tip "$GITHUB_SHA"', plan)
            self.assertNotIn('+refs/heads/main:refs/remotes/origin/main', plan)
            builder = plan.index('      - name: Resolve one immutable builder for every architecture\n')
            preparation = plan.index('      - name: Prepare exact release comparison history\n')
            guard = plan.index('      - name: Validate prepared nonpublishing coordinator qualification\n')
            selection = plan.index('      - name: Select only the affected apps or requested collection\n')
            self.assertLess(builder, preparation)
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
            preparation_step = plan[preparation:guard]
            self.assertIn("if: \"github.event_name != 'workflow_dispatch'", preparation_step)
            self.assertIn('SOURCE_TOKEN: ${{ github.token }}', preparation_step)
            self.assertIn('BASELINE_SHA: ${{ github.event.inputs.baseline_sha }}', preparation_step)
            self.assertNotIn('tools/', preparation_step)
            self.assertIn('/usr/bin/git fetch --no-tags --no-write-fetch-head', preparation_step)
            # The token is scoped to the job's own, validated server, and the
            # checkout must come from that server's copy of this repository.
            self.assertIn('[[ $GITHUB_SERVER_URL =~ ^https://[a-z0-9.-]+$ ]]', preparation_step)
            self.assertIn('$origin == "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY"', preparation_step)
            self.assertIn('GIT_CONFIG_KEY_0="http.$GITHUB_SERVER_URL/.extraheader"', preparation_step)
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
            self.assertIn("  plan:\n    if: \"!startsWith(github.ref, 'refs/tags/') && github.event.inputs.recover_97894 != 'true'\"", source)
            authorization = source.split('\n  authorize-publish:\n', 1)[1].split(
                '\n  build:\n', 1
            )[0]
            self.assertIn("if: needs.plan.outputs.publish == 'true'", authorization)
            self.assertNotIn('secrets.', authorization)
            self.assertNotIn('enable-openid-connect', authorization)
            self.assertNotIn('container:', authorization)
            self.assertNotIn('tools/', authorization)
            self.assertIn('persist-credentials: false', authorization)
            self.assertIn('ref: ${{ github.sha }}', authorization)
            self.assertIn('[[ $(git rev-parse HEAD) == "$GITHUB_SHA" ]]', authorization)
            self.assertIn('[[ $GITHUB_REF == refs/heads/main ]]', authorization)
            self.assertIn('[[ $GITHUB_EVENT_NAME == push || $GITHUB_EVENT_NAME == workflow_dispatch ]]', authorization)
            self.assertNotIn('authorize-publish-final:', source)
            build = source.split('\n  build:\n', 1)[1].split(
                '\n  final-collection:\n', 1
            )[0]
            self.assertNotIn('Revalidate publication source', build)
            self.assertNotIn('/usr/bin/git ls-remote --exit-code --refs origin refs/heads/main', build)
            self.assertIn("TILO_APP_RELEASE_SOURCE: ${{ needs.plan.outputs.publish == 'true' && github.sha || '' }}", build)
            self.assertIn("TILO_APP_RELEASE_SEQUENCE: ${{ needs.plan.outputs.publish == 'true' && github.run_number || '' }}", build)
            history_helper = (ROOT / 'tools/ci/app_release_history.py').read_text()
            self.assertNotIn('SOURCE_TOKEN', history_helper)
            self.assertNotIn('extraheader', history_helper)
            publish = source.split('\n  publish:\n', 1)[1] + PUBLISH_SCRIPT.read_text()
            self.assertIn('needs: [plan, authorize-publish, build]', publish)
            self.assertIn('persist-credentials: false', publish)
            self.assertNotIn('TAG_AUDIENCE', publish)
            self.assertIn('audience=$MAIN_AUDIENCE', publish)
            self.assertIn('git show -s --format=%ct "$GITHUB_SHA"', publish)
        with self.subTest(phase='coordinator preguard shell is syntactically complete'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            preguard = source.split('\n  coordinator-preguard:\n', 1)[1].split('\n  plan:\n', 1)[0]
            script = textwrap.dedent(preguard.split('        run: |\n', 1)[1])
            subprocess.run(['bash', '-n'], input=script, text=True, check=True)
        with self.subTest(phase='token scoped git steps are syntactically complete'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            for name in ('Prepare exact release comparison history',):
                with self.subTest(name=name):
                    step = source.split(f'      - name: {name}\n', 1)[1].split(
                        '\n      - name:', 1
                    )[0]
                    script = textwrap.dedent(
                        step.split('        run: |\n', 1)[1]
                    ).replace('\r', '')
                    subprocess.run(['bash', '-n'], input=script.encode(), check=True)

    def coordinator_bootstrap(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        script = source.split('# BEGIN coordinator preguard bootstrap\n', 1)[1].split(
            '          # END coordinator preguard bootstrap', 1)[0]
        namespace = {'__name__': 'contract_test'}
        exec(compile(textwrap.dedent(script), '<coordinator-bootstrap>', 'exec'), namespace)
        return namespace

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

    def test_coordinator_bootstrap_fetches_only_the_exact_bounded_closure(self):
        with self.subTest(phase='coordinator bootstrap manifest matches exact runtime closure'):
            bootstrap = self.coordinator_bootstrap()
            self.assertEqual(
                [entry[0] for entry in bootstrap['FILES']],
                [
                    'tools/app_packaging_qualification.py',
                    'tools/app_packaging_contract.py',
                    'tools/ci/__init__.py',
                    'tools/ci/python.py',
                    'tools/ci/resolve_builder.py',
                    'tools/ci/images.toml',
                ],
            )
            for relative, size, digest in bootstrap['FILES']:
                payload = (ROOT / relative).read_bytes()
                self.assertEqual(len(payload), size, relative)
                self.assertEqual(hashlib.sha256(payload).hexdigest(), digest, relative)
        with self.subTest(phase='coordinator bootstrap rejects redirects'):
            bootstrap = self.coordinator_bootstrap()
            with self.assertRaisesRegex(ValueError, 'redirects are forbidden'):
                bootstrap['NoRedirect']().redirect_request(None, None, 302, '', {}, 'https://elsewhere.invalid')
        with self.subTest(phase='coordinator bootstrap rejects oversize source'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
                bootstrap['fetch_one'](
                    self.Opener(b'ab'), Path(directory), 'a' * 40, 'token',
                    'tools/example.py', 1, hashlib.sha256(b'a').hexdigest())
        with self.subTest(phase='coordinator bootstrap rejects hash mismatch'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
                bootstrap['fetch_one'](
                    self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                    'tools/example.py', 1, hashlib.sha256(b'b').hexdigest())
        with self.subTest(phase='coordinator bootstrap rejects path escape'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'invalid.*path'):
                bootstrap['fetch_one'](
                    self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                    'tools/../escape.py', 1, hashlib.sha256(b'a').hexdigest())
        with self.subTest(phase='coordinator bootstrap requests only the exact ref'):
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

    def test_pull_requests_start_no_jobs_and_never_package(self):
        with self.subTest(phase='prs start nothing; packaging excludes them regardless'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            self.assertIn('- "tools/apps/**"', source)
            triggers = source.split('\non:\n', 1)[1].split('\nconcurrency:', 1)[0]
            self.assertNotRegex(triggers, r'(?m)^  pull_request(?:_target)?:')
            plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
            self.assertIn("    if: \"!startsWith(github.ref, 'refs/tags/') && github.event.inputs.recover_97894 != 'true'\"", plan)
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
        with self.subTest(phase='every plan resolves one builder'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            plan = source.split('\n  plan:\n', 1)[1].split('\n  authorize-publish:\n', 1)[0]
            builder = plan.split('        id: builder\n', 1)[1].split('\n      - ', 1)[0]
            self.assertIn('run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', builder)
            self.assertNotRegex(builder, r'(?m)^        if:')
            # Tags never reach the plan job, so it resolves for every event it runs on.
            self.assertIn("    if: \"!startsWith(github.ref, 'refs/tags/') && github.event.inputs.recover_97894 != 'true'\"", plan)
            selection = plan.split('        id: plan\n', 1)[1].split('\n      - ', 1)[0]
            # Only main pushes compare the resolved builder with the prior
            # collection's builder.
            self.assertEqual(1, selection.count('steps.builder.outputs'))
            self.assertIn('BUILDER: ${{ steps.builder.outputs.image }}', selection)
            pushed = selection.split('if [[ $GITHUB_EVENT_NAME == push ]]', 1)
            self.assertEqual(2, len(pushed))
            self.assertNotIn('"${BUILDER:-}"', pushed[0])
            provenance = plan.split('      - name: Retain trusted coordinator plan provenance\n', 1)[1]
            self.assertIn("if: github.event.inputs.coordinator_intent != ''", provenance)
            self.assertIn('RESOLVED_BUILDER: ${{ steps.builder.outputs.image }}', provenance)
        with self.subTest(phase='signing secrets exist only on publishing builds'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            build = source.split('\n  build:\n', 1)[1].split('\n  publish:\n', 1)[0]
            self.assertIn(
                "TAPP_PUBLISHER_IDENTITY: ${{ needs.plan.outputs.publish == 'true' && secrets.TAPP_PUBLISHER_IDENTITY || '' }}",
                build,
            )
            self.assertIn(
                "RUNTIME_BUNDLE_SIGNING_KEY: ${{ needs.plan.outputs.publish == 'true' && secrets.RUNTIME_BUNDLE_SIGNING_KEY || '' }}",
                build,
            )
            self.assertNotIn(
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
                'apps_publish=' + shlex.quote(bash_path(PUBLISH_SCRIPT)) + '\n'
                'ok() { :; }\nbad() { exit 1; }\n' + script[start:end],
                newline='\n',
            )
            return subprocess.run(
                ['bash', bash_path(contract)], capture_output=True
            ).returncode

    def test_builder_contract_requires_a_connected_immutable_image_input(self):
        with self.subTest(phase='actual immutable builder contract passes'):
            self.assertEqual(self.check((ROOT / '.forgejo/workflows/release-apps.yml').read_text()), 0)
        with self.subTest(phase='crlf and large job do not create false failures'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            source = source.replace('    outputs:\n', '    # ' + 'x' * 131072 + '\n    outputs:\n', 1)
            self.assertEqual(self.check(source), 0)
            self.assertEqual(self.check(source.replace('\n', '\r\n')), 0)
        with self.subTest(phase='disconnected or mutable builder fails'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            for before, after in (
                ('      builder_image: ${{ steps.builder.outputs.image }}', '      other: unused'),
                ('        id: builder', '        id: different'),
                ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py', '        run: echo skipped'),
                ('        run: python3 -I tools/ci/python.py tools/ci/resolve_builder.py',
                 '        run: python3 tools/ci/resolve_builder.py'),
                ('    needs: [plan, authorize-publish]', '    needs: other'),
                ('      image: ${{ needs.plan.outputs.builder_image }}',
                 '      image: registry.tilo.so/tilo/tilo-builder:latest'),
            ):
                with self.subTest(before=before):
                    self.assertIn(before, source)
                    self.assertNotEqual(self.check(source.replace(before, after, 1)), 0)
        with self.subTest(phase='required image reference in comment cannot replace build input'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            reference = '      image: ${{ needs.plan.outputs.builder_image }}'
            self.assertNotEqual(self.check(source.replace(reference, '# ' + reference)), 0)

    def test_publication_uses_narrow_secrets_public_origin_and_signed_registry(self):
        with self.subTest(phase='publisher does not receive registry token in steps'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            before, publish = source.split('\n  publish:\n', 1)
            publish = publish.replace('    steps:', '    steps:\n      - run: echo token\n        env:\n          FORBIDDEN: ${{ secrets.REGISTRY_TOKEN }}', 1)
            self.assertNotEqual(self.check(before + '\n  publish:\n' + publish), 0)
        with self.subTest(phase='persisted collection urls use the configured public origin'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            publish = source.split('\n  publish:\n', 1)[1] + PUBLISH_SCRIPT.read_text()
            self.assertIn('TILO_PACKAGE_PUBLIC_BASE_URL: ${{ vars.TILO_PACKAGE_PUBLIC_BASE_URL }}', publish)
            self.assertIn('package_public_api=${TILO_PACKAGE_PUBLIC_BASE_URL%/}', publish)
            self.assertIn('bundle_url="$package_public_api/tilo-app-collections/', publish)
            self.assertIn('bundle_upload_url="$package_api/tilo-app-collections/', publish)
            self.assertIn('channel_url=$bundle_url', publish)
            self.assertIn('update_variable TILO_APP_COLLECTION_URL "$channel_url"', publish)
            self.assertNotIn('channel_url=$bundle_upload_url', publish)
            self.assertIn("$package_public_api != *'.svc.cluster.local'*", publish)
        with self.subTest(phase='published tapps are submitted to the signed registry'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            build, publish = source.split('\n  publish:\n', 1)
            publish += PUBLISH_SCRIPT.read_text()
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
            self.assertIn('registry_media_urls="$RUNNER_TEMP/tilo-registry-media-public-urls.tsv"', publish)
            self.assertIn('tilo-app-metadata/$VERSION/$media" >> "$registry_media_urls"', publish)
        with self.subTest(phase='every uploaded tapp has a compressed twin beside it'):
            script = PUBLISH_SCRIPT.read_text()
            self.assertIn('"$registry_cli" compress -o "$twins/$file.zst" "$tapp"', script)
            self.assertIn('"$package_api/tilo-app-$name/$VERSION/$file.zst" >> "$upload_manifest"', script)
            # The twin is written before the upload that needs it.
            self.assertLess(script.index('registry_cli="${index%/index.json}/tilo-app-package"'),
                            script.index('"$registry_cli" compress'))
            self.assertLess(script.index('"$registry_cli" compress'),
                            script.index('tools/upload_immutable_packages.sh "$upload_manifest"'))
            self.assertIn('public registry media is unavailable or has the wrong type', publish)
            self.assertIn('curl --silent --show-error --location --head --dump-header "$headers"', publish)
            # Archived screenshots are downloaded and digest-checked; a single
            # gateway timeout from the proxy must not fail the publication.
            self.assertIn('--connect-timeout 10 --max-time 180 --retry 4 --retry-delay 2 \\\n'
                          '      "$media_url" --output "$archived_file"', publish)
            self.assertIn("[[ ${actual_sha%% *} == \"$version\" ]]", publish)
            self.assertIn("*.svg) expected_type='image/svg+xml'", publish)
            self.assertIn("*.webp) expected_type='image/webp'", publish)
            # Every media URL is checked, eight at a time, and any failure fails.
            self.assertIn('tr \'\\t\' \'\\n\' < "$registry_media_urls"', publish)
            self.assertIn("| xargs -d '\\n' -n 2 -P 8 bash -c 'check_media \"$1\" \"$2\"' _", publish)
            self.assertIn('metadata="$registry_metadata/$name.json"', publish)
            self.assertIn('--metadata "$metadata"', publish)
            self.assertIn('--data-binary "@$request"', publish)
            self.assertIn('[[ $status == 202 ]]', publish)
            self.assertIn('$registry_url/artifacts/$owner/generic/tilo-app-$name/$VERSION/$file', publish)
        with self.subTest(phase='verifier package secret is narrow and artifact token stays job scoped'):
            source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
            binding = '          VERIFIER_BUNDLE_READ_TOKEN: ${{ secrets.TILO_VERIFIER_BUNDLE_READ_TOKEN }}'
            self.assertEqual(source.count(binding), 3)
            for occurrence in (0, 1, 2):
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


if __name__ == '__main__':
    unittest.main()
