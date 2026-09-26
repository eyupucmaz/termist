#!/bin/sh
# Stand-in agent CLI: prints each argument on its own line in brackets, so a space or
# a quote inside one argument stays visible, then echoes one line typed into it.
for a in "$@"; do printf '[%s]\n' "$a"; done
read line
echo "follow-up: $line"
sleep 30
