#!/usr/bin/env bash
# Look for credentials in the staged diff. Reads added lines only, so an
# existing string does not re-fire on every commit that touches its file.
#
#   scan-secrets.sh            scan the staged diff
#   scan-secrets.sh --all      scan every tracked file (slow; for an audit)
#
# Deliberately narrow: a scanner that cries wolf gets bypassed, and a bypassed
# scanner catches nothing. Each pattern below matches a credential *shape* that
# has a vendor prefix or a fixed length, not "a long string".
set -uo pipefail
root=$(git rev-parse --show-toplevel)
cd "$root"

if [ "${1:-}" = "--all" ]; then
    subject=$(git ls-files -z | xargs -0 grep -nIH '' 2>/dev/null)
else
    # `+` lines from the staged diff, tagged with their file.
    subject=$(git diff --cached -U0 --diff-filter=ACM | awk '
        /^\+\+\+ b\//   { file = substr($0, 7); next }
        /^\+[^+]/       { print file ":" NR ":" substr($0, 2) }
    ')
fi
# This file is a list of credential shapes, so it matches itself.
subject=$(printf '%s\n' "$subject" | grep -v '^scripts/scan-secrets\.sh:' || true)
[ -z "$subject" ] && exit 0

# name = regex. Anchored to a recognisable prefix or an exact key length.
patterns=(
  "AWS access key=AKIA[0-9A-Z]{16}"
  "GitHub token=gh[pousr]_[A-Za-z0-9]{36,}"
  "GitLab token=glpat-[A-Za-z0-9_-]{20,}"
  "Slack token=xox[abprs]-[A-Za-z0-9-]{10,}"
  "Stripe key=sk_live_[A-Za-z0-9]{24,}"
  "Anthropic key=sk-ant-[A-Za-z0-9_-]{20,}"
  "OpenAI key=sk-[A-Za-z0-9]{48}"
  "Google API key=AIza[A-Za-z0-9_-]{35}"
  "private key block=-----BEGIN [A-Z ]*PRIVATE KEY-----"
  "PuTTY private key=PuTTY-User-Key-File-"
  "JWT=eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\."
  "npm token=npm_[A-Za-z0-9]{36}"
  "password in a URL=[a-z][a-z0-9+.-]*://[^/:@[:space:]]+:[^/@[:space:]]{6,}@"
)

fail=0
for entry in "${patterns[@]}"; do
    name=${entry%%=*}
    regex=${entry#*=}
    hits=$(printf '%s\n' "$subject" | grep -nE -- "$regex" || true)
    [ -z "$hits" ] && continue
    # A committed example is fine as long as it says so on the same line.
    hits=$(printf '%s\n' "$hits" | grep -vEi 'example|placeholder|dummy|fake|redacted|not-?a-?real|test-only|EXAMPLEKEY' || true)
    [ -z "$hits" ] && continue
    printf '  possible %s:\n' "$name" >&2
    printf '%s\n' "$hits" | head -3 | sed 's/^/    /' | cut -c1-160 >&2
    fail=1
done

if [ "$fail" -ne 0 ]; then
    cat >&2 <<'MSG'

  If one of these is genuinely not a credential, add `example` or `placeholder`
  on the same line, or commit with --no-verify and say why in the message.
  If it IS a credential: do not amend it away. It is in your reflog and, once
  pushed, in everyone's. Rotate the key first, then remove it.
MSG
fi
exit "$fail"
