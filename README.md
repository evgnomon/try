# try

A bash-only Rust port of try-ruby: a fuzzy selector for dated scratch
directories under `~/src/github.com/$USER/tries` (or `$TRY_PATH`).

With mise, from the prebuilt binaries on GitHub releases:

```bash
mise use -g github:evgnomon/try
```

Or from source:

```bash
make install            # static musl binary on Linux, to ~/.local/bin; PREFIX=/usr/local for system-wide
```

`make install` also installs `~/.bashrc.d/try.sh`, which runs
`eval "$(try init)"` when `try` is on `PATH`. With mise, or if your `~/.bashrc`
doesn't source `~/.bashrc.d/*.sh`, add:

```bash
echo 'eval "$(try init)"' >> ~/.bashrc
```

```
try [query]            Interactive selector, optionally pre-filtered
try <git-url> [name]   Clone into a dated directory (GitHub PR URLs too)
try . <name>           New dated directory; a detached worktree in a git repo
try ./path [name]      The same, from another directory
try init [path]        Print the bash function
```

Keys: `↑/↓` or `Ctrl-P/N` move, `Enter` selects or creates, `Ctrl-R` renames,
`Ctrl-G` graduates a try to `$TRY_PROJECTS` (default: the parent of
`TRY_PATH`) and leaves a symlink, `Ctrl-D` marks for deletion (confirm by
typing `YES`), `Esc` cancels. `Ctrl-A/E/B/F/K/U/W` edit the search line.

The binary prints a bash script on stdout and draws the UI on stderr; the
function from `try init` evals the script, so `cd` happens in your shell.
