#!/bin/sh
# Opens termist with the stand-in agents of assets/demo and one theme, to judge the
# theme by eye in a real terminal. Nothing touches your own termist: it runs under a
# throwaway TERMIST_HOME, and its daemon is stopped when you quit.
#
#   tools/theme-demo.sh [uskudar|moda|terminal] [truecolor|256|16]
#
# In termist: `p`, a prompt, Enter starts an agent; Tab in the quick prompt picks
# claude, codex or opencode. The stand-ins go running → waiting → done like real ones.
set -eu
theme=${1:-uskudar}
colors=${2:-auto}
root=$(cd "$(dirname "$0")/.." && pwd)
cargo build --quiet --manifest-path "$root/Cargo.toml" -p termist
bin=$root/target/debug/termist
demo=$root/assets/demo

TERMIST_HOME=$(mktemp -d)
export TERMIST_HOME
export TERMIST_CLAUDE_BIN=$demo/demo-claude
export TERMIST_CODEX_BIN=$demo/demo-codex
export TERMIST_OPENCODE_BIN=$demo/demo-opencode
mkdir -p "$TERMIST_HOME/config" "$TERMIST_HOME/code/orbit-api"
printf 'theme = "%s"\ncolors = "%s"\n' "$theme" "$colors" >"$TERMIST_HOME/config/config.toml"

trap '"$bin" kill >/dev/null 2>&1; rm -rf "$TERMIST_HOME"' EXIT
cd "$TERMIST_HOME/code/orbit-api"
"$bin"
