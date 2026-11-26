#!/usr/bin/env bash
set -euo pipefail
umask 077

# This deliberately exercises Security.framework against a disposable VM's
# temporary user keychain. Never use a developer or self-hosted keychain.
if [[ "$(uname -s)" != Darwin || "${GITHUB_ACTIONS:-}" != true ||
      "${RUNNER_ENVIRONMENT:-}" != github-hosted || -z "${RUNNER_TEMP:-}" ]]; then
  echo "This smoke test requires a GitHub-hosted macOS runner." >&2
  exit 64
fi

binary="$PWD/target/debug/instantctl"
[[ -x "$binary" ]]
keychain_path() {
  /usr/bin/security default-keychain -d user | sed -n 's/^[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p'
}
original_default="$(keychain_path)"
[[ -f "$original_default" ]]
scratch="$(mktemp -d "$RUNNER_TEMP/instantctl-keychain.XXXXXX")"
keychain="$scratch/smoke.keychain-db"
profile="ci-keychain-${scratch##*.}"
lock="$HOME/Library/Caches/hpe-network/$profile.lock"
default_changed=false

cleanup() {
  local status=$?
  trap - EXIT
  if "$default_changed"; then
    /usr/bin/security default-keychain -d user -s "$original_default" || status=1
  fi
  if [[ -e "$keychain" ]]; then
    /usr/bin/security delete-keychain "$keychain" || status=1
  fi
  rm -f -- "$lock" || status=1
  rm -rf -- "$scratch" || status=1
  exit "$status"
}
[[ ! -e "$lock" ]]
trap cleanup EXIT

keychain_password="$(openssl rand -hex 24)"
/usr/bin/security create-keychain -p "$keychain_password" "$keychain"
/usr/bin/security unlock-keychain -p "$keychain_password" "$keychain"
unset keychain_password
default_changed=true
/usr/bin/security default-keychain -d user -s "$keychain"
[[ "$(keychain_path)" -ef "$keychain" ]]

# The current on-disk profile format predates keyring-core 1.1.0. Seed that
# format directly without logging into HPE or giving the binary real tokens.
site=123e4567-e89b-12d3-a456-426614174000
remove_port=02:00:00:00:00:01:1
keep_port=02:00:00:00:00:01:2
fixture="$(printf '{"username":"fixture","refresh":"dummy-refresh","access":"dummy-access","access_expiry":2000000000,"refresh_pending":false,"default_site":"%s","protected_ports":["%s","%s"]}' "$site" "$remove_port" "$keep_port")"
/usr/bin/security add-generic-password -s hpe-network -a "$profile" -w "$fixture" -T "$binary" "$keychain"

unset HPE_INSTANT_ON_TOKEN
# An explicit MAC takes the local-only metadata update path. Reject accidental
# HTTP(S) requests as well, including any token refresh.
export http_proxy=http://127.0.0.1:9 https_proxy=http://127.0.0.1:9 no_proxy=
export HTTP_PROXY="$http_proxy" HTTPS_PROXY="$https_proxy" NO_PROXY=
"$binary" --format json --profile "$profile" auth status >"$scratch/status.json"
"$binary" --format json --profile "$profile" profile protected-ports remove "$remove_port" >"$scratch/update.json"
"$binary" --format json --profile "$profile" profile show >"$scratch/readback.json"

python3 - "$scratch" "$profile" "$site" "$keep_port" <<'PY'
import json
import pathlib
import sys

directory, profile, site, remaining = sys.argv[1:]
directory = pathlib.Path(directory)
assert json.loads((directory / "status.json").read_text()) == {
    "profile": profile, "username": "fixture", "default_site": site,
    "access_expiry": 2_000_000_000, "refresh_pending": False,
}
expected = {"profile": profile, "default_site": site, "protected_ports": [remaining]}
for name in ("update.json", "readback.json"):
    assert json.loads((directory / name).read_text()) == expected, name
PY
