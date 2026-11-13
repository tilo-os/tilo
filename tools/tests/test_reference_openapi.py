"""tools/docs/reference/openapi.py writes a REST reference from a description."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "tools/docs/reference/openapi.py"

SPEC = {
    "openapi": "3.1.0",
    "info": {"title": "Tilo API", "summary": "Workspaces and their content, over HTTP."},
    "servers": [{"url": "https://tilo.sh"}],
    "tags": [
        {"name": "Workspaces", "description": "A person's workspaces."},
        {"name": "Files", "description": "Bytes in and out of a workspace."},
    ],
    "paths": {
        "/v1/workspaces": {
            "get": {
                "tags": ["Workspaces"],
                "summary": "List the workspaces this key reaches.",
                "parameters": [
                    {
                        "name": "limit",
                        "in": "query",
                        "schema": {"type": "integer"},
                        "description": "How many to list.",
                    }
                ],                "responses": {
                    "200": {
                        "description": "The workspaces.",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "workspaces": {
                                            "type": "array",
                                            "items": {"$ref": "#/components/schemas/Workspace"},
                                        }
                                    },
                                }
                            }
                        },
                    }
                },
            },
            "post": {
                "tags": ["Workspaces"],
                "summary": "Create a workspace.",
                "requestBody": {
                    "required": True,
                    "content": {
                        "application/json": {
                            "schema": {"$ref": "#/components/schemas/NewWorkspace"}
                        }
                    },
                },
                "responses": {
                    "201": {
                        "description": "The workspace.",
                        "content": {
                            "application/json": {
                                "schema": {"$ref": "#/components/schemas/Workspace"}
                            }
                        },
                    }
                },
            },
        },
        "/v1/workspaces/{id}/content": {
            "put": {
                "tags": ["Files"],
                "summary": "Upload bytes for an action to use.",
                "parameters": [
                    {
                        "name": "id",
                        "in": "path",
                        "required": True,
                        "description": "The workspace.",
                        "schema": {"type": "string"},
                    },
                    {
                        "name": "name",
                        "in": "query",
                        "required": True,
                        "description": "What to call the file.",
                        "schema": {"type": "string"},
                    },
                ],
                "responses": {
                    "200": {
                        "description": "Its content reference.",
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "content": {
                                            "type": "string",
                                            "description": "The reference actions take.",
                                        }
                                    },
                                }
                            }
                        },
                    }
                },
            }
        },
    },
    "components": {
        "securitySchemes": {"key": {"type": "http", "scheme": "bearer"}},
        "schemas": {
            "Workspace": {
                "type": "object",
                "required": ["id", "name"],
                "properties": {
                    "id": {"type": "string", "description": "Its id."},
                    "name": {"type": "string", "description": "Its name."},
                    "role": {"type": "string", "description": "Your part in it."},
                },
            },
            "NewWorkspace": {
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": {"type": "string", "description": "What to call it."},
                    "host": {"type": "string", "description": "Where to make it."},
                },
            },
        }
    },
}


def write_spec(root: Path, spec: dict, name: str = "openapi.json") -> Path:
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(spec), encoding="utf-8")
    return path


def run(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-I", str(GENERATOR), *arguments],
        capture_output=True,
        text=True,
        check=False,
    )


class OpenApiReferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.out = self.root / "generated" / "rest.md"

    def tearDown(self) -> None:
        self.directory.cleanup()

    def generate(self, spec: dict | None = None, *arguments: str) -> subprocess.CompletedProcess[str]:
        write_spec(self.root, SPEC if spec is None else spec)
        return run("--root", str(self.root), "--out", str(self.out), *arguments)

    def page(self) -> str:
        return self.out.read_text(encoding="utf-8")

    def test_writes_the_page_from_the_description(self) -> None:
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        page = self.page()
        self.assertTrue(page.startswith("# Tilo API\n"))
        self.assertIn("Workspaces and their content, over HTTP.", page)
        self.assertEqual(page.count("\n# "), 0, "one H1 only")
        self.assertLess(page.index("## Workspaces"), page.index("## Files"))
        self.assertIn("Every call carries a key as `Authorization: Bearer $TILO_API_KEY`.", page)
        self.assertIn("## Workspaces\n\nA person's workspaces.", page)
        self.assertIn("### GET /v1/workspaces", page)
        self.assertIn("### POST /v1/workspaces", page)
        self.assertIn("### PUT /v1/workspaces/{id}/content", page)

    def test_shows_parameters_bodies_and_answers(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        page = self.page()
        self.assertIn("| `limit` | query | no | How many to list. (integer) |", page)
        self.assertIn("| `id` | path | yes | The workspace. |", page)
        self.assertIn("| `name` | string | yes | What to call it. |", page)
        self.assertIn("| `host` | string | no | Where to make it. |", page)
        self.assertIn("| 201 | The workspace. | id, name, role |", page)

    def test_every_call_has_a_command_that_runs_it(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        page = self.page()
        self.assertIn('curl -X GET "https://tilo.sh/v1/workspaces" \\', page)
        self.assertIn('curl -X PUT "https://tilo.sh/v1/workspaces/{id}/content?name=text" \\', page)
        self.assertIn('-H "Authorization: Bearer $TILO_API_KEY"', page)
        self.assertIn("""-d '{"name": "text"}'""", page)

    def test_two_runs_write_the_same_page(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        first = self.page()
        self.assertEqual(self.generate().returncode, 0)
        self.assertEqual(first, self.page())

    def test_an_undocumented_operation_is_an_error(self) -> None:
        spec = json.loads(json.dumps(SPEC))
        del spec["paths"]["/v1/workspaces"]["get"]["summary"]
        result = self.generate(spec)
        self.assertEqual(result.returncode, 1)
        self.assertIn("GET /v1/workspaces says nothing", result.stderr)

    def test_an_undocumented_parameter_is_an_error(self) -> None:
        spec = json.loads(json.dumps(SPEC))
        del spec["paths"]["/v1/workspaces"]["get"]["parameters"][0]["description"]
        result = self.generate(spec)
        self.assertEqual(result.returncode, 1)
        self.assertIn("parameter 'limit' has no description", result.stderr)

    def test_a_tree_with_no_description_asks_for_one(self) -> None:
        result = run("--root", str(self.root), "--out", str(self.out))
        self.assertEqual(result.returncode, 1)
        self.assertIn("no openapi*.json", result.stderr)

    def test_two_descriptions_ask_which_one(self) -> None:
        write_spec(self.root, SPEC, "api/openapi.json")
        write_spec(self.root, SPEC, "other/openapi-extra.json")
        result = run("--root", str(self.root), "--out", str(self.out))
        self.assertEqual(result.returncode, 1)
        self.assertIn("more than one OpenAPI description", result.stderr)
        chosen = run(
            "--root", str(self.root), "--out", str(self.out),
            "--spec", str(self.root / "api/openapi.json"),
        )
        self.assertEqual(chosen.returncode, 0, chosen.stderr)


if __name__ == "__main__":
    unittest.main()
