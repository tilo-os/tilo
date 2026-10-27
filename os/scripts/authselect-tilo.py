#!/usr/bin/env python3
"""Put pam_tilo in front of the password check of an authselect profile.

usage: authselect-tilo.py PROFILE_DIR

Edits the system-auth and password-auth templates of a custom authselect
profile (`authselect create-profile`) so every PAM service that includes them
(the greeter, the lock screen, polkit, sudo, su, passwd) asks tilo-authd first.
pam_tilo's answer is final for anyone tilo-authd knows; it steps aside only
when the daemon cannot answer (`authinfo_unavail`) or does not know the person
(`ignore`), and then the profile's own lines run as before.

In the password phase it goes before every other line, so a person whose
secret tilo-authd checks changes it there and the old password's quality rules,
which a TPM-throttled PIN would fail, never see it.

The lines are dash-prefixed: an image without the module skips them instead
of refusing everyone. Running it twice changes nothing. A template without
the lines it anchors on is an error: the image would otherwise ship a stack
that skips the daemon.
"""

import re
import sys
from pathlib import Path

CONTROL = "[success=done authinfo_unavail=ignore ignore=ignore default=die]"
AUTH = f"-auth       {CONTROL} pam_tilo.so\n"
PASSWORD = f"-password   {CONTROL} pam_tilo.so\n"
TEMPLATES = ("system-auth", "password-auth")


def edit(text: str, name: str) -> str:
    if "pam_tilo.so" in text:
        return text
    lines = text.splitlines(keepends=True)
    unix_auth = next((i for i, line in enumerate(lines)
                      if re.match(r"\s*auth\s+\S+(\s+\S+)*\s+pam_unix\.so\b", line)), None)
    if unix_auth is None:
        raise SystemExit(f"{name}: no `auth ... pam_unix.so` line to put pam_tilo before")
    lines.insert(unix_auth, AUTH)
    first_password = next((i for i, line in enumerate(lines)
                           if re.match(r"\s*-?password\s", line)), None)
    if first_password is None:
        raise SystemExit(f"{name}: no password phase to put pam_tilo in front of")
    lines.insert(first_password, PASSWORD)
    return "".join(lines)


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__.splitlines()[2], file=sys.stderr)
        return 2
    profile = Path(argv[1])
    for name in TEMPLATES:
        path = profile / name
        path.write_text(edit(path.read_text(encoding="utf-8"), str(path)), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
