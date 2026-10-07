# Changelog

## Unreleased

From a pull request to an agent: its branch in a worktree, its review comments to the agent working
on it.

### Added

- **A worktree for a pull request.** In a pull request, `w` opens a worktree on its branch beside the
  repo (`<repo>-worktrees/<branch>`, checked out by `gh pr checkout`), or uses the one that has the
  branch already, and opens the new-task prompt there with the pull request's line in it.
- **Review comments to an agent.** `Space` marks threads in the conversation or the diff; `a` hands the
  marked ones (or the one under the cursor) to an agent: the card already on that branch, as a
  follow-up you can edit, or a new agent in the worktree.
- **Bands.** Cards stand in bands, one per worktree, under the branch and its pull request with its
  checks and open threads. termist follows a Claude Code session into its own worktree.
- **A card's pull request.** `Shift+V` opens it in the browser; `v` opens the list on it; a click on its
  number in the band opens it.

### Updating

This version changes how the TUI and the daemon talk, and what the daemon keeps of each session. Run
`termist update`, then, when your sessions can stop, `termist kill` and start `termist` again.

## 0.1.4 - 2026-10-06

Writing to pull requests from termist, and the pane on any side.

### Added

- **Comments and reviews.** In a pull request, `c` comments on it; in its diff, `c` comments on the line
  under the new cursor, or on a range chosen with `v`, and `Ctrl+s` puts those lines in a suggestion.
  Line comments wait in your review, marked *pending*, until `A` sends it as a comment, an approval or a
  change request. On a thread `r` replies and `x` resolves it or opens it again; `e` and `D` edit and
  delete your own comments. A box closed with Esc keeps its words; one GitHub refuses stays open and
  says why.
- **The pane on any side.** `pane_position` (and the settings, `s`) takes `left` and `top` too. Beside
  the pane the cards stand in one column, so a wide screen gives the pane all the room. `Ctrl+a z`
  moves it between beside and under (or above) the cards, keeping its side.

### Changed

- In a pull request's conversation `j` / `k` go from comment to comment; in its diff they move a line
  cursor.

### Updating

This version changes how the TUI and the daemon talk. Run `termist update`, then, when your sessions
can stop, `termist kill` and start `termist` again.

## 0.1.3 - 2026-10-05

A pull request's diff in termist, and the mouse everywhere.

### Added

- **A pull request's diff (`d`).** In a pull request, `d` (or Enter on a file) opens its files as a tree
  beside one file's changes: unified, or old and new side by side with `s`, the words that changed
  marked, line comments under their lines. `J` / `K` go from file to file, `{` / `}` from hunk to hunk,
  and `Ctrl+r` marks a file viewed on GitHub, as the checkbox there does, then shows the next one.
- **The mouse, everywhere.** A click on a project tab goes to it, on a card selects it (again: types into
  it), on the pane types into it. In a pull request the tabs, threads, checks and files take clicks;
  in its diff, the tree and the threads. The wheel moves what is under it.

### Updating

This version changes how the TUI and the daemon talk. Run `termist update`, then, when your sessions
can stop, `termist kill` and start `termist` again.

## 0.1.2 - 2026-10-03

A project's pull requests in termist: `v` lists them repo by repo, with the reviews asked of you,
checks and conflicts, and opens one whole with its conversation and line threads.

### Added

- **Pull requests (`v`).** A project's open pull requests, read through the GitHub CLI: repo by repo for a
  folder of repos, with reviews asked of you (`⇄` on the tab, a toast), checks and conflicts at a glance.
  Enter opens one whole: description, conversation with line threads, checks, files. `m` chooses the
  repos and the account each is read with, `b` opens it in the browser, `R` reads GitHub again. Turn it
  off with `[github] enabled = false` or in the settings (`s`).

### Updating

This version changes how the TUI and the daemon talk. Run `termist update`, then, when your sessions
can stop, `termist kill` and start `termist` again: until then the new TUI cannot reach the old daemon.
Agents you stop this way come back with `Enter` on their card.

## 0.1.1 - 2026-10-02

Toasts tell you when an agent needs you, a status line shows the machine, the model list knows what
each CLI offers, and there are thirteen new themes and a second sound.

### Added

- **A second sound, a kedi, and a sound for each alert.** The settings (`s`) now have a *done sound* and
  a *waiting sound*: one for an agent that is done, one for an agent that asks you something. Each can be
  the martı (a seagull), the kedi (a cat), the system's sound, the terminal bell, or off, and you hear a
  sound as you pick it. In `config.toml` they are `notify.done_sound` and `notify.waiting_sound`, with
  the values `marti`, `kedi`, `system`, `bell` and `off`. `termist sound test kedi` plays the cat.
- **Toasts in the top right.** When an agent starts waiting for you or finishes, in any project, a toast
  says so; click it to go to the card. A copy from the pane gets one too. Turn agent toasts off with
  `notify.toasts = false` or in the settings (`s`).
- **A status line.** The top right shows CPU, memory, battery and the time, like tmux's. Each part can
  be turned off in the settings or under `[status]`.
- **Models to choose from.** `Ctrl+O` in a new task lists the models each CLI offers: Claude Code's
  `opus`, `sonnet`, `haiku` and `fable`, and what `codex debug models` and `opencode models` say. Type to
  filter; the effort levels follow the model.
- **Thirteen new themes.** Aksaray, Kadıköy, Beşiktaş, Balat, Kapalıçarşı, Adalar and Bebek, and
  Catppuccin Mocha and Latte, Tokyo Night, Gruvbox Dark, Nord and Dracula. Your own themes go in the
  `themes` folder next to `config.toml`.

### Changed

- `notify.sounds` still works and sets both sounds, unless one has its own setting. Its old value
  `istanbul` means `marti`.
- In the model list (`Ctrl+O`), `j`, `k`, `h`, `l` and `q` now type into the filter. Move with the
  arrows and close it with `Esc`.

### Updating

This version changes how the TUI and the daemon talk. Run `termist update`, then, when your sessions
can stop, `termist kill` and start `termist` again: until then the new TUI cannot reach the old daemon.
Agents you stop this way come back with `Enter` on their card.

## 0.1.0 - 2026-10-02

termist leaves beta. Everything the betas brought, themes, Istanbul scenes, sounds, notifications,
settings and scrolling back, is now the first full release, and text in the pane can be copied with the
mouse.

### Added

- **Drag over the pane to copy.** Select text in any session, agent or shell, with the mouse; it goes to
  the clipboard when you let go, and stays highlighted until you click, type or scroll. The pane's
  title says how many characters were copied. termist writes the system clipboard itself, so this works
  inside tmux too, and sends OSC 52 for terminals reached over ssh. `Shift`+drag still gives you your
  terminal's own selection.

### Updating

`termist update`, then quit termist (`q`) and start it again. The daemon did not change, so your
sessions keep running; no `termist kill` is needed.

## 0.1.0-beta.4 - 2026-09-30

You can scroll back through what an agent wrote.

### Added

- **Scroll back through a session's output.** Turn the wheel over the pane, press `PgUp` in the grid or
  `Ctrl+a [` while typing into a card. Then `↑`/`↓` or `j`/`k` move a line, `PgUp`/`PgDn` a page,
  `Ctrl+u`/`Ctrl+d` half a page and `g` goes to the oldest line; `q`, `Esc`, `G` or scrolling to the
  bottom takes you back to the live screen. The pane's title shows how far back you are, and output
  that arrives meanwhile does not move the view. Nothing you press while scrolling reaches the agent.
- **Full-screen agents scroll themselves.** Claude Code and OpenCode keep their own history: the wheel
  goes to them, and `PgUp` or `Ctrl+a [` asks them to scroll. Before, the wheel reached Claude Code as
  arrow keys and walked its prompt history instead.
- **termist takes the mouse** so the wheel works. To select text, hold `Shift` while you drag. Set
  `mouse = false` (or turn it off in the settings, `s`) to give the mouse back to your terminal. Inside
  tmux, the wheel needs tmux's `set -g mouse on`.

### Updating

This version changes how the TUI and the daemon talk. Run `termist update`, then, when your sessions
can stop, `termist kill` and start `termist` again: until then the new TUI cannot reach the old daemon.
Agents you stop this way come back with `Enter` on their card.

## 0.1.0-beta.3 - 2026-09-30

The follow-up box has room for what you want to say.

### Changed

- **A follow-up takes several lines** (`Space`). `Alt+Enter`, `Shift+Enter` or `C-j` starts a new line
  and `Enter` sends it all to the agent as one message.
- **Prompt boxes grow with the text.** The follow-up and the new task (`p`) boxes are 72 columns wide
  and 4 rows tall, grow to 10 rows as you type, and wrap long lines at a word instead of sliding them
  sideways.

### Updating

`termist update`, then quit termist (`q`) and start it again. The daemon did not change, so your
sessions keep running; no `termist kill` is needed.

## 0.1.0-beta.2 - 2026-09-28

termist's sound is now a real seagull, and it stays quiet until you ask for it.

### Changed

- **Sounds are off unless you turn them on** (`notify.sounds`, or `s`). A new install stays quiet; a
  `config.toml` that already names a sound keeps it.
- **One sound, a real martı.** The `istanbul` sound is now a recorded seagull, played both when an
  agent waits for you and when one is done. The ferry's horn is gone, and `termist sound test` takes no
  name.

### Updating

`termist update`, then, when your sessions can stop, `termist kill` and start `termist` again.
To hear the martı, set `sounds` to `istanbul` in the settings (`s`).

## 0.1.0-beta.1 - 2026-09-28

termist gets its look and its voice: themes, Istanbul scenes, sounds and notifications, settings
you can change from inside, and a way to update itself.

### New

- **Themes.** Üsküdar (dark, the default) and Moda (light) paint the whole screen, the agents' own
  colours included; the terminal theme keeps your terminal's colours. With 256 colours the themes are
  matched to the nearest ones; with 16, the terminal theme stands in.
- **Agents see the right colours.** An agent that asks its terminal for its colours is told the ones
  termist draws it with, so it picks a light look on Moda. With the terminal theme, termist asks your
  terminal first.
- **Settings (`s`).** Theme, colours, prefix, where the pane goes, and every key of the grid and of
  focus mode. Changes apply at once and are saved to `config.toml`, keeping your comments.
- **`config.toml`**, with `config.local.toml` over it. A mistake in it never stops termist: it is
  reported once and that setting keeps its default. `termist config path|check|export|import`.
- **Help (`?`)** lists every key as it is bound now.
- **The pane goes right of the cards** from 180 columns (`pane_position`); `C-a z` moves it for the
  rest of the run.
- **Istanbul scenes.** Galata, Kız Kulesi, Ayasofya, the Bosphorus Bridge, a vapur with its gulls and
  the Basilica Cistern, in the colours of the hour and gently moving: for a second at start (any key
  skips it), on an empty grid, after `idle_minutes` without a key (an agent that starts waiting ends
  it), and at the top of the help. Too small a terminal, or 16 colours, gets a one-line wordmark.
- **Sounds.** A ferry's horn when an agent waits for you, a seagull when one is done, made by code (no
  sound files, no audio library). `notify.sounds`: `istanbul`, `system`, `bell` or `off`.
  `termist sound test vapur|marti` plays one.
- **Desktop notifications** when the terminal is not in front, asked of the terminal itself (Ghostty,
  WezTerm, kitty, iTerm2 and others; through tmux too).
- **`termist update`** (also `termist upgrade`) installs the newest release, pre-releases included,
  through the release's own installer and into the same place; `--check` only looks. A termist that
  the installer did not put there (`cargo install`, a source build) is left alone.
- After the prefix, `n` and `t` start an agent or a shell, as in the grid.
- Inside tmux, termist says once that `C-a` is tmux's prefix too.

### Changed

- A key matches with exactly its modifiers: Ctrl+h, Ctrl+] or Ctrl+1 no longer do what h, ] or 1 do.
- The daemon speaks protocol 4: after upgrading, `termist kill` the old daemon once.

### Known limits

- The themes, scenes and sounds are new: tell us what looks or sounds wrong.
- Windows builds are provided but have had the least use; there, termist cannot yet ask the terminal
  for its colours.
- Text boxes and lists keep their keys; only the grid's and focus mode's can be rebound.

### Updating

From 0.1.0-alpha.1, run the install line again; from now on, `termist update`. Then, when your
sessions can stop, `termist kill` and start `termist` again.

macOS and Linux: `curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh`

Windows (PowerShell):
`powershell -ExecutionPolicy Bypass -c "irm https://eyupucmaz.github.io/termist/install.ps1 | iex"`

## 0.1.0-alpha.1 - 2026-09-27

The first public build of termist, mission control for your coding agents.

### What works

- **Three agents, one grid.** Claude Code, Codex and OpenCode run as cards in project tabs, each with a
  status dot driven by the agent's own hooks: running, waiting for you, done, closed or disconnected.
- **A daemon keeps them alive.** Close the TUI or the terminal and the agents keep working; `termist`
  attaches again. `termist kill` stops everything.
- **Resume after a reboot.** Sessions come back as `○ disconnected`; `Enter` resumes the same conversation.
- **Quick prompt (`p`)** with a launch line: project, agent, model and effort, and your prompt history.
- **Follow-up (`Space`)** sends the next instruction without entering the card; it refuses while the
  agent is waiting for an answer.
- **`.` and `,`** jump to the next session that needs you, across projects; **`/`** finds any session.
- **Projects** open from a folder browser (`o`) and close as tabs (`x`); cards scroll, rename (`r`) and
  archive (`a`, `A`).
- A Claude turn cancelled before its answer starts is still noticed, from the terminal title.

### Known limits

- Notifications, sounds, Istanbul scenes, themes and a settings screen are not there yet.
- Windows builds are provided but have had the least use.
- Keys cannot be rebound yet.

### Install

macOS and Linux: `curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh`

Windows (PowerShell):
`powershell -ExecutionPolicy Bypass -c "irm https://github.com/eyupucmaz/termist/releases/download/v0.1.0-alpha.1/termist-installer.ps1 | iex"`
