#!/usr/bin/env python3
"""Resolve the Forgejo builder tag to its immutable package manifest digest."""
import json
import os
from pathlib import Path
import re
import sys
from urllib.request import HTTPRedirectHandler, Request, build_opener

IMAGE = "code.sandbox.example/qos/tilo-builder"
FILES_URL = "https://code.sandbox.example/api/v1/packages/qos/container/tilo-builder/latest/files"


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("builder metadata redirects are not accepted")


def fetch_files(token):
    if not token:
        raise ValueError("REGISTRY_TOKEN is required to resolve the current builder")
    request = Request(FILES_URL, headers={"Authorization": "token " + token,
                                         "Accept": "application/json"})
    with build_opener(NoRedirect()).open(request, timeout=30) as response:
        payload = response.read(1024 * 1024 + 1)
    if len(payload) > 1024 * 1024:
        raise ValueError("builder metadata exceeds response bound")
    return json.loads(payload)


def resolve(requested="", token="", fetch=fetch_files):
    if requested:
        if not re.fullmatch(re.escape(IMAGE) + r"@sha256:[0-9a-f]{64}", requested):
            raise ValueError("builder_image must be an immutable tilo-builder SHA-256 reference")
        return requested
    rows = fetch(token)
    # This Forgejo container-package endpoint returns the manifest/index as
    # manifest.json; its SHA-256 is the registry's content-addressed reference.
    # Reject ambiguous or changed API contracts instead of silently using latest.
    if not isinstance(rows, list) or len(rows) != 1 or not isinstance(rows[0], dict):
        raise ValueError("expected exactly one builder manifest file")
    row = rows[0]
    if row.get("name") != "manifest.json" or not re.fullmatch(r"[0-9a-f]{64}", str(row.get("sha256", ""))):
        raise ValueError("builder manifest name or SHA-256 is invalid")
    if type(row.get("Size")) is not int or row["Size"] <= 0:
        raise ValueError("builder manifest size is invalid")
    return IMAGE + "@sha256:" + row["sha256"]


def main():
    try:
        image = resolve(os.environ.get("REQUESTED_BUILDER", ""), os.environ.get("REGISTRY_TOKEN", ""))
        output = os.environ["GITHUB_OUTPUT"]
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write("image=" + image + "\n")
        print("Resolved CI builder: " + image)
    except Exception as error:
        # HTTP errors must never echo credentials or authentication responses.
        print("::error::CI builder resolution failed (" + type(error).__name__ + ")", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
