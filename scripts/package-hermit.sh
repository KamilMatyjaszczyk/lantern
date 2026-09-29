#!/usr/bin/env bash
# Package sources for a native build on Hermit; never bundle the PC executable.
set -euo pipefail
if (($#)); then
    printf 'Usage: scripts/package-hermit.sh\nCreates lantern-hermit.tar.gz in the project directory.\n'
    [[ $# == 1 && ($1 == --help || $1 == -h) ]] && exit 0
    exit 2
fi

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
archive="$project_dir/lantern-hermit.tar.gz"
staging=$(mktemp "$project_dir/.lantern-hermit.XXXXXX")
trap 'rm -f -- "$staging"' EXIT

# An explicit source list keeps build output, local settings and previous
# archives out. Extra exclusions also protect against test database/cache files.
tar -czf "$staging" -C "$project_dir" \
    --exclude=target --exclude=__pycache__ --exclude='*.pyc' \
    --exclude='*.db' --exclude='*.db-wal' --exclude='*.db-shm' --exclude='*.db-journal' \
    --transform 's,^,lantern/,' \
    Cargo.toml Cargo.lock README.md src scripts packaging docs tests

# Replace the previous package only after tar completes successfully.
chmod 644 "$staging"
mv -f -- "$staging" "$archive"
printf 'Created: %s\nCopy this file to Hermit, then run:\n' "$archive"
printf '  tar -xzf lantern-hermit.tar.gz\n  cd lantern\n  ./scripts/install-user.sh\n'
