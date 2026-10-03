import importlib.util
import io
import json
from pathlib import Path
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import call, patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev" / "require-private-oci-owner.py"
WORKFLOW = Path(__file__).resolve().parents[2] / ".forgejo" / "workflows" / "build-image.yml"
spec = importlib.util.spec_from_file_location("require_private_oci_owner", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class OwnerVisibilityTests(unittest.TestCase):
    def test_workflow_checks_before_firmware_import_and_candidate_promotion(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        import_step = workflow.index("- name: Import independently pinned Lenovo firmware data")
        candidate_push = workflow.index('podman push --digestfile "$digest_file" "$candidate"')
        promote_step = workflow.index("  promote-lenovo:")
        immutable_copy = workflow.index('skopeo copy --preserve-digests "docker://$image" "docker://$target"')
        guard = "python3 os/dev/require-private-oci-owner.py"
        self.assertIn(guard, workflow[import_step:candidate_push])
        self.assertIn(guard, workflow[workflow.index("if [[ '${{ steps.release-mode.outputs.publish }}' == true ]]; then", import_step):candidate_push])
        self.assertIn(guard, workflow[promote_step:immutable_copy])

    def test_only_exact_private_tilo_owner_passes(self):
        self.assertTrue(module.owner_is_private({"username": "tilo", "visibility": "private"}))
        for owner in (
            {"username": "tilo", "visibility": "public"},
            {"username": "tilo", "visibility": "limited"},
            {"username": "other", "visibility": "private"},
            {"username": "tilo"},
            [],
        ):
            with self.subTest(owner=owner):
                self.assertFalse(module.owner_is_private(owner))

    def test_missing_token_fails_without_request(self):
        with patch.dict(module.os.environ, {}, clear=True), patch.object(module, "HTTPSConnection") as connection:
            with redirect_stderr(io.StringIO()):
                self.assertEqual(module.main(), 1)
            connection.assert_not_called()

    def test_public_owner_and_redirect_fail_closed(self):
        for status, owner in (
            (200, {"username": "tilo", "visibility": "public"}),
            (302, {"username": "tilo", "visibility": "private"}),
        ):
            with self.subTest(status=status):
                response = type("Response", (), {
                    "status": status,
                    "read": lambda self, count: json.dumps(owner).encode(),
                })()
                with patch.dict(module.os.environ, {"REGISTRY_TOKEN": "test-token"}), \
                     patch.object(module, "HTTPSConnection") as connection, \
                     redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
                    connection.return_value.getresponse.return_value = response
                    self.assertEqual(module.main(), 1)
                    connection.return_value.request.assert_called_once_with(
                        "GET", module.OWNER_PATH,
                        headers={"Authorization": "token test-token"},
                    )

    def test_private_owner_passes(self):
        response = type("Response", (), {
            "status": 200,
            "read": lambda self, count: b'{"username":"tilo","visibility":"private"}',
        })()
        with patch.dict(module.os.environ, {"REGISTRY_TOKEN": "test-token"}), \
             patch.object(module, "HTTPSConnection") as connection, \
             redirect_stdout(io.StringIO()):
            connection.return_value.getresponse.return_value = response
            self.assertEqual(module.main(), 0)

    def test_explicit_public_owner_override_retries_without_registry_token(self):
        public = {"username": "tilo", "visibility": "public"}
        self.assertTrue(module.owner_is_approved(public, True))
        self.assertFalse(module.owner_is_approved(public, False))
        self.assertFalse(module.owner_is_approved(
            {"username": "other", "visibility": "public"}, True))
        with patch.dict(module.os.environ, {
                "REGISTRY_TOKEN": "test-token",
                "TILO_ALLOW_PUBLIC_LENOVO_FIRMWARE": "true"}), \
             patch.object(module, "fetch_owner", side_effect=[None, public]) as fetch, \
             redirect_stdout(io.StringIO()):
            self.assertEqual(module.main(), 0)
        self.assertEqual(fetch.call_args_list, [
            call({"Authorization": "token test-token"}), call({})])


if __name__ == "__main__":
    unittest.main()
