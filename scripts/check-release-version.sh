#!/usr/bin/env bash
set -euo pipefail

raw_version="${1:?usage: ./scripts/check-release-version.sh [v]X.Y.Z}"
version="${raw_version#v}"

printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$' || {
  echo "error: version must be X.Y.Z or X.Y.Z-rc.1, got: $raw_version" >&2
  exit 1
}

cargo_version="$(
  awk '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' Cargo.toml
)"

if [[ -z "$cargo_version" ]]; then
  echo "error: could not read workspace version from Cargo.toml" >&2
  exit 1
fi

if [[ "$cargo_version" != "$version" ]]; then
  echo "error: Cargo.toml version $cargo_version does not match release $version" >&2
  exit 1
fi

python3 - "$version" <<'PY'
import json, pathlib, re, sys

version = sys.argv[1]
mismatches = []

changelog = pathlib.Path("CHANGELOG.md")
if not changelog.exists():
    mismatches.append(("CHANGELOG.md", "<missing>"))
else:
    text = changelog.read_text()
    pattern = rf"(?m)^## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}$"
    if not re.search(pattern, text):
        mismatches.append(("CHANGELOG.md", "<missing release heading>"))

# Cargo.lock local workspace packages must match the release version.
# This catches stale local versions that fail --locked builds.
cargo_lock = pathlib.Path("Cargo.lock")
if not cargo_lock.exists():
    mismatches.append(("Cargo.lock", "<missing>"))
else:
    lock_text = cargo_lock.read_text()
    for pkg in ("instantctl", "instantctl-api"):
        pattern = rf'(?m)^name = "{re.escape(pkg)}"\nversion = "([^"]+)"'
        match = re.search(pattern, lock_text)
        if not match:
            mismatches.append((f"Cargo.lock:{pkg}", "<missing package>"))
        elif match.group(1) != version:
            mismatches.append((f"Cargo.lock:{pkg}", match.group(1)))

if mismatches:
    print(f"release metadata version mismatch for {version}:")
    for path, actual in mismatches:
        print(f"  - {path}: {actual!r}")
    raise SystemExit(1)

print(f"release metadata matches {version}")
PY
