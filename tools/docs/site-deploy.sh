#!/usr/bin/env bash
# Deploy a website's image: pin REPOSITORY@DIGEST in tilo/infra's apps/NAME/deployment.yaml and
# push infra's main, where Argo CD applies it. The pin is the only line it changes; a digest
# infra pins already is no commit. Others push to infra's main too, so a push that lost the race
# is made again on the new head.
#
#   site-deploy.sh NAME REPOSITORY DIGEST
#
# SITES_INFRA_DEPLOY_KEY is the private SSH key of tilo/infra's deploy key for the websites (a
# t secret given to trusted runs of the sites' main alone).
set -euo pipefail
name=${1:?usage: site-deploy.sh NAME REPOSITORY DIGEST}
repository=${2:?usage: site-deploy.sh NAME REPOSITORY DIGEST}
digest=${3:?usage: site-deploy.sh NAME REPOSITORY DIGEST}
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "site-deploy: not a digest: $digest" >&2; exit 1; }
: "${SITES_INFRA_DEPLOY_KEY:?site-deploy: the check needs secrets = [\"SITES_INFRA_DEPLOY_KEY\"]}"

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/site-deploy.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
umask 077
printf '%s\n' "$SITES_INFRA_DEPLOY_KEY" >"$work/key"
export GIT_SSH_COMMAND="ssh -i $work/key -o IdentitiesOnly=yes -o UserKnownHostsFile=$here/code.tilo.so.known_hosts -o StrictHostKeyChecking=yes"
file=apps/$name/deployment.yaml
image=$repository@$digest

for attempt in 1 2 3 4 5; do
  rm -rf "$work/infra"
  git clone -q --depth 1 --branch main git@code.tilo.so:tilo/infra.git "$work/infra"
  grep -qE "$repository@sha256:[0-9a-f]{64}" "$work/infra/$file" \
    || { echo "site-deploy: $file pins no $repository@sha256:… to move" >&2; exit 1; }
  sed -i -E "s#$repository@sha256:[0-9a-f]{64}#$image#" "$work/infra/$file"
  if git -C "$work/infra" diff --quiet; then
    echo "infra pins $image already"
    exit 0
  fi
  git -C "$work/infra" -c user.name='tilo sites' -c user.email='sites@tilo.so' \
    commit -q -a -m "deploy($name): $image" -m "Built from the site's commit ${T_COMMIT:-?} and tilo's main."
  if git -C "$work/infra" push -q origin HEAD:main; then
    echo "$image"
    exit 0
  fi
  echo "site-deploy: infra's main moved; trying again on its new head ($attempt)" >&2
done
echo "site-deploy: could not push the pin to infra's main" >&2
exit 1
