# Records docs/prs.gif and docs/prs-*.png: a pull request from the list to an agent.
# GitHub is the stand-in demo-gh, with made-up repos (acme/site, acme/api) and people;
# prs-setup makes the folder of repos under a temporary TERMIST_HOME.
# Run from the repository root after `cargo build --release -p termist`:
#   vhs assets/demo/prs.tape

Output docs/prs.gif

Set Shell "bash"
Set FontFamily "MesloLGLDZ Nerd Font Mono"
# 14, not 15: the diff has room to stand side by side
Set FontSize 14
Set Width 1400
Set Height 800
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set Framerate 24

Hide
Type "export PS1='$ ' DEMO=$PWD/assets/demo BIN=$PWD/target/release/termist"
Enter
Type "export TERMIST_HOME=/tmp/termist-site TERMIST_GH_BIN=$DEMO/demo-gh TERMIST_CLAUDE_BIN=$DEMO/demo-claude TERMIST_CODEX_BIN=$DEMO/demo-codex TERMIST_OPENCODE_BIN=$DEMO/demo-opencode"
Enter
Type "$DEMO/prs-setup && cd $TERMIST_HOME/code/acme && clear && $BIN"
Enter
Sleep 3s

# two agents at work in the folder of repos
Type "p"
Sleep 400ms
Type "Write tests for billing"
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
Type "Add rate limiting to uploads"
Sleep 300ms
Tab
Sleep 300ms
Type "1"
Sleep 300ms
Enter
Sleep 1.5s
Ctrl+A
Escape
Sleep 3s
Show

Sleep 1.5s

# the pull requests of every repo in the folder; #212 asks for your review
Type "v"
Sleep 1.5s
Type@400ms "jjj"
Sleep 1.5s
Screenshot docs/prs-inbox.png
Sleep 500ms

# the whole pull request: overview, then the conversation with its line threads
Enter
Sleep 2s
Tab
Sleep 2s
# mark both review threads for an agent
Type "n"
Sleep 500ms
Space
Sleep 600ms
Type "n"
Sleep 500ms
Space
Sleep 1.5s
Screenshot docs/prs-conversation.png
Sleep 500ms

# the diff: unified, then side by side
Type "d"
Sleep 2.5s
Type "s"
Sleep 2s
Screenshot docs/prs-diff.png
Sleep 500ms
Type "s"
Sleep 800ms

# a comment on two lines of the next file; it waits in your review
Type "J"
Sleep 1.2s
Type@300ms "jjj"
Type "v"
Sleep 300ms
Type "j"
Sleep 500ms
Type "c"
Sleep 800ms
Type@40ms "Could IDLE come from the config? The admin site wants longer sessions."
Sleep 1s
Enter
Sleep 2.5s

# the review: a verdict and a word
Type "A"
Sleep 1s
Type@40ms "Two small things in the threads, then it is good to go."
Sleep 600ms
Tab
Sleep 500ms
Tab
Sleep 1.2s
Screenshot docs/prs-review.png
Sleep 500ms
Enter
Sleep 2.5s

# hand the marked threads to an agent: a worktree on the branch, a new task there
Escape
Sleep 1.2s
Type "a"
Sleep 2.5s
Enter
Sleep 6s
Ctrl+A
Escape
Sleep 2s
Screenshot docs/prs-bands.png
Sleep 2s

# leave termist, stop the daemon, and remove the temporary TERMIST_HOME
Hide
Type "q"
Sleep 300ms
Type "y"
Sleep 1s
Type "$BIN kill && cd / && rm -rf $TERMIST_HOME"
Enter
Sleep 1s
