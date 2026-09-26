#!/bin/sh
# Stand-in for `claude`: echoes its arguments, then plays the hook sequence of one
# real turn with a permission prompt, waiting for the user's answer.
# Like Claude Code, it runs the hook command strings from the `--settings` file it was
# given, with the event JSON on stdin. TERMIST_HOME is removed from the hooks' env so
# that only TERMIST_RUNTIME_DIR (exported by the daemon) can route them back.
echo "fake-claude $*"
settings=""
while [ $# -gt 0 ]; do
  case "$1" in
    --settings) settings="$2"; shift 2 ;;
    *) shift ;;
  esac
done
hook() {
  cmd=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["hooks"][sys.argv[2]][0]["hooks"][0]["command"])' "$settings" "$1")
  printf '%s' "$2" | env -u TERMIST_HOME sh -c "$cmd"
}
hook SessionStart '{"hook_event_name":"SessionStart","session_id":"fake-session","source":"startup"}'
hook UserPromptSubmit '{"hook_event_name":"UserPromptSubmit","session_id":"fake-session"}'
hook PermissionRequest '{"hook_event_name":"PermissionRequest","session_id":"fake-session","tool_name":"Write"}'
echo "Allow Write? (y/n)"
read answer
hook PostToolUse '{"hook_event_name":"PostToolUse","session_id":"fake-session"}'
hook Stop '{"hook_event_name":"Stop","session_id":"fake-session"}'
echo "done: $answer"
sleep 30
