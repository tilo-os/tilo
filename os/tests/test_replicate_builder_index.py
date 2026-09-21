import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "replicate_builder_index", ROOT / "os/dev/replicate-builder-index.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Response:
    def __init__(self, document):
        self.document = document

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False

    def read(self):
        return json.dumps(self.document).encode()


class RegionalBuilderReplicationTest(unittest.TestCase):
    @staticmethod
    def token_for(**overrides):
        claims = {
            "iss": MODULE.OIDC_ISSUER,
            "sub": MODULE.OIDC_SUBJECT,
            "aud": "tilo-builder-replication",
        }
        claims.update(overrides)
        payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
        return "header." + payload + ".signature"

    def setUp(self):
        self.raw = json.dumps(
            {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "manifests": [
                    {
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                        "size": 123,
                        "digest": "sha256:" + "a" * 64,
                        "platform": {"os": "linux", "architecture": "amd64"},
                    },
                    {
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                        "size": 124,
                        "digest": "sha256:" + "b" * 64,
                        "platform": {"os": "linux", "architecture": "arm64"},
                    },
                ],
            },
            separators=(",", ":"),
        ).encode()
        self.digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
        self.token = self.token_for()
        self.calls = []
        self.aws_environments = []
        self.cosign_auth = []
        self.target_raw = None

    def execute(self, arguments, **kwargs):
        arguments = tuple(str(value) for value in arguments)
        self.calls.append(arguments)
        if arguments[:3] == ("skopeo", "inspect", "--raw"):
            if self.target_raw is not None and ".dkr.ecr." in arguments[-1]:
                return subprocess.CompletedProcess(arguments, 0, stdout=self.target_raw)
            return subprocess.CompletedProcess(arguments, 0, stdout=self.raw)
        if arguments[:2] == ("cosign", "verify") and kwargs.get("env") is not None:
            config = Path(kwargs["env"]["DOCKER_CONFIG"]) / "config.json"
            self.cosign_auth.append((config.read_text(), config))
        if arguments[:3] == ("aws", "sts", "get-caller-identity"):
            self.aws_environments.append(kwargs["env"])
            return subprocess.CompletedProcess(
                arguments,
                0,
                stdout=json.dumps(
                    {
                        "Account": "125650147312",
                        "Arn": "arn:aws:sts::125650147312:assumed-role/burst-us-tilo-builder-replication/run",
                    }
                ),
            )
        if arguments[:3] == ("aws", "ecr", "get-login-password"):
            self.aws_environments.append(kwargs["env"])
            return subprocess.CompletedProcess(arguments, 0, stdout="short-lived-password\n")
        if arguments[:3] == ("aws", "ecr", "batch-get-image"):
            self.aws_environments.append(kwargs["env"])
            return subprocess.CompletedProcess(arguments, 0, stdout="0\n")
        return subprocess.CompletedProcess(arguments, 0, stdout="")

    def test_replication_preserves_digest_and_isolates_short_lived_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "builder-index.txt"
            output = Path(temporary) / "replication.txt"
            source.write_text(MODULE.SOURCE_REPOSITORY + "@" + self.digest + "\n")
            environment = {
                "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token?x=1",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                "AWS_ACCESS_KEY_ID": "ambient",
                "AWS_SECRET_ACCESS_KEY": "ambient-secret",
                "GITHUB_RUN_ID": "1234",
            }
            with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                MODULE.urllib.request,
                "urlopen",
                return_value=Response({"value": self.token}),
            ) as open_token, patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                target = MODULE.replicate(
                    source,
                    "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder",
                    "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                    "us-west-2",
                    "tilo-builder-replication",
                    ROOT / "keys/cosign.pub",
                    output,
                )

            request = open_token.call_args.args[0]
            self.assertEqual(request.get_header("Authorization"), "bearer request-secret")
            self.assertIn("audience=tilo-builder-replication", request.full_url)
            self.assertEqual(target.rsplit("@", 1)[1], self.digest)
            self.assertIn("--preserve-digests", next(call for call in self.calls if call[:2] == ("skopeo", "copy")))
            self.assertIn(("cosign", "verify", "--key"), [call[:3] for call in self.calls])
            for aws_environment in self.aws_environments:
                self.assertNotIn("AWS_ACCESS_KEY_ID", aws_environment)
                self.assertNotIn("AWS_SECRET_ACCESS_KEY", aws_environment)
                self.assertEqual(aws_environment["AWS_ROLE_ARN"].rsplit("/", 1)[1], "burst-us-tilo-builder-replication")
                self.assertFalse(Path(aws_environment["AWS_WEB_IDENTITY_TOKEN_FILE"]).exists())
            receipt = output.read_text()
            self.assertIn("source=" + MODULE.SOURCE_REPOSITORY + "@" + self.digest, receipt)
            self.assertIn("target=" + target, receipt)
            self.assertNotIn(self.token, receipt)
            self.assertNotIn("short-lived-password", receipt)

    def test_replication_rejects_source_bytes_that_do_not_match_the_promoted_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "builder-index.txt"
            source.write_text(MODULE.SOURCE_REPOSITORY + "@sha256:" + "f" * 64 + "\n")
            with patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                with self.assertRaisesRegex(ValueError, "different manifest bytes"):
                    MODULE.replicate(
                        source,
                        "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder",
                        "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                        "us-west-2",
                        "tilo-builder-replication",
                        ROOT / "keys/cosign.pub",
                        Path(temporary) / "output",
                    )
            self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))

    def test_oidc_denials_happen_before_aws_credentials_are_requested(self):
        cases = (
            ("missing environment", {}, self.token, "environment"),
            (
                "malformed token",
                {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                },
                "not-a-jwt",
                "malformed",
            ),
            (
                "wrong issuer",
                {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                },
                self.token_for(iss="https://attacker.invalid"),
                "issuer or subject",
            ),
            (
                "wrong subject",
                {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                },
                self.token_for(sub="repo:other/repo:ref:refs/heads/main"),
                "issuer or subject",
            ),
            (
                "wrong audience",
                {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                },
                self.token_for(aud="another-service"),
                "audience",
            ),
        )
        for name, environment, token, message in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                self.calls.clear()
                source = Path(temporary) / "python-index.txt"
                source.write_text("docker.io/library/python@" + self.digest + "\n")
                with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                    MODULE.urllib.request, "urlopen", return_value=Response({"value": token})
                ), patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                    with self.assertRaisesRegex(ValueError, message):
                        MODULE.replicate(
                            source,
                            "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                            "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                            "us-west-2",
                            "tilo-builder-replication",
                            None,
                            Path(temporary) / "output",
                        )
                self.assertFalse(any(call[:2] == ("aws", "sts") for call in self.calls))
                self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))

    def test_unexpected_aws_identity_stops_before_ecr_or_copy(self):
        identities = (
            {"Account": "000000000000", "Arn": "arn:aws:sts::000000000000:assumed-role/wrong/run"},
            {"Account": "125650147312", "Arn": "arn:aws:iam::125650147312:user/not-a-role"},
            {"Account": "125650147312"},
        )
        for identity in identities:
            with self.subTest(identity=identity), tempfile.TemporaryDirectory() as temporary:
                self.calls.clear()
                source = Path(temporary) / "python-index.txt"
                source.write_text("docker.io/library/python@" + self.digest + "\n")
                environment = {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                }

                def execute(arguments, **kwargs):
                    arguments = tuple(str(value) for value in arguments)
                    if arguments[:3] == ("aws", "sts", "get-caller-identity"):
                        self.calls.append(arguments)
                        return subprocess.CompletedProcess(arguments, 0, stdout=json.dumps(identity))
                    return self.execute(arguments, **kwargs)

                with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                    MODULE.urllib.request, "urlopen", return_value=Response({"value": self.token})
                ), patch.object(MODULE.subprocess, "run", side_effect=execute):
                    with self.assertRaisesRegex(ValueError, "unexpected replication identity"):
                        MODULE.replicate(
                            source,
                            "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                            "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                            "us-west-2",
                            "tilo-builder-replication",
                            None,
                            Path(temporary) / "output",
                        )
                self.assertFalse(any(call[:2] == ("aws", "ecr") for call in self.calls))
                self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))

    def test_source_mapping_and_regional_scope_reject_before_external_commands(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source.txt"
            source.write_text("code.sandbox.example/qos/unknown@" + self.digest + "\n")
            with self.assertRaisesRegex(ValueError, "no configured regional repository"):
                MODULE.replicate(
                    source,
                    "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/unknown",
                    "arn:aws:iam::125650147312:role/replicator",
                    "us-west-2",
                    "tilo-builder-replication",
                    None,
                    Path(temporary) / "output",
                )
            source.write_text("docker.io/library/python@" + self.digest + "\n")
            with self.assertRaisesRegex(ValueError, "no configured regional repository"):
                MODULE.replicate(
                    source,
                    "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder",
                    "arn:aws:iam::125650147312:role/replicator",
                    "us-west-2",
                    "tilo-builder-replication",
                    None,
                    Path(temporary) / "output",
                )
        for role, repository, region, message in (
            ("not-an-arn", "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder", "us-west-2", "invalid"),
            ("arn:aws:iam::125650147312:role/replicator", "bad.example/qos/tilo-builder", "us-west-2", "invalid"),
            ("arn:aws:iam::000000000000:role/replicator", "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder", "us-west-2", "do not agree"),
            ("arn:aws:iam::125650147312:role/replicator", "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder", "eu-central-1", "do not agree"),
        ):
            with self.subTest(role=role, repository=repository, region=region), self.assertRaisesRegex(ValueError, message):
                MODULE.regional_identity(role, repository, region)
        self.assertEqual(self.calls, [])

    def test_existing_target_is_verified_without_copy_and_bad_ecr_counts_fail_closed(self):
        for existing, succeeds in (("1", True), ("2", False)):
            with self.subTest(existing=existing), tempfile.TemporaryDirectory() as temporary:
                self.calls.clear()
                source = Path(temporary) / "python-index.txt"
                source.write_text("docker.io/library/python@" + self.digest + "\n")
                environment = {
                    "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                    "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
                }

                def execute(arguments, **kwargs):
                    arguments = tuple(str(value) for value in arguments)
                    if arguments[:3] == ("aws", "ecr", "batch-get-image"):
                        self.calls.append(arguments)
                        return subprocess.CompletedProcess(arguments, 0, stdout=existing + "\n")
                    return self.execute(arguments, **kwargs)

                with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                    MODULE.urllib.request, "urlopen", return_value=Response({"value": self.token})
                ), patch.object(MODULE.subprocess, "run", side_effect=execute):
                    if succeeds:
                        target = MODULE.replicate(
                            source,
                            "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                            "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                            "us-west-2",
                            "tilo-builder-replication",
                            None,
                            Path(temporary) / "output",
                        )
                        self.assertTrue(target.endswith("@" + self.digest))
                    else:
                        with self.assertRaisesRegex(ValueError, "invalid image-existence result"):
                            MODULE.replicate(
                                source,
                                "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                                "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                                "us-west-2",
                                "tilo-builder-replication",
                                None,
                                Path(temporary) / "output",
                            )
                self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))

    def test_generic_mapping_preserves_external_index_without_claiming_tilo_signature(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "python-index.txt"
            output = Path(temporary) / "replication.txt"
            source.write_text("docker.io/library/python@" + self.digest + "\n")
            environment = {
                "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
            }
            with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                MODULE.urllib.request,
                "urlopen",
                return_value=Response({"value": self.token}),
            ), patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                target = MODULE.replicate(
                    source,
                    "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                    "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                    "us-west-2",
                    "tilo-builder-replication",
                    None,
                    output,
                )

            self.assertTrue(target.endswith("@" + self.digest))
            self.assertFalse(any(call[:2] == ("cosign", "verify") for call in self.calls))
            self.assertIn("--preserve-digests", next(call for call in self.calls if call[:2] == ("skopeo", "copy")))

    def test_private_single_image_manifest_uses_explicit_source_auth(self):
        self.raw = json.dumps(
            {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "config": {
                    "mediaType": "application/vnd.oci.image.config.v1+json",
                    "size": 321,
                    "digest": "sha256:" + "c" * 64,
                },
                "layers": [
                    {
                        "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                        "size": 654,
                        "digest": "sha256:" + "d" * 64,
                    }
                ],
            },
            separators=(",", ":"),
        ).encode()
        digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "registry-server.txt"
            source_auth = Path(temporary) / "forgejo-auth.json"
            output = Path(temporary) / "replication.txt"
            source.write_text("code.sandbox.example/qos/tilo-registry-server@" + digest + "\n")
            source_auth.write_text('{"auths":{"code.sandbox.example":{}}}')
            environment = {
                "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
            }
            with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                MODULE.urllib.request,
                "urlopen",
                return_value=Response({"value": self.token}),
            ), patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                target = MODULE.replicate(
                    source,
                    "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-registry-server",
                    "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                    "us-west-2",
                    "tilo-builder-replication",
                    ROOT / "keys/cosign.pub",
                    output,
                    source_auth,
                )

            source_inspect = next(
                call
                for call in self.calls
                if call[:3] == ("skopeo", "inspect", "--raw") and "code.sandbox.example" in call[-1]
            )
            copy = next(call for call in self.calls if call[:2] == ("skopeo", "copy"))
            self.assertIn(("--authfile", str(source_auth)), list(zip(source_inspect, source_inspect[1:])))
            self.assertIn(("--src-authfile", str(source_auth)), list(zip(copy, copy[1:])))
            self.assertIn("--preserve-digests", copy)
            self.assertTrue(target.endswith("@" + digest))
            self.assertEqual(self.cosign_auth[0][0], source_auth.read_text())
            self.assertFalse(self.cosign_auth[0][1].exists())

    def test_index_requires_an_identifying_media_type(self):
        descriptor = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "size": 1,
            "digest": "sha256:" + "a" * 64,
        }
        with self.assertRaisesRegex(ValueError, "unsupported media type"):
            MODULE.validate_manifest(
                {"schemaVersion": 2, "manifests": [descriptor]},
                "code.sandbox.example/qos/tilo-registry-server",
            )

    def test_builder_requires_exactly_two_children_but_generic_indexes_do_not(self):
        descriptor = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "size": 1,
            "digest": "sha256:" + "a" * 64,
        }
        for count in (1, 3):
            document = {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "manifests": [descriptor] * count,
            }
            with self.subTest(count=count), self.assertRaisesRegex(ValueError, "two-platform index"):
                MODULE.validate_manifest(document, MODULE.SOURCE_REPOSITORY)
            MODULE.validate_manifest(document, "code.sandbox.example/qos/tilo-registry-server")

    def test_rejects_ambiguous_and_unknown_manifest_shapes(self):
        descriptor = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "size": 1,
            "digest": "sha256:" + "a" * 64,
        }
        with self.assertRaisesRegex(ValueError, "ambiguous manifest shape"):
            MODULE.validate_manifest(
                {
                    "schemaVersion": 2,
                    "manifests": [descriptor],
                    "config": descriptor,
                    "layers": [],
                },
                "code.sandbox.example/qos/tilo-registry-server",
            )
        with self.assertRaisesRegex(ValueError, "neither index nor image-manifest"):
            MODULE.validate_manifest(
                {"schemaVersion": 2},
                "code.sandbox.example/qos/tilo-registry-server",
            )

    def test_rejects_target_manifest_bytes_changed_after_copy(self):
        self.target_raw = self.raw + b"\n"
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "python-index.txt"
            source.write_text("docker.io/library/python@" + self.digest + "\n")
            environment = {
                "ACTIONS_ID_TOKEN_REQUEST_URL": "https://code.invalid/token",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "request-secret",
            }
            with patch.dict(MODULE.os.environ, environment, clear=True), patch.object(
                MODULE.urllib.request,
                "urlopen",
                return_value=Response({"value": self.token}),
            ), patch.object(MODULE.subprocess, "run", side_effect=self.execute):
                with self.assertRaisesRegex(ValueError, "changed the promoted OCI identity"):
                    MODULE.replicate(
                        source,
                        "125650147312.dkr.ecr.us-west-2.amazonaws.com/dockerhub/library/python",
                        "arn:aws:iam::125650147312:role/burst-us-tilo-builder-replication",
                        "us-west-2",
                        "tilo-builder-replication",
                        None,
                        Path(temporary) / "output",
                    )

    def test_builder_rejects_single_image_manifest(self):
        document = {
            "schemaVersion": 2,
            "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
            "config": {
                "mediaType": "application/vnd.docker.container.image.v1+json",
                "size": 1,
                "digest": "sha256:" + "c" * 64,
            },
            "layers": [],
        }
        with self.assertRaisesRegex(ValueError, "two-platform index"):
            MODULE.validate_manifest(document, MODULE.SOURCE_REPOSITORY)

    def test_accepts_oci_and_docker_index_and_image_media_types(self):
        descriptor = {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "size": 1,
            "digest": "sha256:" + "a" * 64,
        }
        for media_type in (
            "application/vnd.oci.image.index.v1+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
        ):
            with self.subTest(media_type=media_type):
                MODULE.validate_manifest(
                    {"schemaVersion": 2, "mediaType": media_type, "manifests": [descriptor]},
                    "code.sandbox.example/qos/tilo-registry-server",
                )
        for media_type in (
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
            None,
        ):
            with self.subTest(media_type=media_type):
                document = {"schemaVersion": 2, "config": descriptor, "layers": [descriptor]}
                if media_type is not None:
                    document["mediaType"] = media_type
                MODULE.validate_manifest(document, "code.sandbox.example/qos/tilo-registry-server")


if __name__ == "__main__":
    unittest.main()
