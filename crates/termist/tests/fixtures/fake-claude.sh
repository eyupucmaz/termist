#!/bin/sh
# Stand-in for `claude`: echoes its arguments, then plays the hook sequence of one
# real turn with a permission prompt (spike 2026-09-25), waiting for the user's answer.
echo "fake-claude $*"
hook() { printf '%s' "$2" | "$TERMIST_BIN" hook --harness claude "$1"; }
hook SessionStart '{"hook_event_name":"SessionStart","session_id":"fake-session","source":"startup"}'
hook UserPromptSubmit '{"hook_event_name":"UserPromptSubmit","session_id":"fake-session"}'
hook PermissionRequest '{"hook_event_name":"PermissionRequest","session_id":"fake-session","tool_name":"Write"}'
echo "Allow Write? (y/n)"
read answer
hook PostToolUse '{"hook_event_name":"PostToolUse","session_id":"fake-session"}'
hook Stop '{"hook_event_name":"Stop","session_id":"fake-session"}'
echo "done: $answer"
sleep 30
