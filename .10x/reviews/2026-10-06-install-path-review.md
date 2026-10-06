# Review: install-path (`wt/install-path`, over main `0230382`)

Reviewer: adversarial review and fix of the installer's PATH line, `UNDRA_MODIFY_PATH=1`, and the note about an
`undra` found first on PATH. Author's commits `dabd709` and `7a74426`; the review's fixes `d4d5be7` (installer) and
`868cb51` (harness), the record and this file after them. Nothing pushed.

Verdict: merge after the fixes below. The design is right (the printed `printf` line cannot join a last line; the opt-in
is validated before any download; the note comes before the hint), the POSIX sh is clean, and the harness is
byte-exact. Two defects were real and are fixed at the cause with checks that fail on the unfixed code.

## Findings

### Medium (fixed)

1. **The `--version` guard left a `sleep 10` behind and, under dash, said `Terminated`.** `site/install.sh:278` at
   `dabd709`: the guard was `(sleep 10; kill "$version_pid") &`, stopped with `kill "$guard_pid"`. A TERM to a subshell
   kills the subshell alone; its `sleep` child kept running for the rest of its 10 s on every install that found an older
   undra (seen: `pgrep -fx 'sleep 10'` = 1 right after the call, under sh, dash and bash). And when the guard did fire
   (an undra that hangs), dash, which is `sh` on Ubuntu, reported the child it reaped: `Terminated: 15` on the user's
   terminal, between the note and the PATH hint (`site/install.sh:281`, `wait "$version_pid"`). macOS `sh` (bash 3.2 in
   posix mode) and bash printed nothing.
   Fix (`d4d5be7`, `site/install.sh:274-297`): the guard subshell traps TERM, runs its sleep in the background and kills
   it from the trap; `wait`'s stderr is closed. Verified by hand under `set -eu` in all three shells (0 sleeps left,
   "version unknown" for a hung binary, nothing on stderr) and by harness section 12 (`packaging/test-install.sh`,
   "the --version guard"): a fake `sleep` first on PATH (the installer resolves `sleep` through PATH) records its pid, and
   an older undra that answers only once that pid is recorded shows the sleep is gone when the install ends; a second fake
   that returns at once fires the guard with no 10 s wait, so a hung undra (`exec /bin/sleep 600`) is "version unknown",
   is killed, and `Terminated`/`Killed` appear nowhere, under sh, dash and bash. No check waits 10 s.

2. **The duplicate detector took another directory for ours.** `site/install.sh:419` at `dabd709`: `index($0, name)`
   with no boundary, so a startup file holding `~/.undra/bin-old`, `$HOME/.undra/bin2`, `$HOME/.undra/binaries`,
   `/mnt/Users/x/.undra/bin` or `$HOME/.undra/bin/sub` on a PATH line made `UNDRA_MODIFY_PATH=1` write nothing and say
   the file "already puts ... on PATH". The brief's `bin-old` case was exactly this. Also `site/install.sh:416`: the line
   had to contain `PATH` or `fish_add_path`, so zsh's `path=(~/.undra/bin $path)` was not recognised and a second line was
   written (harmless, but the record claimed the idiom was out of scope rather than wrong).
   Fix (`d4d5be7`, `site/install.sh:404-435`): every occurrence of each spelling is checked for a whole name: the byte
   before is not `[A-Za-z0-9._~/-]`, the byte after is not `[A-Za-z0-9._-]`, a trailing `/` is allowed; `PATH` matches in
   any case. 24 spellings probed by hand under bash and dash (all right, including `${HOME}`, CRLF, a trailing comment,
   `path+=(...)`, `fish_add_path`, `MANPATH=...bin-old`). Harness 10b now writes the line past `bin-old`, `bin2` and
   `bin/sub`, and writes nothing past `path=(...)` and `bin/`.

### Low (open, recorded in the piece's record)

3. A custom `UNDRA_HOME` holding `"` or `$` goes into the double-quoted `export PATH="...:$PATH"` as it is (written and
   printed). A space, `'`, `%` and `\` are handled: the printed `printf` line was run through `eval` in bash and through
   `sh -c` for each and left the byte-exact file. Nobody has such a home; not fixed.
4. `ZDOTDIR` and `XDG_CONFIG_HOME` are not honoured (author's note; the old hint had the same assumption).
5. A line that reaches the directory through the user's own variable (`$UNDRA_HOME/bin`) is not recognised; a second
   line is appended, which only lists the directory twice.

### Checked and sound

* **Portability.** `sh -n`, `dash -n`, `bash -n` pass; the harness runs the installer under `/bin/sh`, `/bin/dash` and
  `/bin/bash`. No `[[`, `${x//}`, `echo -e`, `$'..'`, arrays or bare `read -r` in `site/install.sh` (the greps hit only
  `[[:space:]]` in regexes). `real_path` (`site/install.sh:298-316`) resolves relative links, absolute links to relative
  links, `..` in a chain, a directory symlink and `bin/../bin`, all to the same `/private/...` path on macOS, in all three
  shells; a loop stops after 40 hops; a missing file comes back unchanged. No `realpath`, `readlink -f` or `timeout`.
* **The written and printed lines.** A file without a final newline gets one first; empty and missing files get the one
  line (bash's per platform, fish's directories made); a last line that is a comment is kept whole; the detector skips
  `#` lines. `UNDRA_HOME` elsewhere is written in full (new harness 10g) and found by a second run. The opt-in is validated
  before `need curl` (`site/install.sh:39-44`; harness 10e checks "Installing" never appears) and writes nothing when
  `$bin_dir` is on PATH (10e, `expect_only_undra`).
* **The shadowing note.** `command -v` under `sh` cannot return a function or alias; a non-`/` answer returns anyway
  (`:245-248`). A non-executable file is not found by `command -v`. A symlink to the installed file, or `$bin_dir` first on
  PATH, prints nothing (harness 11). stderr-only or failing `--version` is "version unknown". The note precedes the hint
  (harness 11 checks the order). Text: terse, no em-dash, names `cargo uninstall undra-cli` only under `.cargo/bin` or
  `$CARGO_HOME/bin`, else `rm <path>`, then the PATH-order alternative.
* **The harness.** Mutations run against it, each caught by the check meant for it: the author's guard
  (`FAIL: the guard's sleep was left running`), `wait` with stderr open and the trap kept (`FAIL: under /bin/dash the
  guard's kill was reported to the user`), the detector without the boundary (`FAIL: .zshrc is not what was expected`
  on `bin-old`), the printed line without the leading `\n` (`FAIL: no zsh PATH line under /bin/sh`), `shadow_note` not
  called (`FAIL: an older undra first on PATH is not named`). No network (127.0.0.1 server), no fixed waits: the only
  timing is an upper bound (`gone` polls up to 1 s; the waiting undra up to 2 s). Cost here: 24 s before the review's
  sections, 29 s after.
* **CI.** `.github/workflows/ci.yml:80-83`: the Rust (Linux) job, repository root, right after `cargo build -p undra-cli`,
  so `target/debug/undra` exists; ubuntu-latest has python3, curl, tar, dash and `/bin/sleep`. `scripts/ci-local.sh --list`
  shows the step as `run`.
* **Docs and record.** The header comment and `--help` describe `UNDRA_MODIFY_PATH` and its files correctly;
  `docs/ONBOARDING.md`'s row matches the command and the `UNDRA_BIN` shortcut. Of the record's seven "seen to fail"
  mutations, two were reproduced here (the printed line without `\n`, no shadow note); the rest stand as the author's.
  The record's "about 20 s" is now "about 30 s"; its "not tested" guard and "`path+=` not recognised" bullets are
  replaced by the review's section.

## What was run

* `sh -n`, `dash -n`, `bash -n site/install.sh`; `grep -P '\x{2014}'` over every touched file (none).
* `cargo build -p undra-cli`; `UNDRA_BIN=target/debug/undra bash packaging/test-install.sh`: green before the fixes
  (28 checks, 24 s) and after (31 checks, 29 s).
* Hand probes in the scratchpad, each under `sh`, `dash` and `bash`: `version_of` before and after (orphan count,
  stderr, a hung and a fast binary, `set -eu`); `has_path_line` on 24 spellings; `printf_text` with a space, `'`, `%` and
  `\` in the path, the printed line run through `eval` and `sh -c` and the file read back with `od -c`; `real_path` on six
  link shapes, a loop and a missing path.
* Five installer mutations against the harness (above). `scripts/ci-local.sh --list`.
* Not run: shellcheck (not installed, nothing may be downloaded); `scripts/ci-local.sh` in full (the brief's scope is the
  installer and its harness; the touched CI step was run directly).

## Open

* Findings 3 to 5 above (low, recorded).
* The piece still needs CI green on its pushed head before `scripts/wt.sh merge` (not pushed by this review).
