# termist

**terminal istanbul** — mission control for your coding agents.

Run Claude Code, Codex and OpenCode side by side, across every project, from one terminal tab.
Every session is a card with a status dot — running, waiting on you, or done — and a background
daemon keeps the agents alive when you close the TUI or your terminal. Written in Rust for macOS,
Linux and Windows, in Istanbul.

![termist: three agents at work, one waiting for an answer](docs/demo.gif)

> **Status: beta.** `v0.1.0-beta.1` adds themes, Istanbul scenes, sounds, notifications and settings
> to the first public build. It is used every day by its author, but expect rough edges.

## Install

**macOS and Linux**

```sh
curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh
```

**Windows** (PowerShell)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://eyupucmaz.github.io/termist/install.ps1 | iex"
```

Both put `termist` in `~/.cargo/bin`. Prebuilt archives for every platform are on the
[releases page](https://github.com/eyupucmaz/termist/releases). From source:
`cargo install --git https://github.com/eyupucmaz/termist termist`.

**Updating.** `termist update` installs the newest release where the installer put termist
(`termist update --check` only says whether there is one). Versions without it (0.1.0-alpha.1)
update by running the install line above again. Either way the daemon keeps running the old version,
with your sessions: when they can stop, run `termist kill`, then `termist` starts the new one.

termist runs the agent CLIs you already have: install at least one of
[Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex) or
[OpenCode](https://opencode.ai). It never writes into your repositories or your agents' own config.

## Use

Open a terminal in a project and run `termist`. The daemon starts on its own; the folder becomes a project tab.

| Key | Does |
|---|---|
| `p` | new task: type a prompt; `Tab` picks the agent, `^O` model and effort, `^P` project |
| `n` / `t` | new agent session / new shell |
| `Enter` | type into the selected card (`Ctrl+a Esc` back to the grid) |
| `Space` | send a follow-up to a card without entering it |
| `.` / `,` | next / previous session that needs you, across all projects |
| `/` | find any session |
| `o` / `x` | open a project / close its tab (sessions keep running) |
| `r` / `a` / `A` | rename / archive / show archived |
| `d` | stop a session (asks first) |
| `q` | quit the TUI; the agents keep running |

`termist kill` stops the daemon and every session. Sessions survive a restart: after a reboot they come
back as `○ disconnected`, and `Enter` resumes each one where it left off.

### Status dots

| | |
|---|---|
| `●` yellow | running |
| `◆` red | waiting for you (a permission or a question) |
| `✓` blue | done, not seen yet |
| `●` green | done, seen |
| `✗` magenta | exited with an error |
| `○` grey | not running; `Enter` resumes it |

Status comes from each agent's own hooks, passed on the command line or through termist's own config
directory — never through files in your project.

## Contributing

Issues and pull requests are welcome. Please read the [code of conduct](CODE_OF_CONDUCT.md) first.
The demo above is recorded with stand-in agents: `vhs assets/demo/demo.tape` after
`cargo build --release -p termist`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at
your option.
