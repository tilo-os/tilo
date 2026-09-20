#!/usr/bin/env python3
"""Resolve the GHCR builder tag to its immutable manifest digest."""
import errno
import http.client
import json
import os
from pathlib import Path
import re
import socket
import ssl
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener

IMAGE = "ghcr.io/tilo-os/tilo-builder"
VERSIONS_URL = (
    "https://api.github.com/orgs/tilo-os/packages/container/"
    "tilo-builder/versions?per_page=100"
)
MAX_RESPONSE = 1024 * 1024
ATTEMPTS = 3
# errno.errorcode can prefer Winsock aliases on Windows. Keep supported
# transport categories identical across platforms, without changing errno tests.
TRANSIENT_ERRNOS = {errno.ECONNRESET: "ECONNRESET", errno.ECONNREFUSED: "ECONNREFUSED",
                    errno.ECONNABORTED: "ECONNABORTED", errno.ETIMEDOUT: "ETIMEDOUT",
                    errno.ENETUNREACH: "ENETUNREACH", errno.EHOSTUNREACH: "EHOSTUNREACH",
                    errno.EPIPE: "EPIPE"}


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("builder metadata redirects are not accepted")


def failure(error):
    """Return only a fixed safe category and whether this GET may be retried."""
    if isinstance(error, HTTPError):
        code = error.code
        if type(code) is int and 100 <= code <= 599:
            return f"HTTP_{code}", code == 429 or 500 <= code <= 599
        return "HTTPError", False
    if isinstance(error, URLError):
        # Never stringify reason: it can contain URLs, credentials or headers.
        if isinstance(error.reason, BaseException):
            return failure(error.reason)
        return "URLError", False
    if isinstance(error, ssl.SSLCertVerificationError):
        return "TLS_CERTIFICATE", False
    if isinstance(error, ssl.SSLError):
        return "TLS_ERROR", False
    if isinstance(error, socket.gaierror):
        return ("DNS_TEMPORARY", True) if error.errno == socket.EAI_AGAIN else ("DNS_ERROR", False)
    if isinstance(error, TimeoutError):
        return "TIMEOUT", True
    if isinstance(error, http.client.IncompleteRead):
        return "HTTP_INCOMPLETE", True
    if isinstance(error, OSError):
        code = error.errno
        name = TRANSIENT_ERRNOS.get(code, errno.errorcode.get(code)) if type(code) is int else None
        return ("OS_" + name if name else "OSError"), code in TRANSIENT_ERRNOS if type(code) is int else False
    if isinstance(error, ValueError):
        return "ValueError", False
    return "UnexpectedError", False


def fetch_versions(token):
    if not token:
        raise ValueError("REGISTRY_TOKEN is required to resolve the current builder")
    request = Request(
        VERSIONS_URL,
        headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    opener = build_opener(NoRedirect())
    # Only this fixed-origin metadata GET is retried. Each attempt retains the
    # 30-second socket timeout and 1 MiB response bound; backoff is 1s then 2s.
    for attempt in range(ATTEMPTS):
        try:
            with opener.open(request, timeout=30) as response:
                payload = response.read(MAX_RESPONSE + 1)
            break
        except Exception as error:
            category, transient = failure(error)
            if isinstance(error, HTTPError):
                error.close()
            if not transient or attempt == ATTEMPTS - 1:
                raise
            print(f"Builder metadata retry {attempt + 1}/{ATTEMPTS - 1} ({category})", file=sys.stderr)
            time.sleep(attempt + 1)
    if len(payload) > MAX_RESPONSE:
        raise ValueError("builder metadata exceeds response bound")
    return json.loads(payload)


def resolve(requested="", token="", fetch=fetch_versions):
    if requested:
        if not re.fullmatch(re.escape(IMAGE) + r"@sha256:[0-9a-f]{64}", requested):
            raise ValueError("builder_image must be an immutable tilo-builder SHA-256 reference")
        return requested
    rows = fetch(token)
    if not isinstance(rows, list):
        raise ValueError("GitHub package versions response is invalid")
    latest = []
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("GitHub package version is invalid")
        metadata = row.get("metadata")
        container = metadata.get("container") if isinstance(metadata, dict) else None
        tags = container.get("tags") if isinstance(container, dict) else None
        if isinstance(tags, list) and "latest" in tags:
            latest.append(row)
    if len(latest) != 1:
        raise ValueError("expected exactly one GHCR version tagged latest")
    digest = latest[0].get("name")
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("GHCR package version digest is invalid")
    return IMAGE + "@" + digest


def main():
    try:
        image = resolve(os.environ.get("REQUESTED_BUILDER", ""), os.environ.get("REGISTRY_TOKEN", ""))
        output = os.environ["GITHUB_OUTPUT"]
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write("image=" + image + "\n")
        print("Resolved CI builder: " + image)
    except Exception as error:
        # HTTP errors must never echo credentials or authentication responses.
        print("::error::CI builder resolution failed (" + failure(error)[0] + ")", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
