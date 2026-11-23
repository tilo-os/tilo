#!/usr/bin/env bash
# Deploy a website's image: write its manifests, with the image pinned by digest, to the site
# repository's `deploy` branch, which an Argo CD Application in tilo/infra applies. The branch
# holds one file, site.yaml: MANIFEST with every `IMAGE` replaced by REPOSITORY@DIGEST. A digest
# the branch pins already is no commit. A newer head of the site's main deploys after this one,
# so a superseded run pushes nothing and never rolls the site back.
#
#   site-deploy.sh SITE_REPOSITORY MANIFEST REPOSITORY DIGEST
#
# SITE_REPOSITORY is the forge's name (tilo/tilolang-site). SITE_DEPLOY_KEY is the private SSH
# key of that repository's write deploy key (a t secret given to trusted runs of its main alone).
set -euo pipefail
usage='usage: site-deploy.sh SITE_REPOSITORY MANIFEST REPOSITORY DIGEST'
site=${1:?$usage}
manifest=${2:?$usage}
repository=${3:?$usage}
digest=${4:?$usage}
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "site-deploy: not a digest: $digest" >&2; exit 1; }
grep -q 'IMAGE' "$manifest" || { echo "site-deploy: $manifest names no IMAGE to pin" >&2; exit 1; }
: "${SITE_DEPLOY_KEY:?site-deploy: the check needs secrets = [\"SITE_DEPLOY_KEY\"]}"

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/site-deploy.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
(umask 077 && printf '%s\n' "$SITE_DEPLOY_KEY" >"$work/key")
export GIT_SSH_COMMAND="ssh -i $work/key -o IdentitiesOnly=yes -o UserKnownHostsFile=$here/code.tilo.so.known_hosts -o StrictHostKeyChecking=yes"
remote=git@code.tilo.so:$site.git
image=$repository@$digest

if [[ -n ${T_COMMIT:-} ]]; then
  main=$(git ls-remote "$remote" refs/heads/main | cut -f1)
  if [[ $main != "$T_COMMIT" ]]; then
    echo "the site's main is $main now, not $T_COMMIT: its own run deploys"
    exit 0
  fi
fi

git init -q "$work/deploy"
cd "$work/deploy"
git remote add origin "$remote"
if git fetch -q --depth 1 origin deploy 2>/dev/null; then
  git checkout -q FETCH_HEAD
else
  git checkout -q --orphan deploy
fi
sed "s#IMAGE#$image#g" "$manifest" >site.yaml
git add site.yaml
if git diff --cached --quiet; then
  echo "the site runs $image already"
  exit 0
fi
git -c user.name='tilo sites' -c user.email='sites@tilo.so' \
  commit -q -m "Deploy $image" -m "Built from the site's commit ${T_COMMIT:-?} and tilo's main."
git push -q origin HEAD:refs/heads/deploy
echo "$image"
