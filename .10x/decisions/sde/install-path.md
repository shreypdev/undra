# install-path: the installer's PATH line broke a .zshrc, and an older undra answered first

Branch `wt/install-path` (from main `0230382`). Installer and its test only: `site/install.sh`, `packaging/test-install.sh`,
one CI step, one row of `docs/ONBOARDING.md`. No wire, ABI or generated-shape change, so no ADR.

## What happened

A user installed 1.1.0 with `curl -fsSL https://shreypdev.github.io/undra/install.sh | sh` and hit two things:

1. The printed command was `echo 'export PATH="$HOME/.undra/bin:$PATH"' >> ~/.zshrc && exec zsh`. Their `.zshrc` did not end
   with a newline, so the export was appended to its last line and zsh failed to parse the file.
2. An old `~/.cargo/bin/undra` (0.1.0) came earlier on PATH and kept answering `undra --version` after the install. Nothing
   said so; finding out why took several minutes.

## The changes

1. **The printed line cannot break a file.** zsh and bash are now told
   `printf '\nexport PATH="$HOME/.undra/bin:$PATH"\n' >> ~/.zshrc && exec zsh` (bash: `~/.bashrc`, `~/.bash_profile` on
   macOS). The leading `\n` ends a last line that has no newline; the trailing one ends the file. A custom `UNDRA_HOME` is put
   into the format with `%` and `\` doubled and `'` escaped (`printf_text`), so an odd path cannot become a format directive.
   fish keeps `fish_add_path`; any other shell keeps the plain export line.
2. **`UNDRA_MODIFY_PATH=1` writes the line itself.** Validated first, before any download (`0` or `1`, anything else is
   refused). With `1`: the file is chosen by `$SHELL` as the hint does (zsh `~/.zshrc`; bash `~/.bashrc`, `~/.bash_profile` on
   Darwin; fish `~/.config/fish/conf.d/undra.fish` with `fish_add_path`, directories made). A file whose last byte is not a
   newline gets one first. A non-comment line that mentions `PATH` or `fish_add_path` and names the directory (in full, or as
   `$HOME/...`, `${HOME}/...` or `~/...`) counts as already there: nothing is written and the run says so. Otherwise exactly one
   line is appended and the run prints the line, the file and `exec <shell>`. An unknown shell (or no `HOME`) writes nothing and
   prints the export line. A directory already on PATH: nothing to do, and the run says no startup file changed. Unset or `0`:
   print only, as before. Documented in the header comment and `--help`.
3. **A shadowing `undra` is named.** After the install, `command -v undra` is resolved; if it is a file whose real path
   (symlinks followed by `real_path`, since POSIX has no `realpath`) is not the installed file's, a note before the PATH line
   gives its path and the first line of its `--version` (stdin `/dev/null`, killed after 10 s; a failure or no output is
   "version unknown"), and the fix: `cargo uninstall undra-cli` when it is under `.cargo/bin` (or `$CARGO_HOME/bin`), else
   `rm <path>`, or put `$bin_dir` before its directory on PATH. An `undra` that is the installed file, or that comes after it,
   prints nothing.

## How each is tested

`packaging/test-install.sh` (the existing harness: a release served from 127.0.0.1, a fresh `HOME` per run) gained sections 10
and 11, and its runs now use a PATH without directories that hold an `undra` (a developer's own would otherwise be named in
every run). Byte-for-byte checks with `cmp` against the expected file:

* (1) every install under sh, dash and bash prints the exact zsh `printf` line; the bash line names the platform's file; the
  printed line, extracted from the output and run (without the `exec`) by `sh` and by `bash` on a `.zshrc` without a final
  newline, leaves `alias ll="ls -l"\nexport PATH=...\n`.
* (2) a `.zshrc` without a final newline ends up as its old line, the export on its own line, a final newline; a second run
  into the same `HOME` writes nothing and says so; a line spelt with `~` or in full counts, a commented one does not; a
  missing `~/.bashrc` / `~/.bash_profile` is created holding the one line and nothing else; fish's `conf.d/undra.fish` holds
  `fish_add_path "$HOME/.undra/bin"`; tcsh writes nothing and prints the export; already on PATH writes nothing;
  `UNDRA_MODIFY_PATH=yes` is refused before "Installing".
* (3) a stale `undra` (prints `undra 0.1.0`, then a second line) first on PATH is named with `(undra 0.1.0)` only, `rm` as the
  fix, before the PATH line; one in `<home>/.cargo/bin` gets `cargo uninstall undra-cli` and "put ... before ..."; one after
  the installed directory, or a symlink to the installed file first on PATH, prints nothing; one whose `--version` exits 3 is
  "version unknown".

Each new check was seen to fail against a mutated installer (no leading newline, no duplicate check, comments counted, no
shadow note, the note after the hint, paths compared without resolving symlinks, the printed line without `\n`). Nothing
depends on the network or on timing: the fakes answer at once, the 10 s guard is never reached.

The harness ran only in the release workflow; CI's Rust job now runs it after `cargo build -p undra-cli`
(`UNDRA_BIN=target/debug/undra bash packaging/test-install.sh`, about 30 s on a laptop with the review's sections), so a
change to the installer is checked on its pull request.

## The review's fixes (`.10x/reviews/2026-10-06-install-path-review.md`)

* The guard's subshell, killed with TERM once the version was in, left its `sleep 10` running for the rest of the 10 s; and
  under dash (Ubuntu's `sh`) `wait` printed `Terminated` when the guard killed a hung `undra`. The subshell now traps TERM and
  kills its own sleep; `wait`'s stderr is closed. Section 12 of the harness puts a fake `sleep` first on PATH (the installer
  finds `sleep` there): one that records its pid and sleeps for real shows the sleep is gone once the version is in; one that
  returns at once fires the guard without the 10 s wait, so a hung `undra` (`exec /bin/sleep 600`) is "version unknown", is
  killed, and no shell prints `Terminated`. Nothing waits 10 s.
* `has_path_line` matched the directory's name inside a longer one: `~/.undra/bin-old`, `$HOME/.undra/bin2` and
  `$HOME/.undra/bin/sub` counted as the line and nothing was written. The name must now be whole (a trailing slash allowed),
  and `PATH` is matched in any case, so zsh's `path=(~/.undra/bin $path)` and `path+=(...)` count too. Section 10b checks
  both ways; 10g checks a custom `UNDRA_HOME` is written in full, once.

## Not covered

* `shellcheck` is not installed on this machine and nothing could be downloaded, so it was not run; the script was written to
  the existing `# shellcheck disable=` conventions and passes `sh -n`, `dash -n` and `bash -n`. CI does not run shellcheck.
* `ZDOTDIR` and `XDG_CONFIG_HOME` are not honoured: the files are the ones the brief names (`~/.zshrc`,
  `~/.config/fish/...`), as the printed hint already assumed.
* A line that puts the directory on PATH through a variable of the user's (`export PATH="$UNDRA_HOME/bin:$PATH"`) is not
  recognised and a second line is written; it is harmless (the directory twice on PATH).
* A custom `UNDRA_HOME` holding `"` or `$` is written into the double-quoted line as it is; a space, `'`, `%` or `\` is fine
  (checked by hand under sh and bash).
* No page under `site/docs/` documents the installer's environment variables (`UNDRA_HOME` appears in none), so no site page
  changed; `--help` documents `UNDRA_MODIFY_PATH`.
