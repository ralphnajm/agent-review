#!/bin/sh
# Prints the CHANGELOG.md section of a version (default: the current one),
# without its heading. Fails if the section is missing or empty.
set -eu
root="$(dirname "$0")/.."
version="${1:-$("$root/scripts/version.sh")}"
notes="$(awk -v heading="## [$version]" '
    index($0, heading) == 1 { found = 1; next }
    found && /^## \[/ { exit }
    found { print }
' "$root/CHANGELOG.md" | sed -e '/./,$!d')"
if [ -z "$notes" ]; then
    echo "CHANGELOG.md has no notes for version $version" >&2
    exit 1
fi
printf '%s\n' "$notes"
