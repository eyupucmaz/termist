# Changelog

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
