#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_dir=$(cd -- "$script_dir/.." && pwd -P)
env_file=${ILLUMIA_ENV_FILE:-"$script_dir/.env"}
compose_file="$script_dir/compose.yaml"

fail() {
  printf 'compose-prod: %s\n' "$1" >&2
  exit 1
}

read_setting() {
  local name=$1
  local value
  local count
  if value=$(printenv "$name"); then
    :
  else
    [[ -f "$env_file" ]] || fail "missing $env_file"
    count=$(awk -v key="$name" '
      $0 ~ "^[[:space:]]*" key "=" { count += 1 }
      END { print count + 0 }
    ' "$env_file")
    [[ $count -eq 1 ]] || fail "$name must occur exactly once in $env_file"
    value=$(awk -v key="$name" '
      $0 ~ "^[[:space:]]*" key "=" {
        sub("^[[:space:]]*" key "=", "")
        sub("\\r$", "")
        print
      }
    ' "$env_file")
  fi
  printf '%s' "$value"
}

validate_digest() {
  local name=$1
  local value
  value=$(read_setting "$name")
  [[ $value =~ ^[0-9a-f]{64}$ ]] \
    || fail "$name must be exactly one lowercase 64-hex sha256 digest"
  [[ $value != 0000000000000000000000000000000000000000000000000000000000000000 ]] \
    || fail "$name still uses the zero-digest placeholder"
  printf -v "$name" '%s' "$value"
  export "$name"
}

validate_digest ILLUMIA_SERVER_DIGEST
validate_digest ILLUMIA_ML_DIGEST

if grep -Eq '^[[:space:]]+build:' "$compose_file"; then
  fail "production compose must not contain build directives"
fi
[[ $(grep -Ec '^[[:space:]]+pull_policy:[[:space:]]+always[[:space:]]*$' "$compose_file") -eq 2 ]] \
  || fail "each production service must use pull_policy: always"
grep -Fq 'image: ghcr.io/shiningwank0/illumia-server@sha256:${ILLUMIA_SERVER_DIGEST:?' "$compose_file" \
  || fail "production server repository and sha256 prefix must be fixed"
grep -Fq 'image: ghcr.io/shiningwank0/illumia-ml@sha256:${ILLUMIA_ML_DIGEST:?' "$compose_file" \
  || fail "production ML repository and sha256 prefix must be fixed"

if [[ ${1:-} == --check-only ]]; then
  [[ $# -eq 1 ]] || fail "--check-only does not accept additional arguments"
  exit 0
fi
[[ $# -gt 0 ]] || fail "a compose command is required"

arguments=()
for argument in "$@"; do
  case "$argument" in
    build|--build|--build=*|-f|-f*|--file|--file=*|--env-file|--env-file=*|--project-directory|--project-directory=*|--pull|--pull=*)
      fail "production wrapper does not permit $argument"
      ;;
    up)
      arguments+=(up --no-build)
      ;;
    *) arguments+=("$argument") ;;
  esac
done

cd -- "$repository_dir"
exec docker compose --env-file "$env_file" -f "$compose_file" "${arguments[@]}"
