#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == "--help" ]]; then
  echo "Usage: apps/db-client/tests/run-live.sh"
  echo "Start disposable PostgreSQL and MariaDB containers and run the ignored live tests."
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "Usage: apps/db-client/tests/run-live.sh" >&2
  exit 2
fi

for command in podman cargo; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "db-client live tests require $command" >&2
    exit 1
  fi
done

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

postgres_container="tilo-db-test-postgres-$$"
mysql_container="tilo-db-test-mariadb-$$"

cleanup() {
  podman rm --force "$postgres_container" "$mysql_container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

wait_for_database() {
  local container=$1
  shift
  for _ in {1..90}; do
    if podman exec "$container" "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "$container did not become ready" >&2
  podman logs "$container" >&2 || true
  return 1
}

podman run --detach --rm \
  --name "$postgres_container" \
  --publish 127.0.0.1::5432 \
  --env POSTGRES_HOST_AUTH_METHOD=trust \
  "${TILO_DB_TEST_POSTGRES_IMAGE:-docker.io/library/postgres:16}"

podman run --detach --rm \
  --name "$mysql_container" \
  --publish 127.0.0.1::3306 \
  --env MARIADB_ALLOW_EMPTY_ROOT_PASSWORD=1 \
  "${TILO_DB_TEST_MARIADB_IMAGE:-docker.io/library/mariadb:11}"

wait_for_database "$postgres_container" pg_isready --username postgres --dbname postgres
wait_for_database "$mysql_container" mariadb-admin ping --user root --silent

postgres_binding=$(podman port "$postgres_container" 5432/tcp)
mysql_binding=$(podman port "$mysql_container" 3306/tcp)

TILO_DB_TEST_PG_HOST=127.0.0.1 \
TILO_DB_TEST_PG_PORT="${postgres_binding##*:}" \
TILO_DB_TEST_MYSQL_HOST=127.0.0.1 \
TILO_DB_TEST_MYSQL_PORT="${mysql_binding##*:}" \
  cargo test -p db-client --test live_roundtrip -- \
    --ignored --nocapture --test-threads 1
