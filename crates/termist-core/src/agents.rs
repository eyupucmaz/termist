//! What an agent is told of termist, when `[agents] teach` is on.

/// Added to every agent's system prompt: the commands, and that they are for when the
/// user asks.
pub const TEACH: &str = "\
You run inside termist, which shows each coding agent as a card. Only when the user asks for \
work in parallel, for another agent, or for a separate worktree, use these commands:
- `termist spawn \"<task>\" [--harness claude|codex|opencode] [--model M] [--effort E] \
[--preset NAME] [--worktree BRANCH] [--wait]` starts another agent on the task, in your folder \
or in BRANCH's worktree. It returns at once; with --wait it waits, and exits 0 when that agent \
is done, 2 when it waits for the user, 1 when it stopped.
- `termist worktree BRANCH` makes (or finds) BRANCH's git worktree and moves your card there. \
You keep running: from then on work in the folder it prints, with `cd <folder> && ...` in \
every shell command and absolute paths for files.
- `termist open FILE[:LINE]` opens a file in the user's editor.
Never start an agent the user did not ask for.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_name_every_command_and_when_to_use_them() {
        for word in [
            "termist spawn",
            "--worktree BRANCH",
            "--wait",
            "termist worktree BRANCH",
            "termist open",
            "Only when the user asks",
        ] {
            assert!(TEACH.contains(word), "{word}");
        }
        assert!(TEACH.lines().count() <= 12, "a few lines");
    }
}
