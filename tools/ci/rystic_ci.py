#!/usr/bin/env python3
"""Build in the native builder, then run staged tests in the same VM (never Cargo here).

No authentication is performed: the caller supplies a private RYSTIC_HOME containing
its token. All subprocess output is captured and never included in reports.
"""

import argparse
from contextlib import contextmanager
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import urllib.parse
import urllib.request
import uuid

CLI_VERSION = "0.0.28"
CLI_SHA256_AMD64 = "ed01116c50142ac43394c81faae61e851d2ca582779891df21b7e73ec2d05f9c"
ROOT = Path(__file__).resolve().parents[2]
TARGETS = {
    "gdrive-rystic": (
        "cloud-source-gdrive",
        "rystic-contract-tests",
        (
            "imports_and_mutations_through_the_production_adapter",
            "initial_sync_obtains_a_change_cursor",
            "changes_after_cursor_paginate_and_resume_without_loss",
            "resumable_upload_commits_exact_bytes",
        ),
        ("native_document_exports_a_genuine_docx_package",),
    ),
    "gdrive-rystic-volume": (
        "tilo-fs",
        "rystic-tests",
        (
            "production_service_and_files_engine_independent_import",
            "production_volume_bothway_bytes_metadata_and_restart",
        ),
        (),
    ),
    "rystic": (
        "cloud-source-dropbox",
        "rystic-contract-tests",
        (
            "importing_writing_and_resuming_use_the_production_adapter",
            "mounted_folder_rejects_foreign_ids",
            "sync_delivers_new_items_after_initial_enumeration",
        ),
        ("id_addressed_replacement_preserves_identity_and_checks_revision",),
    ),
}
PROVIDERS = {
    "gdrive": {
        "product": "google-drive-twin",
        "version": "0.0.5",
        "model_hash": "sha256:2521a3325edc5162aa9da197dc7df19cec9f62049014fbbc784bcba6b830423a",
        "variable": "TILO_RYSTIC_GDRIVE_URL",
        "targets": ("gdrive-rystic", "gdrive-rystic-volume"),
    },
    "dropbox": {
        "product": "dropbox-twin",
        "version": "0.0.4",
        "model_hash": "sha256:9919d76d75cab1fd5e1984ce1d6f92a43fad583d73f5961c3a88df2a9d45d8db",
        "variable": "TILO_RYSTIC_DROPBOX_URL",
        "targets": ("rystic",),
    },
}


class Failure(Exception):
    """Only fixed, non-secret error codes may cross the reporting boundary."""


class Interrupted(BaseException):
    """Cancellation must bypass ordinary provider failures and stop scheduling."""

    def __init__(self, signum):
        self.signum = signum


@contextmanager
def interruption_handlers():
    def interrupt(signum, _frame):
        # A second cancellation must not interrupt owned-instance cleanup.
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, signal.SIG_IGN)
        raise Interrupted(signum)

    previous = {
        sig: signal.signal(sig, interrupt) for sig in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


@contextmanager
def deferred_interruptions():
    """Finish bounded cleanup even if the first cancellation arrives during it."""
    pending = []

    def defer(signum, _frame):
        if not pending:
            pending.append(signum)

    previous = {
        sig: signal.signal(sig, defer) for sig in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if pending and sys.exc_info()[0] is None:
            for sig in (signal.SIGINT, signal.SIGTERM):
                signal.signal(sig, signal.SIG_IGN)
            raise Interrupted(pending[0])


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def command(argv, *, timeout=30, env=None, cwd=None):
    """Kill and reap the subprocess group on timeout or interruption."""
    with subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        cwd=cwd,
        start_new_session=True,
    ) as child:
        try:
            stdout, _ = child.communicate(timeout=timeout)
        except BaseException:
            with deferred_interruptions():
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass  # The child may have exited just before cancellation.
                child.communicate()
            raise
    return subprocess.CompletedProcess(
        argv, child.returncode, stdout.decode("utf-8", "replace")
    )


def checked(argv, **kwargs):
    result = command(argv, **kwargs)
    if result.returncode:
        raise Failure("command_failed")
    return result.stdout


def harness_metadata():
    """Compatibility metadata only; T owns production-source dependency tracking."""
    return {
        "targets": {
            name: {
                "package": package,
                "feature": feature,
                "supported": list(supported),
                "diagnostic": list(diagnostic),
            }
            for name, (package, feature, supported, diagnostic) in TARGETS.items()
        },
        "providers": {
            name: {**spec, "targets": list(spec["targets"])}
            for name, spec in PROVIDERS.items()
        },
    }


def build(artifacts):
    """Must be called *inside* the native builder, with real Cargo, not its shim."""
    artifacts = Path(artifacts).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    # Refuse stale success evidence if any compilation fails.
    (artifacts / "manifest.json").unlink(missing_ok=True)
    entries = {}
    for target, (package, feature, _, _) in TARGETS.items():
        output = checked(
            [
                "cargo",
                "test",
                "--locked",
                "-p",
                package,
                "--features",
                feature,
                "--test",
                target,
                "--no-run",
                "--message-format=json",
            ],
            timeout=1800,
            cwd=ROOT,
        )
        executables = set()
        for line in output.splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if (
                item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") == target
                and item.get("profile", {}).get("test") is True
                and item.get("executable")
            ):
                executables.add(item["executable"])
        if len(executables) != 1:
            raise Failure("compiler_artifact_count")
        executable = Path(executables.pop())
        if not executable.is_absolute():
            executable = ROOT / executable
        destination = artifacts / target
        shutil.copy2(executable, destination)
        entries[target] = {
            "sha256": digest(destination),
            "compiler_path": str(executable),
        }
    # The native DOCX contract embeds the source validator's absolute path.
    validator = ROOT / "crates/system/cloud-source-gdrive/tests/validate_docx.py"
    manifest = {
        "schema": 3,
        "harness_sha256": digest(__file__),
        "metadata": harness_metadata(),
        "binaries": entries,
        "docx_validator": {"path": str(validator), "sha256": digest(validator)},
    }
    if os.environ.get("T_COMMIT"):
        manifest["build_commit"] = os.environ["T_COMMIT"]
    (artifacts / "manifest.json").write_text(
        json.dumps(manifest, sort_keys=True) + "\n"
    )
    return {"status": "built", "binaries": entries}


def validate_download_url(url):
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "https"
        or parsed.hostname
        not in {
            "www.rystic.ai",
            "github.com",
            "release-assets.githubusercontent.com",
        }
        or parsed.username
        or parsed.password
        or parsed.port not in (None, 443)
    ):
        raise Failure("untrusted_download_url")


def fetch(url):
    validate_download_url(url)

    # Official GitHub assets redirect to a separate download host. Validate every hop;
    # install_cli still requires the pinned archive checksum after downloading.
    class TrustedRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            validate_download_url(newurl)
            return super().redirect_request(req, fp, code, msg, headers, newurl)

    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), TrustedRedirect()
    )
    with opener.open(url, timeout=30) as response:
        data = response.read(100 * 1024 * 1024 + 1)
    if len(data) > 100 * 1024 * 1024:
        raise Failure("download_too_large")
    return data


def install_cli(artifacts):
    """Official pinned Linux/amd64 archive; no installer execution or user CLI."""
    meta = json.loads(
        fetch(
            "https://www.rystic.ai/api/cli-release?os=linux&arch=amd64&version="
            + CLI_VERSION
        )
    )
    if (
        meta.get("version") != CLI_VERSION
        or meta.get("product") != "rystic"
        or meta.get("sha256") != CLI_SHA256_AMD64
        or meta.get("asset") != "rystic_0.0.28_linux_amd64.tar.gz"
    ):
        raise Failure("cli_release_mismatch")
    archive = fetch(meta["url"])
    if hashlib.sha256(archive).hexdigest() != CLI_SHA256_AMD64:
        raise Failure("cli_checksum_mismatch")
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        members = [
            m for m in tar.getmembers() if Path(m.name).name == "rystic" and m.isfile()
        ]
        if len(members) != 1:
            raise Failure("cli_archive_layout")
        payload = tar.extractfile(members[0]).read()
    artifacts = Path(artifacts)
    artifacts.mkdir(parents=True, exist_ok=True)
    executable = artifacts / "rystic-cli"
    executable.write_bytes(payload)
    executable.chmod(0o755)
    (artifacts / "cli.json").write_text(
        json.dumps(
            {
                "version": CLI_VERSION,
                "archive_sha256": CLI_SHA256_AMD64,
                "binary_sha256": digest(executable),
            }
        )
        + "\n"
    )
    return {"status": "installed", "cli_version": CLI_VERSION}


def cli_environment():
    home = Path(os.environ.get("RYSTIC_HOME", ""))
    if (
        not home.is_absolute()
        or home.is_symlink()
        or not home.is_dir()
        or home.stat().st_mode & 0o077
    ):
        raise Failure("private_rystic_home_required")
    token = home / "token"
    if (
        not token.is_file()
        or token.is_symlink()
        or token.stat().st_mode & 0o077
        or token.stat().st_uid != os.getuid()
        or home.stat().st_uid != os.getuid()
    ):
        raise Failure("private_token_required")
    # Never inherit developer CLI overrides, vendor credentials, or proxy settings.
    env = {
        k: os.environ[k]
        for k in ("PATH", "HOME", "TMPDIR", "LD_LIBRARY_PATH")
        if k in os.environ
    }
    env.update(RYSTIC_HOME=str(home), RYSTIC_RUNNER="native", RYSTIC_NETWORK="off")
    return env


def listed_tests(output):
    names = [line[:-6] for line in output.splitlines() if line.endswith(": test")]
    if len(names) != len(set(names)):
        raise Failure("duplicate_tests")
    return set(names)


def preflight(artifacts, providers, strict):
    artifacts = Path(artifacts).resolve()
    manifest = json.loads((artifacts / "manifest.json").read_text())
    if (
        manifest.get("schema") != 3
        or manifest.get("harness_sha256") != digest(__file__)
        or manifest.get("metadata") != harness_metadata()
    ):
        raise Failure("harness_mismatch")
    cli_meta = json.loads((artifacts / "cli.json").read_text())
    cli = artifacts / "rystic-cli"
    if (
        cli_meta.get("version") != CLI_VERSION
        or cli_meta.get("archive_sha256") != CLI_SHA256_AMD64
        or digest(cli) != cli_meta.get("binary_sha256")
    ):
        raise Failure("cli_evidence_mismatch")
    env = cli_environment()
    version = checked([str(cli), "version"], env=env)
    if not version.startswith("rystic " + CLI_VERSION + " "):
        raise Failure("cli_version_mismatch")
    plans = {}
    # Validate every binary, even for single-provider invocations: no paid start
    # is possible with only a partially built artifact set.
    for target, (_, _, supported, diagnostic) in TARGETS.items():
        binary = artifacts / target
        if digest(binary) != manifest["binaries"][target]["sha256"]:
            raise Failure("binary_checksum_mismatch")
        output = checked([str(binary), "--list", "--format", "terse"], env=env)
        if listed_tests(output) != set(supported + diagnostic):
            raise Failure("test_inventory_mismatch")
        plans[target] = supported + diagnostic if strict else supported
    if strict:
        validator = manifest["docx_validator"]
        if digest(validator["path"]) != validator["sha256"]:
            raise Failure("docx_validator_mismatch")
    # Pull explicitly pinned releases; CLI verifies release checksums before unpacking.
    # This stage is free and completes for *all* providers before the first lease.
    for provider in providers:
        spec = PROVIDERS[provider]
        checked(
            [str(cli), "pull", spec["product"], "--version", spec["version"]],
            env=env,
            timeout=120,
        )
    return artifacts, cli, env, plans


def version_handshake(url):
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "http"
        or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}
        or parsed.username
        or parsed.password
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
        or not parsed.port
    ):
        raise Failure("non_loopback_url")

    # Do not follow a loopback redirect to an outside host.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(url.rstrip("/") + "/_rystic/version", timeout=10) as response:
        return json.loads(response.read(65536))


def run_provider(provider, artifacts, cli, env, plans):
    spec = PROVIDERS[provider]
    name = "tilo-ci-" + uuid.uuid4().hex
    report = {
        "provider": provider,
        "providerVersion": spec["version"],
        "model_hash": None,
        "status": "failed",
        "tests": [],
        "cleanup": "pending",
    }
    deadline = time.monotonic() + 240
    try:
        output = checked(
            [
                str(cli),
                "run",
                spec["product"],
                "--version",
                spec["version"],
                "--runner",
                "native",
                "--network",
                "off",
                "--name",
                name,
                "--port",
                "0",
                "-d",
                "-json",
            ],
            env=env,
            timeout=60,
        )
        url = json.loads(output)["data"]["url"]
        version = version_handshake(url)
        if any(
            version.get(key) != spec[key]
            for key in ("product", "version", "model_hash")
        ):
            raise Failure("provider_identity_mismatch")
        report["model_hash"] = version["model_hash"]
        test_env = {k: v for k, v in env.items() if k != "RYSTIC_HOME"}
        test_env[spec["variable"]] = url
        for target in spec["targets"]:
            for test in plans[target]:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Failure("check_timeout")
                result = command(
                    [str(artifacts / target), test, "--exact", "--test-threads=1"],
                    env=test_env,
                    timeout=remaining,
                )
                # A successful process that silently runs zero/ignored tests is not proof.
                proof = bool(
                    re.search(
                        r"test result: ok\. 1 passed; 0 failed; 0 ignored;",
                        result.stdout,
                    )
                )
                report["tests"].append(
                    {
                        "binary": target,
                        "test": test,
                        "testexit": result.returncode,
                        "status": "passed"
                        if result.returncode == 0 and proof
                        else "failed",
                    }
                )
        report["status"] = (
            "passed"
            if all(t["status"] == "passed" for t in report["tests"])
            else "failed"
        )
    except subprocess.TimeoutExpired:
        report["error"] = "timeout"
    except Failure as error:
        report["error"] = str(error)
    except Exception:
        report["error"] = "runtime_error"
    finally:
        with deferred_interruptions():
            cleanup_provider(cli, name, env, report)
    return report


def cleanup_provider(cli, name, env, report):
    # Even a failed/timed-out start can have created this UUID. Never use ps/down.
    clean = True
    cleanup_deadline = time.monotonic() + 30
    for action in ("stop", "rm"):
        try:
            result = command(
                [str(cli), action, name],
                env=env,
                timeout=max(
                    0.01,
                    (cleanup_deadline - time.monotonic())
                    / (2 if action == "stop" else 1),
                ),
            )
            clean &= result.returncode == 0
        except Exception:
            clean = False
    report["cleanup"] = "passed" if clean else "failed"
    if not clean:
        report["status"] = "failed"


def run(artifacts, providers, strict):
    try:
        artifacts, cli, env, plans = preflight(artifacts, providers, strict)
    except Failure as error:
        return {
            "status": "failed",
            "error": "preflight_failed",
            "reason": str(error),
            "providers": [],
        }
    except Exception:
        return {"status": "failed", "error": "preflight_failed", "providers": []}
    reports = []
    for provider in providers:
        report = run_provider(provider, artifacts, cli, env, plans)
        reports.append(report)
        if report["cleanup"] != "passed":
            break  # Do not create another lease while ownership cleanup is uncertain.
    return {
        "status": "passed"
        if all(r["status"] == "passed" for r in reports)
        else "failed",
        "mode": "strict" if strict else "supported",
        "providers": reports,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("build", "install-cli", "run"))
    parser.add_argument("--artifacts", type=Path, default=Path("/target/rystic-ci"))
    parser.add_argument("--provider", choices=("all", *PROVIDERS), default="all")
    parser.add_argument(
        "--strict",
        action="store_true",
        help="include unsupported contracts; assertions remain fatal",
    )
    args = parser.parse_args()
    exit_code = 0
    with interruption_handlers():
        try:
            if args.operation == "build":
                report = build(args.artifacts)
            elif args.operation == "install-cli":
                report = install_cli(args.artifacts)
            else:
                providers = (
                    list(PROVIDERS) if args.provider == "all" else [args.provider]
                )
                report = run(args.artifacts, providers, args.strict)
        except Interrupted as error:
            report = {"status": "interrupted"}
            exit_code = 128 + error.signum
        except Exception:
            report = {"status": "failed", "error": "operation_failed"}
        print(json.dumps(report, sort_keys=True))
    return exit_code or int(report["status"] == "failed")


if __name__ == "__main__":
    raise SystemExit(main())
