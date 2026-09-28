#!/usr/bin/env python3
"""Send an email via Resend using the official Python SDK.

Setup:
    pip install resend
    export RESEND_API_KEY=re_xxxxxxxxx

Usage:
    scripts/send-mail.py --to someone@example.com --subject "Hello" --text "Plain body"
    scripts/send-mail.py --to a@x.com --to b@y.com --subject "Hi" --html "<b>Bold</b>"
    scripts/send-mail.py --to someone@example.com --subject "Report" --text-file body.txt

The sender defaults to Resend's test address; pass --from to use a verified
domain of your own.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

try:
    import resend
except ImportError:
    sys.exit("The 'resend' package is not installed. Run: pip install resend")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Send an email through Resend.")
    parser.add_argument(
        "--from",
        dest="sender",
        default=os.environ.get("RESEND_FROM", "Acme <onboarding@resend.dev>"),
        help="Sender address, e.g. 'Name <you@yourdomain.com>' "
        "(default: $RESEND_FROM or Resend's test sender)",
    )
    parser.add_argument(
        "--to",
        action="append",
        required=True,
        help="Recipient address; repeat the flag for multiple recipients",
    )
    parser.add_argument("--cc", action="append", default=[], help="CC address (repeatable)")
    parser.add_argument("--bcc", action="append", default=[], help="BCC address (repeatable)")
    parser.add_argument("--reply-to", dest="reply_to", help="Reply-To address")
    parser.add_argument("--subject", required=True, help="Email subject line")

    body = parser.add_mutually_exclusive_group(required=True)
    body.add_argument("--text", help="Plain-text body")
    body.add_argument("--html", help="HTML body")
    body.add_argument("--text-file", type=Path, help="Read plain-text body from a file")
    body.add_argument("--html-file", type=Path, help="Read HTML body from a file")

    parser.add_argument(
        "--attach",
        action="append",
        default=[],
        type=Path,
        help="Attach a file (repeatable)",
    )
    return parser.parse_args(argv)


def build_params(args: argparse.Namespace) -> resend.Emails.SendParams:
    params: resend.Emails.SendParams = {
        "from": args.sender,
        "to": args.to,
        "subject": args.subject,
    }

    if args.text is not None:
        params["text"] = args.text
    elif args.html is not None:
        params["html"] = args.html
    elif args.text_file is not None:
        params["text"] = args.text_file.read_text(encoding="utf-8")
    elif args.html_file is not None:
        params["html"] = args.html_file.read_text(encoding="utf-8")

    if args.cc:
        params["cc"] = args.cc
    if args.bcc:
        params["bcc"] = args.bcc
    if args.reply_to:
        params["reply_to"] = args.reply_to

    if args.attach:
        attachments: list[resend.Attachment] = []
        for path in args.attach:
            attachments.append(
                {
                    "filename": path.name,
                    "content": list(path.read_bytes()),
                }
            )
        params["attachments"] = attachments

    return params


def main(argv: list[str]) -> int:
    api_key = os.environ.get("RESEND_API_KEY")
    if not api_key:
        print("RESEND_API_KEY is not set in the environment.", file=sys.stderr)
        return 2

    args = parse_args(argv)
    resend.api_key = api_key

    try:
        params = build_params(args)
        result = resend.Emails.send(params)
    except FileNotFoundError as exc:
        print(f"File not found: {exc.filename}", file=sys.stderr)
        return 2
    except resend.exceptions.ResendError as exc:
        print(f"Resend rejected the request: {exc}", file=sys.stderr)
        return 1

    print(f"Sent. Email id: {result['id']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
