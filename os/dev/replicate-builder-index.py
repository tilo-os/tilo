#!/usr/bin/env python3
"""Copy a promoted immutable image into its regional ECR repository.

The source and destination must expose identical manifest bytes. Forgejo OIDC
is exchanged for a short-lived, repository-scoped AWS role; neither static AWS
credentials nor the ECR password are written outside a private temporary
directory.
"""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.request


SOURCE_REPOSITORY = "code.podesta.ai/qos/tilo-builder"
REGIONAL_REPOSITORIES = {
    "code.podesta.ai/public/runner-image": "public/runner-image",
    "code.podesta.ai/qos/ci-fast": "qos/ci-fast",
    "code.podesta.ai/qos/tilo-builder": "qos/tilo-builder",
    "code.podesta.ai/qos/tilo-release-tools": "qos/tilo-release-tools",
    "code.podesta.ai/public/tilo-release-tools": "qos/tilo-release-tools",
    "code.podesta.ai/qos/tilo-registry-server": "qos/tilo-registry-server",
    "docker.io/library/python": "dockerhub/library/python",
}
OIDC_ISSUER = "https://code.podesta.ai/api/actions"
OIDC_SUBJECT = "repo:tilo-11/tilo-23:ref:refs/heads/main"
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def command(arguments, *, environment=None, input_text=None):
    return subprocess.run(
        arguments,
        check=True,
        input=input_text,
        stdout=subprocess.PIPE,
        text=True,
        env=environment,
    ).stdout.strip()


def raw_manifest(reference, *, authfile=None):
    arguments = ["skopeo", "inspect", "--raw"]
    if authfile is not None:
        arguments.extend(("--authfile", str(authfile)))
    arguments.append("docker://" + reference)
    return subprocess.run(arguments, check=True, stdout=subprocess.PIPE).stdout


def manifest_digest(document):
    return "sha256:" + hashlib.sha256(document).hexdigest()


def valid_descriptor(descriptor):
    return (
        isinstance(descriptor, dict)
        and isinstance(descriptor.get("mediaType"), str)
        and type(descriptor.get("size")) is int
        and descriptor["size"] >= 0
        and isinstance(descriptor.get("digest"), str)
        and DIGEST.fullmatch(descriptor.get("digest", "")) is not None
    )


def validate_manifest(document, source_repository):
    if not isinstance(document, dict) or document.get("schemaVersion") != 2:
        raise ValueError("promoted image is not a schema-2 manifest")
    media_type = document.get("mediaType")
    index_types = {
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
    }
    image_types = {
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    }
    has_index_shape = "manifests" in document
    has_image_shape = "config" in document or "layers" in document
    if has_index_shape and has_image_shape:
        raise ValueError("promoted image has an ambiguous manifest shape")
    if has_index_shape:
        if media_type not in index_types:
            raise ValueError("promoted platform index has an unsupported media type")
        manifests = document.get("manifests")
        if not isinstance(manifests, list) or not manifests or not all(valid_descriptor(item) for item in manifests):
            raise ValueError("promoted platform index has invalid child descriptors")
        if source_repository == SOURCE_REPOSITORY and len(manifests) != 2:
            raise ValueError("promoted builder is not the accepted two-platform index")
        return
    if has_image_shape:
        if media_type is not None and media_type not in image_types:
            raise ValueError("promoted image manifest has an unsupported media type")
        config = document.get("config")
        layers = document.get("layers")
        if not valid_descriptor(config) or not isinstance(layers, list) or not all(
            valid_descriptor(item) for item in layers
        ):
            raise ValueError("promoted image manifest has invalid config or layers")
        if source_repository == SOURCE_REPOSITORY:
            raise ValueError("promoted builder is not the accepted two-platform index")
        return
    raise ValueError("promoted image has neither index nor image-manifest content")


def oidc_token(audience):
    url = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_URL", "")
    request_token = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "")
    if not url or not request_token:
        raise ValueError("Forgejo OIDC request environment is required")
    separator = "&" if "?" in url else "?"
    request = urllib.request.Request(
        url + separator + "audience=" + audience,
        headers={"Authorization": "bearer " + request_token},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        token = json.loads(response.read())["value"]
    if not isinstance(token, str) or len(token.split(".")) != 3:
        raise ValueError("Forgejo returned a malformed OIDC token")
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    claims = json.loads(base64.urlsafe_b64decode(payload))
    audience_claim = claims.get("aud")
    audiences = audience_claim if isinstance(audience_claim, list) else [audience_claim]
    if claims.get("iss") != OIDC_ISSUER or claims.get("sub") != OIDC_SUBJECT:
        raise ValueError("Forgejo OIDC token has an unexpected issuer or subject")
    if audience not in audiences:
        raise ValueError("Forgejo OIDC token has an unexpected audience")
    return token, claims["sub"]


def regional_identity(role, repository, region):
    role_match = re.fullmatch(r"arn:aws:iam::([0-9]{12}):role/[A-Za-z0-9+=,.@_/-]+", role)
    repository_match = re.fullmatch(
        r"([0-9]{12})\.dkr\.ecr\.([a-z]{2}(?:-gov)?-[a-z]+-[0-9])\.amazonaws\.com/([a-z0-9][a-z0-9._/-]*)",
        repository,
    )
    if not role_match or not repository_match:
        raise ValueError("regional role or repository is invalid")
    if role_match.group(1) != repository_match.group(1) or repository_match.group(2) != region:
        raise ValueError("regional role, repository, and region do not agree")
    return role_match.group(1), repository.split("/", 1)[0]


def replicate(
    source_file,
    target_repository,
    role,
    region,
    audience,
    public_key,
    output,
    source_authfile=None,
):
    source = source_file.read_text().strip()
    source_match = re.fullmatch(r"([^\s@]+)@(sha256:[0-9a-f]{64})", source)
    if not source_match:
        raise ValueError("source is not an immutable promoted image")
    source_repository, digest = source_match.groups()
    expected_target = REGIONAL_REPOSITORIES.get(source_repository)
    if expected_target is None or target_repository.split("/", 1)[-1] != expected_target:
        raise ValueError("source has no configured regional repository")
    account, registry = regional_identity(role, target_repository, region)

    source_raw = raw_manifest(source, authfile=source_authfile)
    if manifest_digest(source_raw) != digest:
        raise ValueError("source registry returned different manifest bytes")
    validate_manifest(json.loads(source_raw), source_repository)
    with tempfile.TemporaryDirectory(prefix="regional-image-ecr-") as temporary:
        temporary_path = Path(temporary)
        source_environment = None
        if source_authfile is not None:
            source_auth_directory = temporary_path / "source-auth"
            source_auth_directory.mkdir(mode=0o700)
            source_auth_snapshot = source_auth_directory / "config.json"
            source_auth_snapshot.write_bytes(source_authfile.read_bytes())
            source_auth_snapshot.chmod(0o600)
            source_environment = os.environ.copy()
            source_environment["DOCKER_CONFIG"] = str(source_auth_directory)
        if public_key is not None:
            command(
                ("cosign", "verify", "--key", str(public_key), source),
                environment=source_environment,
            )

        token, subject = oidc_token(audience)
        token_file = temporary_path / "oidc-token"
        auth_file = temporary_path / "auth.json"
        token_file.write_text(token)
        token_file.chmod(0o600)

        aws_environment = os.environ.copy()
        for name in (
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_SECURITY_TOKEN",
            "AWS_PROFILE",
            "AWS_DEFAULT_PROFILE",
            "AWS_SHARED_CREDENTIALS_FILE",
        ):
            aws_environment.pop(name, None)
        aws_environment.update(
            {
                "AWS_ROLE_ARN": role,
                "AWS_ROLE_SESSION_NAME": "regional-image-replication-"
                + os.environ.get("GITHUB_RUN_ID", "local")[:32],
                "AWS_WEB_IDENTITY_TOKEN_FILE": str(token_file),
                "AWS_REGION": region,
                "AWS_DEFAULT_REGION": region,
                "AWS_STS_REGIONAL_ENDPOINTS": "regional",
            }
        )
        identity = json.loads(
            command(("aws", "sts", "get-caller-identity", "--output", "json"), environment=aws_environment)
        )
        if identity.get("Account") != account or ":assumed-role/" not in identity.get("Arn", ""):
            raise ValueError("AWS returned an unexpected replication identity")
        password = command(
            ("aws", "ecr", "get-login-password", "--region", region),
            environment=aws_environment,
        )
        command(
            ("skopeo", "login", "--authfile", str(auth_file), "--username", "AWS", "--password-stdin", registry),
            input_text=password,
        )
        target_tag = target_repository + ":sha256-" + digest.removeprefix("sha256:")
        existing = command(
            (
                "aws",
                "ecr",
                "batch-get-image",
                "--region",
                region,
                "--repository-name",
                target_repository.split("/", 1)[1],
                "--image-ids",
                "imageDigest=" + digest,
                "--query",
                "length(images)",
                "--output",
                "text",
            ),
            environment=aws_environment,
        )
        if existing not in {"0", "1"}:
            raise ValueError("ECR returned an invalid image-existence result")
        if existing == "0":
            copy_arguments = [
                "skopeo",
                "copy",
                "--all",
                "--preserve-digests",
                "--dest-authfile",
                str(auth_file),
            ]
            if source_authfile is not None:
                copy_arguments.extend(("--src-authfile", str(source_authfile)))
            copy_arguments.extend(("docker://" + source, "docker://" + target_tag))
            command(tuple(copy_arguments))
        target = target_repository + "@" + digest
        target_raw = raw_manifest(target, authfile=auth_file)
        if target_raw != source_raw or manifest_digest(target_raw) != digest:
            raise ValueError("regional registry changed the promoted OCI identity")

    output.write_text(f"source={source}\ntarget={target}\nsubject={subject}\n")
    return target


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-file", type=Path, required=True)
    parser.add_argument("--target-repository", required=True)
    parser.add_argument("--role-arn", required=True)
    parser.add_argument("--region", required=True)
    parser.add_argument("--audience", default="tilo-builder-replication")
    parser.add_argument("--public-key", type=Path)
    parser.add_argument("--source-authfile", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(
            replicate(
                args.source_file,
                args.target_repository,
                args.role_arn,
                args.region,
                args.audience,
                args.public_key,
                args.output,
                args.source_authfile,
            )
        )
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"regional builder replication failed: {error}\n")


if __name__ == "__main__":
    main()
