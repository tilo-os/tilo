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
    def setUp(self):
        self.raw = json.dumps(
            {
                "schemaVersion": 2,
                "mediaType": "application/vnd.oci.image.index.v1+json",
                "manifests": [
                    {"digest": "sha256:" + "a" * 64, "platform": {"os": "linux", "architecture": "amd64"}},
                    {"digest": "sha256:" + "b" * 64, "platform": {"os": "linux", "architecture": "arm64"}},
                ],
            },
            separators=(",", ":"),
        ).encode()
        self.digest = "sha256:" + hashlib.sha256(self.raw).hexdigest()
        payload = base64.urlsafe_b64encode(
            json.dumps(
                {
                    "iss": MODULE.OIDC_ISSUER,
                    "sub": MODULE.OIDC_SUBJECT,
                    "aud": "tilo-builder-replication",
                }
            ).encode()
        ).decode().rstrip("=")
        self.token = "header." + payload + ".signature"
        self.calls = []
        self.aws_environments = []

    def execute(self, arguments, **kwargs):
        arguments = tuple(str(value) for value in arguments)
        self.calls.append(arguments)
        if arguments[:3] == ("skopeo", "inspect", "--raw"):
            return subprocess.CompletedProcess(arguments, 0, stdout=self.raw)
        if arguments[:3] == ("aws", "sts", "get-caller-identity"):
            self.aws_environments.append(kwargs["env"])
            return subprocess.CompletedProcess(
                arguments,
                0,
                stdout=json.dumps(
                    {
                        "Account": "125650147312",
                        "Arn": "arn:aws:sts::125650147312:assumed-role/burst-us-qos-builder-replication/run",
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
                    "arn:aws:iam::125650147312:role/burst-us-qos-builder-replication",
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
                self.assertEqual(aws_environment["AWS_ROLE_ARN"].rsplit("/", 1)[1], "burst-us-qos-builder-replication")
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
                with self.assertRaisesRegex(ValueError, "different index bytes"):
                    MODULE.replicate(
                        source,
                        "125650147312.dkr.ecr.us-west-2.amazonaws.com/qos/tilo-builder",
                        "arn:aws:iam::125650147312:role/burst-us-qos-builder-replication",
                        "us-west-2",
                        "tilo-builder-replication",
                        ROOT / "keys/cosign.pub",
                        Path(temporary) / "output",
                    )
            self.assertFalse(any(call[:2] == ("skopeo", "copy") for call in self.calls))

    def test_generic_mapping_preserves_external_index_without_claiming_qos_signature(self):
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
                    "arn:aws:iam::125650147312:role/burst-us-qos-builder-replication",
                    "us-west-2",
                    "tilo-builder-replication",
                    None,
                    output,
                )

            self.assertTrue(target.endswith("@" + self.digest))
            self.assertFalse(any(call[:2] == ("cosign", "verify") for call in self.calls))
            self.assertIn("--preserve-digests", next(call for call in self.calls if call[:2] == ("skopeo", "copy")))


if __name__ == "__main__":
    unittest.main()
