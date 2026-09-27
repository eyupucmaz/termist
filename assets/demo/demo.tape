# Records docs/demo.gif and docs/screenshot.png with stand-in agents.
# Run from the repository root after `cargo build --release -p termist`:
#   vhs assets/demo/demo.tape

Output docs/demo.gif

Set Shell "bash"
Set FontFamily "MesloLGLDZ Nerd Font Mono"
Set FontSize 15
Set Width 1400
Set Height 600
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set Framerate 24

Hide
Type "export PS1='$ ' DEMO=$PWD/assets/demo BIN=$PWD/target/release/termist"
Enter
Type "export TERMIST_HOME=$(mktemp -d) TERMIST_CLAUDE_BIN=$DEMO/demo-claude TERMIST_CODEX_BIN=$DEMO/demo-codex TERMIST_OPENCODE_BIN=$DEMO/demo-opencode"
Enter
Type "mkdir -p $TERMIST_HOME/code/orbit-api && cd $TERMIST_HOME/code/orbit-api && clear && $BIN"
Enter
Sleep 2.5s

# three tasks, one per agent
Type "p"
Sleep 400ms
Type "Fix the login redirect loop"
Sleep 300ms
Tab
Sleep 300ms
Type "1"
Sleep 300ms
Enter
Sleep 1.5s
Ctrl+A
Escape
Sleep 400ms
Type "p"
Sleep 400ms
Type "Add rate limiting to uploads"
Sleep 300ms
Tab
Sleep 300ms
Type "2"
Sleep 300ms
Enter
Sleep 1.5s
Ctrl+A
Escape
Sleep 400ms
Type "p"
Sleep 400ms
Type "Write tests for billing"
Sleep 300ms
Tab
Sleep 300ms
Type "3"
Sleep 300ms
Enter
Sleep 1.5s
Ctrl+A
Escape
Sleep 3s
Show

# the fleet: running, waiting on you, done
Sleep 1.5s
Type "h"
Sleep 300ms
Type "h"
Sleep 2s
Screenshot docs/screenshot.png
Sleep 1s

# jump to the agent that needs you and answer it
Type "l"
Sleep 400ms
Type "l"
Sleep 1s
Type "."
Sleep 1.2s
Enter
Sleep 1.5s
Type "y"
Sleep 300ms
Enter
Sleep 3s
Ctrl+A
Escape
Sleep 1.5s

# a new task from the quick prompt
Type "p"
Sleep 800ms
Type@45ms "Split the session store into two files"
Sleep 800ms
Tab
Sleep 1s
Type "1"
Sleep 800ms
Enter
Sleep 5s
Ctrl+A
Escape
Sleep 1.5s

# find any session from anywhere
Type "/"
Sleep 1s
Type@110ms "billing"
Sleep 1.2s
Enter
Sleep 2s
Sleep 1s

# leave termist, then stop the daemon of the temporary TERMIST_HOME
Hide
Escape
Sleep 300ms
Type "q"
Sleep 300ms
Type "y"
Sleep 1s
Type "$BIN kill"
Enter
Sleep 1s
