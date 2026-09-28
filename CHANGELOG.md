# Changelog

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
