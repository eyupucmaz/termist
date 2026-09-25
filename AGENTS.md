# termist — agent guide

**termist** ("terminal istanbul") is a cross-platform (macOS, Linux, Windows) Rust TUI: mission control for
coding agents (Claude Code, Codex, OpenCode). A background daemon owns every PTY; the TUI shows projects →
worktree bands → session cards with status dots, plus one focused terminal pane.

## Read this first

Product docs live in `.docs/` (local only — gitignored, never pushed). **Before any design or
implementation work, read `.docs/INDEX.md`** and the files it points to (PRD, decisions log). If `.docs/`
does not exist in your checkout, you are on a public clone: work from the code and this file.

- The PRD (`.docs/PRD.md`) is the source of truth for scope, keymap and milestones.
- Every product/architecture decision goes into `.docs/DECISIONS.md` (append, dated, with the why).
- Never copy `.docs/` content into committed files, commit messages or PR descriptions.

## Conventions

- Language: Rust (stable), Cargo workspace. UI strings are English and go through the string catalog.
- Every change must build on all three OSes. Platform-specific code lives behind a small trait/module
  (`pty`, `ipc`, `paths`, `notify`), never `cfg` scattered through features.
- No network services: daemon ↔ TUI ↔ hooks talk over a local socket (unix socket / Windows named pipe).
- Tests: unit tests next to code, TUI snapshot tests on ratatui's `TestBackend`, e2e tests drive a real
  daemon with stub agent scripts.
- Git: personal project on GitHub account `eyupucmaz`. Commit author `Eyup Ucmaz <eyupucmaz@gmail.com>`.
