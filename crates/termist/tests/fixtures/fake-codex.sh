#!/bin/sh
# Stand-in for `codex`: prints its arguments, then does what Codex does with the
# `-c hooks.<Event>=[{hooks=[{type="command",command="…"}]}]` flags it was given: runs
# each hook's command through a shell with the event JSON on stdin, for one turn with a
# permission prompt. TERMIST_HOME is removed so only TERMIST_RUNTIME_DIR can route hooks.
echo "fake-codex $*"
map=$(mktemp)
python3 - "$@" > "$map" <<'PY'
import re, sys
pat = re.compile(r'hooks\.(\w+)=\[\{hooks=\[\{type="command",command="((?:[^"\\]|\\.)*)"\}\]\}\]$')
for a in sys.argv[1:]:
    m = pat.match(a)
    if m:
        print(m.group(1) + "\t" + m.group(2).replace('\\"', '"').replace('\\\\', '\\'))
PY
hook() {
  cmd=$(grep "^$1	" "$map" | cut -f2-)
  printf '%s' "$2" | env -u TERMIST_HOME sh -c "$cmd"
}
if [ "$1" = "resume" ]; then echo "resumed $2"; fi
hook SessionStart '{"session_id":"codex-e2e-1","source":"startup"}'
hook UserPromptSubmit '{"session_id":"codex-e2e-1"}'
hook PermissionRequest '{"session_id":"codex-e2e-1","tool_input":{"description":"write hello.txt"}}'
echo "Allow? (y/n)"
read answer
hook PostToolUse '{"session_id":"codex-e2e-1"}'
hook Stop '{"session_id":"codex-e2e-1"}'
echo "done: $answer"
rm -f "$map"
sleep 30
