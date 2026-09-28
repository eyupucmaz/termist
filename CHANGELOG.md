# Changelog

## Unreleased

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
- After the prefix, `n` and `t` start an agent or a shell, as in the grid.
- Inside tmux, termist says once that `C-a` is tmux's prefix too.

### Changed

- A key matches with exactly its modifiers: Ctrl+h, Ctrl+] or Ctrl+1 no longer do what h, ] or 1 do.
- The daemon speaks protocol 4: after upgrading, `termist kill` the old daemon once.

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
