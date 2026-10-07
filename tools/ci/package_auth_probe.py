#!/usr/bin/env python3
"""Read-only CI auth diagnostic; never print credentials or response bodies."""
import json
import os
import urllib.error
import urllib.parse
import urllib.request

ARCHIVE = (
    "https://code.tilo.so/api/packages/tilo/generic/tilo-app-collections/"
    "0.0.0-a33663af30b44d19a673df25b3a2bf59eea6d765-"
    "9749360ed68085fc2c94f79ff3047249854207b4745bb3fc68926f64cabab0b0/"
    "tilo-apps-x86_64.tar.gz"
)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def status(url, credential, method="GET", scheme="token"):
    headers = {"Authorization": f"{scheme} {credential}"} if credential else {}
    if url == ARCHIVE and method == "GET":
        headers["Range"] = "bytes=0-0"
    request = urllib.request.Request(url, headers=headers, method=method)
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=20) as response:
            return response.status
    except urllib.error.HTTPError as error:
        code = error.code
        error.close()
        return code
    except (OSError, urllib.error.URLError):
        return "transport_error"


def main():
    registry = os.environ.get("PACKAGE_TOKEN", "")
    job = os.environ.get("JOB_TOKEN", "")
    oidc_url = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_URL", "")
    oidc_token = os.environ.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "")
    result = {
        "probe": os.environ.get("PROBE_KIND", "unknown"),
        "package_token_present": bool(registry),
        "job_token_present": bool(job),
        "oidc_url_present": bool(oidc_url),
        "oidc_request_token_present": bool(oidc_token),
        "archive_url": ARCHIVE,
    }
    result["anonymous_archive_get_status"] = status(ARCHIVE, "")
    result["anonymous_archive_head_status"] = status(ARCHIVE, "", method="HEAD")
    for name, token in (("package_token", registry), ("job_token", job)):
        if token:
            result[name + "_archive_status"] = status(ARCHIVE, token)
            result[name + "_repository_status"] = status(
                "https://code.tilo.so/api/v1/repos/tilo/tilo", token, method="GET"
            )
    if oidc_url and oidc_token:
        parsed = urllib.parse.urlsplit(oidc_url)
        if parsed.scheme != "https" or parsed.hostname not in {"code.tilo.so", "code.sandbox.example"}:
            result["broker_status"] = "unexpected_origin"
        else:
            audience = os.environ.get("COLLECTION_AUDIENCE", "")
            separator = "&" if parsed.query else "?"
            result["broker_status"] = status(
                oidc_url + separator + urllib.parse.urlencode({"audience": audience}),
                oidc_token, method="GET", scheme="bearer"
            )
    print(json.dumps(result, sort_keys=True))
    return 0 if registry and result.get("package_token_archive_status") in (200, 206) else 1


if __name__ == "__main__":
    raise SystemExit(main())
