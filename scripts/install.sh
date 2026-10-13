#!/bin/sh
# Installs (or updates to) the latest Agent Review release: downloads the
# .deb or .rpm for this distribution from GitHub, installs it with apt or dnf,
# then deletes the download.
#
#   curl -fsSL https://raw.githubusercontent.com/ralphnajm/agent-review/main/scripts/install.sh | sh
set -eu

REPO=ralphnajm/agent-review

fail() {
    echo "agent-review: $*" >&2
    exit 1
}

# Everything runs from here, so a download cut short runs nothing.
main() {
    command -v curl > /dev/null || fail "curl is needed to download the package"
    [ "$(uname -m)" = x86_64 ] ||
        fail "packages are only built for x86_64; see https://github.com/$REPO#from-source to build it"

    # The package format, from the distribution or else the package manager it has.
    like=""
    if [ -r /etc/os-release ]; then
        like=$(. /etc/os-release && echo " ${ID:-} ${ID_LIKE:-} ")
    fi
    case $like in
        *" debian "* | *" ubuntu "*) format=deb ;;
        *" fedora "* | *" rhel "*) format=rpm ;;
        *)
            if command -v apt-get > /dev/null; then
                format=deb
            elif command -v dnf > /dev/null; then
                format=rpm
            else
                fail "no package for this distribution; see https://github.com/$REPO#from-source to build it"
            fi
            ;;
    esac

    if [ "$(id -u)" -eq 0 ]; then
        sudo=""
    elif command -v sudo > /dev/null; then
        sudo=sudo
    else
        fail "installing needs root: run this as root or install sudo"
    fi

    echo "Finding the latest release of Agent Review…"
    url=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n "s/.*\"browser_download_url\": *\"\([^\"]*\.$format\)\".*/\1/p" | head -n 1)
    [ -n "$url" ] || fail "the latest release has no .$format package"

    dir=$(mktemp -d)
    trap 'rm -rf "$dir"' EXIT
    trap 'exit 1' INT TERM
    # Readable by apt's unprivileged downloader.
    chmod 755 "$dir"
    package="$dir/${url##*/}"
    echo "Downloading ${url##*/}…"
    curl -fL --progress-bar -o "$package" "$url"
    chmod 644 "$package"

    # Non-interactive: when piped into sh, standard input is this script.
    echo "Installing…"
    case $format in
        deb) $sudo apt-get install -y "$package" ;;
        rpm) $sudo dnf install -y "$package" ;;
    esac
    echo "Agent Review is installed: open it from your apps, or run agent-review in a repository."
}

main "$@"
