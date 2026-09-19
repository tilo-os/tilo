#!/usr/bin/env python3
"""Resolve the Forgejo builder tag to its immutable package manifest digest."""
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

IMAGE = "code.sandbox.example/qos/tilo-builder"
FILES_URL = "https://code.sandbox.example/api/v1/packages/qos/container/tilo-builder/latest/files"
MAX_RESPONSE = 1024 * 1024
ATTEMPTS = 3
TRANSIENT_ERRNOS = {errno.ECONNRESET, errno.ECONNREFUSED, errno.ECONNABORTED,
                    errno.ETIMEDOUT, errno.ENETUNREACH, errno.EHOSTUNREACH, errno.EPIPE}


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
        name = errno.errorcode.get(code) if type(code) is int else None
        return ("OS_" + name if name else "OSError"), code in TRANSIENT_ERRNOS if type(code) is int else False
    if isinstance(error, ValueError):
        return "ValueError", False
    return "UnexpectedError", False


def fetch_files(token):
    if not token:
        raise ValueError("REGISTRY_TOKEN is required to resolve the current builder")
    request = Request(FILES_URL, headers={"Authorization": "token " + token,
                                         "Accept": "application/json"})
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
        print("::error::CI builder resolution failed (" + failure(error)[0] + ")", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
