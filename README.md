<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/readme/banner-dark.png">
  <img src="assets/readme/banner-light.png" alt="termist, terminal istanbul: mission control for your coding agents. The Galata Tower drawn in the terminal, with a vapur on the Golden Horn.">
</picture>

<h1>termist</h1>

<p><b>Run Claude Code, Codex and OpenCode side by side, across every project, from one terminal tab.</b></p>

<p>
  <a href="https://eyupucmaz.github.io/termist/"><img src="https://img.shields.io/badge/website-eyupucmaz.github.io%2Ftermist-40c2bd?style=for-the-badge&labelColor=13263b" alt="Website"></a>
  <a href="#install"><img src="https://img.shields.io/badge/install-one%20line-f08a4b?style=for-the-badge&labelColor=13263b" alt="Install"></a>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/changelog-0.2.3-7fb2ff?style=for-the-badge&labelColor=13263b" alt="Changelog"></a>
</p>

<p>
  <a href="https://github.com/eyupucmaz/termist/releases"><img src="https://img.shields.io/github/v/release/eyupucmaz/termist?include_prereleases&label=release&color=40c2bd" alt="Latest release"></a>
  <a href="https://github.com/eyupucmaz/termist/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/eyupucmaz/termist/ci.yml?branch=main&label=ci" alt="CI"></a>
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-2a5aa8" alt="macOS, Linux and Windows">
  <img src="https://img.shields.io/badge/written%20in-Rust-b7410e?logo=rust&logoColor=white" alt="Written in Rust">
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20or%20Apache--2.0-4d6075" alt="License: MIT or Apache-2.0"></a>
</p>

<img src="docs/demo.gif" alt="termist in a terminal: the Galata Tower at dusk for a moment, then three agents as cards, one running, one waiting for an answer, one done; the user jumps to the waiting one, answers it, starts a new task and finds a session by name.">

</div>

> [!NOTE]
> **termist 0.2.3 is out**: tools next to your agents. `O` opens the card's folder in your editor (a
> terminal one as a card), `L` lazygit, `f` and `F` find a file or a line, and `PgUp` is copy mode with
> `v`, `y` and `/`. With 0.2.2's `g` (what an agent changed, file by file), 0.2.1's worktrees of their own
> (`Ctrl+N`, `W`, `X`) and 0.2.0's pull requests next to your agents (`v`, `w`, `a`). With 0.1.4's writing (comments,
> reviews), 0.1.3's diff and mouse, 0.1.2's pull requests (`v`), and 0.1.x's toasts, themes, scenes, sounds,
> settings and scrolling back. It is used every day by its author; please
> [tell us](https://github.com/eyupucmaz/termist/issues) what looks or feels wrong.

## Why termist

- **Agents outlive the window.** A small daemon owns every session. Close the TUI or the terminal and the
  agents keep working; `termist` attaches again.
- **One dot says it all.** Every session is a card with a status dot, driven by the agent's own hooks:
  running, waiting for you, or done.
- **Jump to the one that needs you.** `.` goes to the next agent waiting for an answer, across every
  project.
- **Start work without leaving the grid.** `p` opens a prompt: pick the agent, model and project, and
  it starts as a new card. `Space` sends a follow-up to a finished one.
- **Toasts and a status line.** A toast in the top right says when an agent waits for you or is done,
  even in another project; click it to go to the card. The status line shows cpu, ram, battery and the
  time, and each part can be turned off in the settings.
- **Nothing lost on a reboot.** Sessions come back as `○ disconnected`; `Enter` resumes the same
  conversation.
- **Your repositories stay yours.** termist runs the agent CLIs you already have and never writes into
  your projects or your agents' own config.

## Its look and its voice

<img src="docs/look.gif" alt="The settings screen open over three agent cards. The theme changes from Üsküdar, dark, to Moda, light, to the terminal's own colours and back to Moda, and the whole screen follows at once. Then the help opens with the Galata Tower at dusk above the list of keys.">

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="docs/scene.png"><img src="docs/scene.png" alt="The Galata Tower drawn in the terminal at dusk."></a>
      <p><b>Istanbul scenes.</b> Galata, Kız Kulesi, Ayasofya, the Bosphorus Bridge, a vapur and the
      Basilica Cistern, in the colours of the hour: at start, on an empty grid and when you have been
      away a while.</p>
    </td>
    <td width="50%" valign="top">
      <a href="docs/settings.png"><img src="docs/settings.png" alt="The settings screen over the grid."></a>
      <p><b>Settings, from inside (<code>s</code>).</b> Theme, colours, prefix, where the pane goes, and
      every key. Changes apply at once and are saved to <code>config.toml</code>, keeping your
      comments.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <a href="docs/moda.png"><img src="docs/moda.png" alt="The grid in the light Moda theme."></a>
      <p><b>Themes.</b> Üsküdar (dark, the default), Moda (light), seven Istanbul neighbourhoods (Aksaray,
      Kadıköy, Beşiktaş, Balat, Kapalıçarşı, Adalar, Bebek), Catppuccin Mocha and Latte, Tokyo Night,
      Gruvbox Dark, Nord, Dracula, or your terminal's own colours. Your own themes go in the
      <code>themes/</code> folder next to <code>termist config path</code>, as <code>&lt;name&gt;.toml</code>;
      any colour you leave out comes from Üsküdar. Agents are told the colours termist draws them with,
      so they pick a light look on a light theme.</p>
    </td>
    <td width="50%" valign="top">
      <a href="docs/help.png"><img src="docs/help.png" alt="The help, listing the keys of the grid."></a>
      <p><b>Help (<code>?</code>).</b> Every key as it is bound now. Keys of the grid and of focus mode
      can be rebound.</p>
    </td>
  </tr>
</table>

Also new: sounds and desktop notifications when an agent waits for you or is done (Ghostty, WezTerm,
kitty, iTerm2 and others, through tmux too), the pane to the right of the cards from 180 columns, and
`termist update`. The [changelog](CHANGELOG.md) has everything.

## Install

**macOS and Linux**

```sh
curl -LsSf https://eyupucmaz.github.io/termist/install.sh | sh
```

**Windows** (PowerShell)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://eyupucmaz.github.io/termist/install.ps1 | iex"
```

Then open a terminal in a project and run `termist`. The daemon starts on its own, and the folder
becomes a project tab.

termist runs the agent CLIs you already have: install at least one of
[Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex) or
[OpenCode](https://opencode.ai).

<details>
<summary><b>Other ways to install, and updating</b></summary>

<br>

Both installers put `termist` in `~/.cargo/bin`. Prebuilt archives for every platform are on the
[releases page](https://github.com/eyupucmaz/termist/releases). From source:
`cargo install --git https://github.com/eyupucmaz/termist termist`.

`termist update` installs the newest release where the installer put termist
(`termist update --check` only says whether there is one). Versions without it (0.1.0-alpha.1)
update by running the install line above again. Either way the daemon keeps running the old version,
with your sessions: when they can stop, run `termist kill`, then `termist` starts the new one.

</details>

## Use

| Key | Does |
|---|---|
| `p` | new task: type a prompt; `Tab` picks the agent, `^O` model and effort, from the list each CLI offers, `^P` project |
| `Enter` | type into the selected card (`Ctrl+a Esc` back to the grid) |
| `Space` | send a follow-up to a card without entering it |
| wheel, `PgUp` | scroll back through a session's output (`Ctrl+a [` while typing into it) |
| drag | select in the pane; letting go copies it, and a toast in the top right says so |
| `.` / `,` | next / previous session that needs you, across all projects |
| `/` | find any session |
| `v` | the project's open pull requests and their diffs (needs the GitHub CLI) |
| click | a project tab, a card (again: type into it), the pane, a file, a thread |
| `s` / `?` | settings / every key as it is bound now |
| `q` | quit the TUI; the agents keep running |

<details>
<summary><b>All keys</b></summary>

<br>

| Key | Does |
|---|---|
| `p` | new task: type a prompt; `Tab` picks the agent, `^O` model and effort, from the list each CLI offers, `^P` project, `^T` where it starts, `^N` a new worktree |
| `P` | new task like the selected card: its agent, model and worktree |
| `e` | your presets: one opens the new task with its agent, model, effort and words (`Ctrl+S` in the new task saves one) |
| `n` / `t` | new agent session / new shell, in the selected card's worktree |
| `W` / `X` | the worktrees: show, hide, remove / remove the selected one (asks; the branch stays) |
| `g` | what the selected card's branch changed, file by file (`Ctrl+a g` while typing into it) |
| `O` / `L` | the card's folder in your editor / lazygit there, as a card |
| `f` / `F` | find a file / the lines with some text (`git grep`); `Enter` opens it in your editor |
| `Enter` | type into the selected card (`Ctrl+a Esc` back to the grid) |
| `Space` | send a follow-up to a card without entering it |
| `.` / `,` | next / previous session that needs you, across all projects |
| `/` | find any session |
| wheel | scroll back through a session's output |
| `PgUp` | copy mode (`Ctrl+a [` while typing into it): `hjkl` `w` `b` move, `v` / `V` select, `y` copy, `/` `?` search, `q` back to live |
| drag | select in the pane; letting go copies it to the clipboard, and a toast in the top right says so |
| `v` / `R` | the project's pull requests (on the selected card's, if it has one) / read GitHub again |
| `V` | the selected card's pull request in the browser |
| `d` | in a pull request: its files and their diff |
| `w` | in a pull request: a worktree on its branch, and an agent there |
| `Space` / `a` | in a pull request: mark review threads / hand them to an agent |
| click | a project tab, a card (a second click types into it), the pane (types into it), a file, a thread |
| wheel | moves what is under it: the cards, a session's history, a list, the diff |
| `o` / `x` | open a project / close its tab (sessions keep running) |
| `r` / `a` / `A` | rename / archive / show archived |
| `d` | stop a session (asks first) |
| `s` | settings: theme, colours, prefix, pane (right, left, bottom, top), keys |
| `?` | help: every key as it is bound now |
| `q` | quit the TUI; the agents keep running |

`termist kill` stops the daemon and every session.

</details>

### Worktrees

Agents that work in parallel are better off in worktrees of their own. In the new-task prompt `Ctrl+N`
makes one: a branch named from the prompt (`fix the login redirect` → `fix-the-login-redirect`) off the
repo's default branch, beside the repo at `<repo>-worktrees/<branch>`, and the agent starts in it.
`Ctrl+T` picks where a task starts: the project's folder, any worktree, or a new one. Cards stand in
bands, one per worktree, under the branch, what it changed since it left its base
(`3 files +60 −28`, `●` while some of it is not committed) and its pull request; a band whose pull
request merged says so. `X` removes a worktree once its cards are stopped (it asks, and asks again if
it has uncommitted changes; the branch stays). `W` lists every worktree of the project's repos, those
made outside termist too, to show one as a band or hide it.

```toml
[agents]
new_worktree_by_default = false   # the new-task prompt starts with Ctrl+N on
```

### Presets

A setup you start often (an agent, its model and effort, and words you put around the task) is a preset.
In the new-task prompt `Ctrl+S` saves the one on screen: the words before the cursor go first, the ones
after it last. `e` lists them; `Enter` opens the new task with one, the cursor where the task goes; `r`
renames one, `d` deletes it. They are kept in config.toml:

```toml
[[presets]]
name = "review"
harness = "claude"
model = "opus"
effort = "high"
prefix = "Review this change carefully: "
postfix = "\nThen list what to fix, by file."
```

A card started with a task is named after it (`Fix Login Redirect` rather than `codex-4`); with a preset,
after what you typed.

### Tools

`O` opens the card's folder in your editor, and a file found with `f` (a file of the repo) or `F` (the
lines with some text, through `git grep`) at its line. The editor is `editor` in config.toml, else
`$VISUAL` or `$EDITOR`, else the first of `code`, `cursor`, `zed` found. One that runs in a terminal
(`nvim`, `vim`, `hx`, `emacs`…) opens as a card you type into, gone when you quit it; `L` opens lazygit
the same way. `PgUp` (`Ctrl+a [` while typing into a card) is copy mode: a cursor over the session's
history moved as in vi, `v` or `V` to select, `y` to copy, `/` and `?` to search all of it.

```toml
editor = "nvim"   # or "code", "zed --wait", …
```

`g` shows what the selected card's branch changed: the files as a tree beside one file's changes, as
the diff of a pull request is shown, from where the branch left its base to what is on disk now, the
uncommitted too. `u` shows only what is not committed. It follows the agent: what changes on disk is
shown a moment later, where you were. `Ctrl+r` marks a file reviewed and moves to the next; a file
that changes after that is marked `↺` and wants another look. The band says how much of its branch
was reviewed (`2/3 reviewed`, `reviewed ✓`), and the marks stay when termist restarts.

### Pull requests

`v` shows the open pull requests of the project: one repo, or every GitHub repo one level below a folder
(`m` chooses which). Enter opens one: its description, the conversation with line comments, checks and
files. termist reads GitHub through the [GitHub CLI](https://cli.github.com): install `gh` and run
`gh auth login`. With several accounts logged in, each repo is read with the account that has the most
access to it; `m`, then `a`, picks another.

`d`, or Enter on a file, opens the diff: the files as a tree beside one file's changes, with the line
comments under their lines. `J` / `K` go from file to file, `s` shows old and new side by side, and
`Ctrl+r` marks a file viewed on GitHub (as the checkbox there does) and goes to the next one.

And you can write back. `c` comments: on the pull request, or in the diff on the line under the cursor
(`v` chooses several lines, `Ctrl+s` turns them into a suggestion). Line comments wait in your review
until `A` sends it with a comment, an approval or a change request. On a thread `r` replies and `x`
resolves it; `e` and `D` edit and delete your own comments. Esc keeps what you wrote for later.

And you can work on it. `w` opens a worktree on the pull request's branch beside the repo
(`<repo>-worktrees/<branch>`, made with `gh pr checkout`, or the one that already has the branch) and the
new-task prompt for an agent there. To act on review comments, mark threads with `Space` and press `a`:
they go, with the lines they are about, to an agent already on that branch as a follow-up, or to a new
one in its worktree. Cards stand in bands, one per worktree, under the branch and its pull request;
`Shift+V` opens a card's pull request in the browser.

```toml
[github]
enabled = true

[diff]
layout = "unified"   # or "split"
```

<details>
<summary><b>Status dots</b></summary>

<br>

| | |
|---|---|
| `●` yellow | running |
| `◆` red | waiting for you (a permission or a question) |
| `✓` blue | done, not seen yet |
| `●` green | done, seen |
| `✗` magenta | exited with an error |
| `○` grey | not running; `Enter` resumes it |

Status comes from each agent's own hooks, passed on the command line or through termist's own config
directory, never through files in your project.

</details>

## Contributing

Issues and pull requests are welcome. Please read the [code of conduct](CODE_OF_CONDUCT.md) first.

The demos are recorded with stand-in agents: `vhs assets/demo/demo.tape` after
`cargo build --release -p termist`, `vhs assets/demo/look.tape` for the themes, settings and help, and
`vhs assets/demo/prs.tape` for the pull requests, with a stand-in `gh` and made-up repos.
The banner is drawn from the Galata scene by `python3 tools/build-banner.py`.

## Credits

Five themes adapt the palettes of other projects, all under the MIT licence: [Catppuccin](https://github.com/catppuccin/catppuccin)
(Mocha and Latte), [Tokyo Night](https://github.com/folke/tokyonight.nvim), [gruvbox](https://github.com/morhetz/gruvbox),
[Nord](https://github.com/nordtheme/nord) and [Dracula](https://github.com/dracula/dracula-theme). A few colours are
deepened so text stays readable; each theme file says where its colours come from.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at
your option.

<div align="center">
<sub>Written in Rust for macOS, Linux and Windows, in Istanbul.</sub>
</div>
