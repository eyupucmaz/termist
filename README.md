# termist

**terminal istanbul** — mission control for your coding agents.

Run Claude Code, Codex and OpenCode across every project and git worktree from one terminal tab.
Every session is a card with a status dot — running, done, or waiting on you — and a background daemon
keeps them alive when you close the TUI. Written in Rust for macOS, Linux and Windows, in Istanbul.

> **Status:** early development — a first working build, no releases yet.

## Try it (development build)

```sh
cargo run --release -p termist          # TUI; starts the daemon on first run
cargo run -p termist -- kill            # stop the daemon and its sessions
```

`n` new Claude session · `t` shell · `Enter` type into it · `Ctrl+a Esc` back to the grid · `.` next session that needs you · `q` quit (sessions keep running).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at
your option.
