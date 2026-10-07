#!/usr/bin/env python3
"""Resolve the published builder tag to its immutable index digest."""
import errno
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import socket
import ssl
import sys
import time
import tomllib
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, Request, build_opener


def registry_settings(environ=os.environ):
    """Registry coordinates from tools/ci/images.toml, overridable per run."""
    with Path(__file__).with_name("images.toml").open("rb") as stream:
        registry = tomllib.load(stream)["registry"]
    host = environ.get("TILO_CI_REGISTRY") or registry["host"]
    repository = (environ.get("TILO_CI_BUILDER_REPOSITORY")
                  or registry["namespace"] + "/" + registry["builder"])
    realm = environ.get("TILO_CI_REGISTRY_TOKEN_REALM") or registry["token_realm"]
    if not re.fullmatch(r"[a-z0-9.-]+(:[0-9]+)?", host) or not re.fullmatch(r"[a-z0-9._/-]+", repository):
        raise ValueError("invalid builder registry coordinates")
    if not realm.startswith("https://"):
        raise ValueError("registry token realm must use https")
    return host, repository, realm


REGISTRY, REPOSITORY, TOKEN_REALM = registry_settings()
IMAGE = REGISTRY + "/" + REPOSITORY
MANIFEST_URL = "https://" + REGISTRY + "/v2/" + REPOSITORY + "/manifests/latest"
# The builder is always a multi-architecture index; asking for index types
# only means a registry can never answer with one platform's manifest.
INDEX_TYPES = ("application/vnd.oci.image.index.v1+json",
               "application/vnd.docker.distribution.manifest.list.v2+json")
# A registry may answer every request with a token challenge and grant
# anonymous callers pull; one that needs no token is read directly.
PULL_SCOPE = "repository:" + REPOSITORY + ":pull"
AUTH_CHALLENGE = re.compile(r'Bearer realm="(?P<realm>[^"]+)",service="(?P<service>[^"]+)"'
                            r'(?:,scope="(?P<scope>[^"]+)")?')
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


def bounded(response):
    payload = response.read(MAX_RESPONSE + 1)
    if len(payload) > MAX_RESPONSE:
        raise ValueError("builder metadata exceeds response bound")
    return payload


def anonymous_bearer(opener, challenge):
    """Follow the token challenge anonymously: pulls need no identity."""
    match = AUTH_CHALLENGE.fullmatch(challenge or "")
    if not match or match["realm"] != TOKEN_REALM or match["scope"] not in (None, PULL_SCOPE):
        raise ValueError("unexpected registry authentication challenge")
    query = urlencode({"service": match["service"], "scope": PULL_SCOPE})
    with opener.open(Request(TOKEN_REALM + "?" + query), timeout=30) as response:
        document = json.loads(bounded(response))
    token = (document.get("token") or document.get("access_token")) if isinstance(document, dict) else None
    if not isinstance(token, str) or not token:
        raise ValueError("registry returned no pull token")
    return token


def read_manifest(opener):
    headers = {"Accept": ", ".join(INDEX_TYPES)}
    try:
        with opener.open(Request(MANIFEST_URL, headers=headers), timeout=30) as response:
            return bounded(response)
    except HTTPError as error:
        if error.code != 401:
            raise
        challenge = error.headers.get("WWW-Authenticate") if error.headers else None
        error.close()
    bearer = anonymous_bearer(opener, challenge)
    authorized = Request(MANIFEST_URL, headers={**headers, "Authorization": "Bearer " + bearer})
    with opener.open(authorized, timeout=30) as response:
        return bounded(response)


def fetch_manifest():
    # No credential ever reaches these requests.
    opener = build_opener(NoRedirect())
    # Only this fixed-origin metadata read is retried. Each attempt retains the
    # 30-second socket timeout and 1 MiB response bound; backoff is 1s then 2s.
    for attempt in range(ATTEMPTS):
        try:
            return read_manifest(opener)
        except Exception as error:
            category, transient = failure(error)
            if isinstance(error, HTTPError):
                error.close()
            if not transient or attempt == ATTEMPTS - 1:
                raise
            print(f"Builder metadata retry {attempt + 1}/{ATTEMPTS - 1} ({category})", file=sys.stderr)
            time.sleep(attempt + 1)


def resolve(requested="", fetch=fetch_manifest):
    if requested:
        if not re.fullmatch(re.escape(IMAGE) + r"@sha256:[0-9a-f]{64}", requested):
            raise ValueError("builder_image must be an immutable tilo-builder SHA-256 reference")
        return requested
    payload = fetch()
    # The digest of the exact bytes served is the registry's content address.
    # Reject anything but a non-empty index instead of silently using latest.
    if not isinstance(payload, bytes):
        raise ValueError("builder manifest is not raw bytes")
    document = json.loads(payload)
    if (not isinstance(document, dict) or document.get("mediaType") not in INDEX_TYPES
            or not isinstance(document.get("manifests"), list) or not document["manifests"]):
        raise ValueError("expected the builder's multi-architecture index")
    return IMAGE + "@sha256:" + hashlib.sha256(payload).hexdigest()


def main():
    try:
        image = resolve(os.environ.get("REQUESTED_BUILDER", ""))
        output = os.environ["GITHUB_OUTPUT"]
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write("image=" + image + "\n")
        print("Resolved CI builder: " + image)
    except Exception as error:
        # HTTP errors must never echo URLs or response bodies.
        print("::error::CI builder resolution failed (" + failure(error)[0] + ")", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
