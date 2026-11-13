#!/usr/bin/env bash
# The visa flow (sdk/python/examples/visa_proof.py) through Tilo's API with curl alone: a backend
# with one key makes a workspace per applicant (a retried create makes none twice), puts the
# applicant's form in, mints the applicant's agent a key for that workspace alone; the agent
# fills the form and writes a cover letter as one run; the backend downloads the letter, sees the
# minted key reach nothing else, and revokes it.
#
#   TILO_API_URL=https://api.tilo.sh TILO_API_KEY=tilo_sk_… bash docs/api/examples/visa.sh
#
# Needs curl, jq and python3 (only to write the sample form). VISA_APPLICANTS (default 3) sets how
# many workspaces it makes.
set -euo pipefail

api=${TILO_API_URL:?TILO_API_URL names the API server}
key=${TILO_API_KEY:?TILO_API_KEY is the backend key}
applicants=${VISA_APPLICANTS:-3}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

say() { printf '[%s] %s\n' "$(date +%T)" "$*"; }
check() { if eval "$1"; then printf '    ok  %s\n' "$2"; else printf '    FAILED  %s\n' "$2" >&2; exit 1; fi; }

# call KEY METHOD PATH [BODY-FILE|-] [HEADER…]: the answer's body on stdout, its status in $work/status.
call() {
  local token=$1 method=$2 path=$3 body=${4:-} args=()
  shift 4 || shift $#
  [[ -n $body ]] && args+=(--data-binary "@$body" -H 'content-type: application/json')
  for header in "$@"; do args+=(-H "$header"); done
  curl -sS -X "$method" -H "authorization: Bearer $token" "${args[@]}" \
    -o "$work/body" -w '%{http_code}' "$api$path" > "$work/status"
  cat "$work/body"
}
json() { printf '%s' "$1" > "$work/request.json"; printf '%s' "$work/request.json"; }
status() { cat "$work/status"; }

say "1. The backend makes $applicants applicant workspaces with its one key"
first=
for index in $(seq 1 "$applicants"); do
  made=$(call "$key" POST /v1/workspaces "$(json "{\"name\": \"Applicant $index\"}")" "idempotency-key: visa-$index-$$")
  [[ $(status) == 201 ]] || { echo "$made" >&2; exit 1; }
  again=$(call "$key" POST /v1/workspaces "$(json "{\"name\": \"Applicant $index\"}")" "idempotency-key: visa-$index-$$")
  check '[[ $(jq -r .id <<<"$again") == $(jq -r .id <<<"$made") ]]' "a retried create answers workspace $index again"
  [[ -n $first ]] || first=$(jq -r .id <<<"$made")
done
listed=$(call "$key" GET /v1/workspaces)
check '[[ $(jq "[.workspaces[] | select(.name | startswith(\"Applicant \"))] | length" <<<"$listed") -ge $applicants ]]' "every one is listed"
base=/v1/workspaces/$first

say "2. The backend puts the form in and mints the agent's key"
# The sample form is the Python example's own, read out of it without importing the SDK.
python3 - "$here/../../../sdk/python/examples/visa_proof.py" "$work/form.pdf" <<'PY'
import ast, sys
tree = ast.parse(open(sys.argv[1]).read())
make = next(node for node in tree.body if getattr(node, "name", "") == "application_pdf")
scope = {}
exec(compile(ast.Module([make], []), sys.argv[1], "exec"), scope)
open(sys.argv[2], "wb").write(scope["application_pdf"]())
PY
uploaded=$(curl -sS -X POST -H "authorization: Bearer $key" -H 'content-type: application/pdf' \
  --data-binary "@$work/form.pdf" "$api$base/uploads")
form=$(jq -r .content <<<"$uploaded")
check '[[ $form == tilo.content:* ]]' "the form is content ($form)"
minted=$(call "$key" POST /v1/keys "$(json "{\"workspace\": \"$first\", \"name\": \"Applicant 1's agent\"}")" "idempotency-key: mint-$$")
check '[[ $(status) == 201 ]]' "a key for that workspace alone"
agent=$(jq -r .token <<<"$minted")
grant=$(jq -r .grant <<<"$minted")

say "3. The agent fills the form and writes the letter, as one run"
act() { # act PATH ACTION ARGS-JSON: the action's answer
  local answer
  answer=$(call "$agent" POST "$base/resources/$1/actions/$2" "$(json "$3")" "idempotency-key: $(uuidgen 2>/dev/null || date +%s%N)" "tilo-run: visa-run" "tilo-run-title: Fill the visa application")
  [[ $(status) == 200 ]] || { echo "$1#$2 answered $(status): $answer" >&2; exit 1; }
  printf '%s' "$answer"
}
act pdf-viewer open '{}' > /dev/null
act pdf-viewer import-pdf-content "{\"content\": \"$form\", \"name\": \"application.pdf\"}" > /dev/null
fields=$(act pdf-viewer/application.pdf read-form-fields '{}')
check 'jq -e ".result.fields | map(.name) | index(\"surname\")" <<<"$fields" > /dev/null' "the form has a surname field"
path=$(jq -c '[.result.fields[] | select(.name == "surname") | .path][0]' <<<"$fields")
act pdf-viewer/application.pdf field-edit \
  "$(jq -cn --argjson path "$path" '{edit: {path: $path, op: "set", value: {kind: "text", value: "Doe"}}}')" > /dev/null
filled=$(act pdf-viewer/application.pdf read-form-fields '{}')
check '[[ $(jq -r ".result.fields[] | select(.name == \"surname\") | .value.value" <<<"$filled") == Doe ]]' "the surname holds what the agent set"
act word-processor open '{}' > /dev/null
act word-processor append-paragraph '{"text": "Dear Visa Officer, I apply for a short-stay business visa."}' > /dev/null
exported=$(act word-processor export-content '{"format": "docx"}')
letter=$(jq -r '[.. | strings | select(startswith("tilo.content:"))][0]' <<<"$exported")
check '[[ $letter == tilo.content:* ]]' "Writer exports the letter as content ($letter)"
curl -sS -H "authorization: Bearer $key" -o "$work/letter.docx" "$api$base/content/$letter"
check '[[ $(head -c 2 "$work/letter.docx") == PK ]]' "the backend downloads a Word file"
link=$(call "$agent" GET "$base/links?path=pdf-viewer/application.pdf")
check '[[ $(jq -r .link <<<"$link") == http* ]]' "the form has a link to hand over"

say "4. The minted key reaches that workspace and nothing else, until it is revoked"
other=$(jq -r "[.workspaces[] | select(.id != \"$first\")][0].id" <<<"$listed")
call "$agent" GET "/v1/workspaces/$other/resources" > /dev/null
check '[[ $(status) == 403 ]]' "another workspace refuses it"
call "$key" DELETE "/v1/keys/$grant" '' "idempotency-key: revoke-$$" > /dev/null
check '[[ $(status) == 200 ]]' "the backend revokes it"
gone=$(call "$agent" GET "$base/resources")
check '[[ $(status) == 401 && $(jq -r .error.code <<<"$gone") == key_revoked ]]' "the revoked key is refused"
say "All of it held"
