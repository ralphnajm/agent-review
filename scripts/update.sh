#!/bin/sh
# Updates a running Agent Review to the latest release, the way it was
# installed, then starts the new version once the running one has quit. The
# app downloads this script from the main branch and runs it when you click
# Self-Update:
#
#   update.sh VERSION EXE PID
#
# VERSION is the running version, EXE the path of its binary, PID its process.
# Exits with 0 when it updated (the app then quits and comes back as the new
# version), 3 when VERSION is already the latest (or newer), and otherwise
# fails with the reason as the last line on standard error.
#
#   .deb or .rpm package   installs the release's package with apt or dnf
#   make install           builds the release's source and installs it to the same prefix
set -eu

REPO=ralphnajm/agent-review
APP_ID=io.github.ralphnajm.AgentReview
UP_TO_DATE=3

fail() {
    echo "$*" >&2
    exit 1
}

# Runs a command as root. The app has no terminal, so pkexec asks for the
# password in a dialog.
as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
        return
    fi
    command -v pkexec > /dev/null || fail "Installing needs root, and pkexec is not available to ask for your password"
    pkexec "$@" || {
        status=$?
        # pkexec exits with 126 when the password dialog is dismissed.
        [ "$status" -ne 126 ] || fail "Update cancelled"
        exit "$status"
    }
}

# Downloads the latest release's .deb or .rpm into $dir and prints its path.
download_package() {
    url=$(echo "$release" | sed -n "s/.*\"browser_download_url\": *\"\([^\"]*\.$1\)\".*/\1/p" | head -n 1)
    [ -n "$url" ] || fail "The latest release has no .$1 package"
    package="$dir/${url##*/}"
    curl -fsSL -o "$package" "$url"
    # Readable by apt's unprivileged downloader.
    chmod 644 "$package"
    echo "$package"
}

# Everything runs from here, so a download cut short runs nothing.
main() {
    [ $# -eq 3 ] || fail "Usage: update.sh VERSION EXE PID"
    version=$1 exe=$2 pid=$3
    command -v curl > /dev/null || fail "curl is needed to download the update"

    case $exe in
        */target/debug/* | */target/release/*)
            fail "This is a development build: update it with git pull and cargo build"
            ;;
    esac

    release=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")
    latest=$(echo "$release" | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$latest" ] || fail "Could not find the latest release"
    # Also true for a build newer than the latest release.
    if [ "$(printf '%s\n' "$latest" "$version" | sort -V | tail -n 1)" = "$version" ]; then
        echo "Agent Review $version is up to date"
        exit "$UP_TO_DATE"
    fi

    dir=$(mktemp -d)
    trap 'rm -rf "$dir"' EXIT
    trap 'exit 1' INT TERM
    chmod 755 "$dir"

    echo "Updating Agent Review $version to $latest…"
    if command -v dpkg > /dev/null && dpkg -S "$exe" > /dev/null 2>&1; then
        package=$(download_package deb)
        as_root apt-get install -y "$package"
    elif command -v rpm > /dev/null && rpm -qf "$exe" > /dev/null 2>&1; then
        package=$(download_package rpm)
        as_root dnf install -y "$package"
    else
        # Installed with `make install PREFIX=…`, which puts the binary in $prefix/bin.
        prefix=$(dirname "$(dirname "$exe")")
        [ -f "$prefix/share/applications/$APP_ID.desktop" ] ||
            fail "Could not tell how $exe was installed: update it the way you installed it"
        # rustup installs cargo there, which the PATH of a desktop session may lack.
        PATH="$PATH:$HOME/.cargo/bin"
        for tool in git make cargo; do
            command -v "$tool" > /dev/null || fail "Building the update needs $tool"
        done
        git clone --quiet --depth 1 --branch "v$latest" "https://github.com/$REPO.git" "$dir/src"
        make -C "$dir/src" release
        if [ -w "$prefix/bin" ]; then
            make -C "$dir/src" install PREFIX="$prefix"
        else
            # Already built: CARGO=true keeps root from running cargo again.
            as_root make -C "$dir/src" install PREFIX="$prefix" CARGO=true
        fi
    fi

    # The app quits when this script succeeds; start the new version once it has.
    (
        while kill -0 "$pid" 2> /dev/null; do sleep 0.2; done
        exec "$exe"
    ) < /dev/null > /dev/null 2>&1 &
    echo "Updated Agent Review to $latest"
}

main "$@"
