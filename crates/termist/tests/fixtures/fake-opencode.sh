#!/bin/sh
# Stand-in for `opencode`: checks that termist's plugin is in the config dir it was
# given, then sends the events the plugin would forward (including a subagent's, which
# must be ignored) through `termist hook`, for one turn with a permission prompt.
echo "fake-opencode $*"
if [ -f "$OPENCODE_CONFIG_DIR/plugins/termist.ts" ]; then echo "plugin: ok"; fi
hook() { printf '%s' "$2" | env -u TERMIST_HOME "$TERMIST_BIN" hook --harness opencode "$1"; }
hook session.created '{"type":"session.created","properties":{"info":{"id":"ses_parent"}}}'
hook session.created '{"type":"session.created","properties":{"info":{"id":"ses_child","parentID":"ses_parent"}}}'
hook chat.message '{"sessionID":"ses_parent"}'
hook permission.asked '{"type":"permission.asked","properties":{"sessionID":"ses_parent"}}'
echo "Allow? (y/n)"
read answer
hook permission.replied '{"type":"permission.replied","properties":{"sessionID":"ses_parent","reply":"once"}}'
hook permission.asked '{"type":"permission.asked","properties":{"sessionID":"ses_child"}}'
hook session.idle '{"type":"session.idle","properties":{"sessionID":"ses_parent"}}'
echo "done: $answer"
sleep 30
