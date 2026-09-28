# Records docs/look.gif, docs/settings.png, docs/moda.png and docs/help.png:
# the Istanbul scene, the themes switched from the settings, and the help.
# Run from the repository root after `cargo build --release -p termist`:
#   vhs assets/demo/look.tape

Output docs/look.gif

Set Shell "bash"
Set FontFamily "MesloLGLDZ Nerd Font Mono"
Set FontSize 15
Set Width 1400
Set Height 800
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set Framerate 24

Hide
Type "export PS1='$ ' DEMO=$PWD/assets/demo BIN=$PWD/target/release/termist"
Enter
# the scene at dusk, whatever the hour of the recording
Type "export TZ=DEMO$(( (10#$(date -u +%H) + 18) % 24 - 12 )) TERMIST_HOME=/tmp/termist-demo TERMIST_CLAUDE_BIN=$DEMO/demo-claude TERMIST_CODEX_BIN=$DEMO/demo-codex TERMIST_OPENCODE_BIN=$DEMO/demo-opencode"
Enter
Type "rm -rf $TERMIST_HOME && mkdir -p $TERMIST_HOME/config $TERMIST_HOME/code/orbit-api && cp $DEMO/config.toml $TERMIST_HOME/config/ && cd $TERMIST_HOME/code/orbit-api && clear && $BIN"
Enter
Sleep 3s

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
Type "h"
Sleep 300ms
Type "h"
Sleep 1s
Show

# Üsküdar, the default theme
Sleep 1.5s

# the settings: every change applies at once
Type "s"
Sleep 1.8s
Screenshot docs/settings.png
Sleep 500ms
Right
Sleep 2.2s
Right
Sleep 2.2s
Left
Sleep 1.2s
Escape
Sleep 1.5s
Screenshot docs/moda.png
Sleep 1s

# the help lists every key as it is bound now, under the scene
Type "?"
Sleep 2.5s
Type@120ms "jjjjjjjjjjjjjjjjjjjj"
Sleep 1.5s
Screenshot docs/help.png
Sleep 500ms
Type@120ms "jjjjjjjjjjjj"
Sleep 2s
Escape
Sleep 1.5s

# leave termist, then stop the daemon of the temporary TERMIST_HOME
Hide
Type "q"
Sleep 300ms
Type "y"
Sleep 1s
Type "$BIN kill"
Enter
Sleep 1s
