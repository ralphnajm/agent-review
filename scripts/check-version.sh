#!/bin/sh
# Checks that the current version is described in CHANGELOG.md and in the
# AppStream metadata, so every release ships with notes.
set -eu
root="$(dirname "$0")/.."
version="$("$root/scripts/version.sh")"
"$root/scripts/release-notes.sh" "$version" > /dev/null
if ! grep -q "<release version=\"$version\"" "$root"/data/*.metainfo.xml; then
    echo "data/*.metainfo.xml has no <release> entry for version $version" >&2
    exit 1
fi
echo "Version $version has release notes"
