#!/bin/sh
# Installs termist on macOS or Linux:
#   curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh
# It finds the newest release (pre-releases included) and runs that release's own
# installer, which checks the download's checksum and puts `termist` in ~/.cargo/bin.
set -eu

repo="eyupucmaz/termist"
# With a token (as in CI) the API allows far more than 60 lookups an hour per address.
if [ -n "${GITHUB_TOKEN:-}" ]; then
    set -- -H "Authorization: Bearer $GITHUB_TOKEN"
else
    set --
fi
tag=$(curl --proto '=https' --tlsv1.2 -fsSL "$@" "https://api.github.com/repos/$repo/releases?per_page=1" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
if [ -z "$tag" ]; then
    echo "termist: could not find a release of $repo" >&2
    exit 1
fi
echo "termist: installing $tag"
curl --proto '=https' --tlsv1.2 -LsSf "https://github.com/$repo/releases/download/$tag/termist-installer.sh" | sh
