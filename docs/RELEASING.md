# Releasing Undra

One tag, one workflow, one repository (ADR-063). Everything an Undra app needs comes from
`github.com/shreypdev/undra` at a release tag `v<version>`; no registry account exists on either side.

| What | What users run or write | Where it comes from |
|---|---|---|
| CLI, installer (first) | `curl -fsSL https://shreypdev.github.io/undra/install.sh \| sh` | `site/install.sh`, which downloads the Release's tarball and checks its sha256 |
| CLI, cargo (second) | `cargo install --locked --git https://github.com/shreypdev/undra --tag v1.0.0 undra-cli` | the repository |
| Rust crates | `undra = { git = "https://github.com/shreypdev/undra", tag = "v1.0.0" }` | the repository at the tag |
| Swift runtime | `.package(url: "https://github.com/shreypdev/undra", from: "1.0.0")` | `Package.swift` at the repository's root, at the tag |
| Kotlin runtime | `com.github.shreypdev.undra:runtime:v1.0.0` with `https://jitpack.io` for that group | JitPack, which builds the tag (`jitpack.yml`) the first time it is asked |
| npm runtimes | `"@undra/runtime": "https://github.com/shreypdev/undra/releases/download/v1.0.0/undra-runtime-1.0.0.tgz"` | the GitHub Release's assets (also `undra-react-native-*.tgz`, `undra-testkit-*.tgz`) |

`undra init` writes exactly those lines for the release it belongs to; `undra upgrade` moves them. The workflow
(`.github/workflows/release.yml`) builds the CLI for macOS and Linux on x86_64 and arm64, packs the three npm
packages, tests both installs on the real artifacts, and on a version tag publishes the GitHub Release. Maven
Central, the npm registry and Homebrew (through homebrew-core, `brew install undra`) are later and additive
(ADR-063 and its amendment). Windows and Alpine (musl) are not supported (roadmap).

**The release needs no secret.** The workflow uses the `GITHUB_TOKEN` GitHub gives every run, and nothing else: no
tap, no registry account, no token to create, store or renew. There is no Homebrew tap at launch (ADR-063,
amendment of 2026-10-03).

## The launch checklist

In order. Each step is one command or one click, and says how to see that it worked. Run the commands from a
clone of `shreypdev/undra` on `main`, with `gh` signed in as the repository's owner.

### 1. Check the repository

```sh
gh repo view shreypdev/undra --json visibility,defaultBranchRef   # PUBLIC, main
curl -fsSI https://shreypdev.github.io/undra/install.sh | head -n 1  # HTTP/2 200 (GitHub Pages, site.yml)
```

The repository must be public: SwiftPM, JitPack, Cargo and the installer read it without credentials.

### 2. Harden the repository (recommended, once)

There is no secret to set: the release publishes with the `GITHUB_TOKEN` of its own run, and nothing is published to
npm or Homebrew. Verify: `gh secret list --repo shreypdev/undra` prints nothing. (A registry or tap token left from an
earlier plan has no use: `gh secret delete <name> --repo shreypdev/undra`.)

Create the environment `release` (Settings, Environments) and restrict *Deployment branches and tags* to the tag
pattern `v*`, so only a version tag can run the publish job; add a tag ruleset (Settings, Rules, Rulesets: tags
matching `v*`, restrict creation, update and deletion, bypass: you). The workflow already refuses a `v*` tag whose
commit is not on `main`.

Recommended before announcing: reserve the npm scope `@undra` (npmjs.com, *Add Organization*, the free plan for
public packages; nothing is published). Nothing of Undra's is on the registry, but `@undra/react-native`,
`@undra/testkit` and every generated bindings package name `@undra/runtime` by a range, and npm installs a missing
peer from the registry: an app that installs one of them without the runtime's URL, or a `npm install` inside
`generated/ts`, would fetch whatever someone else published as `@undra/runtime`. Verify:
`npm view @undra/runtime` still answers 404 and `npm org ls undra` lists you.

### 3. Rehearse with a release candidate (required)

A prerelease exercises every channel that only a real tag can (SwiftPM against github.com, JitPack's build, the
Release's assets, the installer) before `v1.0.0` exists, and announces nothing. It is not optional: JitPack's build of the
tag (its Android SDK, its group and version) cannot be shown any other way, and Android apps of a release whose JitPack
build fails cannot build. Do not go on to step 4 until steps 7 and 8 pass for the candidate.

```sh
scripts/bump-version.sh 1.0.0-rc.1         # step 4 says what it sets
git switch -c release/1.0.0-rc.1 && git commit -qam "chore(release): 1.0.0-rc.1"
git push -u origin release/1.0.0-rc.1 && gh pr create --fill     # the "All green" check (the Gate workflow), then squash-merge
git switch main && git pull && git tag v1.0.0-rc.1 && git push origin v1.0.0-rc.1
gh run watch
```

What it publishes: a GitHub **prerelease** `v1.0.0-rc.1` with the four CLI tarballs, the three npm tarballs and
`checksums.txt`; nothing else. The installer's "latest" ignores prereleases, no post, no registry. Then run
steps 7 and 8 with `1.0.0-rc.1` for `1.0.0` (install the CLI with
`curl -fsSL https://shreypdev.github.io/undra/install.sh | UNDRA_VERSION=1.0.0-rc.1 sh`): a project made by
that CLI pins `v1.0.0-rc.1` everywhere. A project of `1.0.0` never resolves to a prerelease tag: SwiftPM's
`from: "1.0.0"` ignores `v1.0.1-rc.1` and `v1.1.0-rc.1` (checked with a local tagged clone), and Cargo, JitPack and
the npm URLs name one tag each. A failure here costs a `1.0.0-rc.2`, not a broken `1.0.0`. The
prerelease and its tag can stay; nothing points at them.

The same rehearsal without any account runs on one machine: `bash packaging/rehearse-launch.sh` (a copy of the
repository as the remote, the assets on a local web server, JitPack's command into a local Maven repository; the
summary says per platform whether the app built without the checkout), or the workflow *Launch rehearsal* on
GitHub (`gh workflow run launch-rehearsal.yml`).

### 4. Set the version

```sh
scripts/bump-version.sh 1.0.0
```

It sets the workspace and `Cargo.lock`, the three npm packages and their locks, every `"@undra/runtime"` range and
the runtimes' `Hello` versions, and lists the files. It also files the migration notes kept under a version that was
never released (no tag `v<old>`: the first entry of `crates/undra-cli/src/migrations.rs`, "Since v1.0", kept under
`0.1.0`) under the new one, so `undra upgrade` prints them to the projects of a `0.1.0` CLI; after step 3 they stay
under `1.0.0-rc.1`, which a `0.1.0` project crosses too. Verify: `bash scripts/bump-version.sh --check 1.0.0` says
`every version file says 1.0.0`. In a fresh clone run `cargo fetch` first (the script refreshes `Cargo.lock` offline).

### 5. Open and merge the version pull request

```sh
git switch -c release/1.0.0 && git commit -qam "chore(release): 1.0.0"
git push -u origin release/1.0.0 && gh pr create --fill
gh pr checks --watch                # "All green" (the Gate workflow)
gh pr merge --squash
```

Verify: `git switch main && git pull && bash scripts/bump-version.sh --check` says `1.0.0`.

### 6. Push the tag

```sh
git tag v1.0.0 && git push origin v1.0.0
gh run watch                        # Release: verify, four builds, package, publish
```

### 7. What the workflow publishes, and warming JitPack

The run, in order: **verify** (the tag is `v` plus the workspace version, every version file agrees, the commit is
on `main`); **build** the CLI for four targets (`undra --version` says `undra 1.0.0 (<sha>)`); **package**:
`checksums.txt`, the three npm tarballs, an install of them by URL with no registry, the curl installer against the
tarballs; **publish**: the GitHub Release `v1.0.0` (generated notes, the seven assets and `checksums.txt`). Nothing
else is published: the crates and the Swift package are the tag itself.

```sh
gh release view v1.0.0              # undra-v1.0.0-<4 targets>.tar.gz, undra-{runtime,react-native,testkit}-1.0.0.tgz, checksums.txt
```

JitPack builds the Kotlin modules the first time anyone asks for them. Ask yourself, before announcing:

```sh
curl -fsS -o /dev/null -w '%{http_code}\n' https://jitpack.io/com/github/shreypdev/undra/runtime/v1.0.0/runtime-v1.0.0.pom
# The first request starts the build and may time out: repeat it until it prints 200 (a few minutes). Then the log:
curl -fsS https://jitpack.io/com/github/shreypdev/undra/v1.0.0/build.log | tail -n 20
```

The log ends with `Published: runtime testkit android-adapters android-work undra-compose okhttp-adapters
(com.github.shreypdev.undra, v1.0.0)` (`runtimes/kotlin/undra-runtime/scripts/jitpack-install.sh`) and JitPack's
list of build artifacts. The same is at https://jitpack.io/#shreypdev/undra, the tag's row and its log icon. Check
one Android module too: the same `curl` for `android-adapters/v1.0.0/android-adapters-v1.0.0.pom` prints 200.

JitPack builds and serves these bytes; nothing signs them. Record what it serves, so a later change shows, and add
it to the release notes (`gh release edit v1.0.0 --notes-file ...`, appended to the generated notes):

```sh
for m in runtime testkit android-adapters android-work undra-compose okhttp-adapters; do
  ext=aar; case $m in runtime | testkit) ext=jar ;; esac
  printf '%s  %s\n' "$(curl -fsSL "https://jitpack.io/com/github/shreypdev/undra/$m/v1.0.0/$m-v1.0.0.$ext" | shasum -a 256 | cut -d' ' -f1)" "$m-v1.0.0.$ext"
done
```

An app that wants to hold JitPack to those bytes commits Gradle's dependency verification
(`./gradlew --write-verification-metadata sha256 :app:assembleDebug`, `gradle/verification-metadata.xml`): a changed
artifact then fails its build (ADR-063, section 4).

### 8. Check each channel from a clean machine

The same walk runs on clean GitHub runners, a Mac and a Linux machine, as the `Release smoke` workflow
(`.github/workflows/release-smoke.yml`): it starts by itself when the Release workflow finishes for a tag, and by hand
with `gh workflow run release-smoke.yml -f version=1.0.0 && gh run watch`. Green there is this step done; the
commands below are the same walk on your own machine.

Use clean locations; the version is the tag's and the commit its first seven characters.

```sh
export UNDRA_HOME="$(mktemp -d)"
curl -fsSL https://shreypdev.github.io/undra/install.sh | sh
"$UNDRA_HOME/bin/undra" --version                     # undra 1.0.0 (<sha>)
unset UNDRA_HOME
cargo install --locked --git https://github.com/shreypdev/undra --tag v1.0.0 undra-cli    # says (unknown) for the sha

cd "$(mktemp -d)" && undra init smoke && cd smoke
grep 'undra = ' core/Cargo.toml                       # tag = "v1.0.0"
(cd web && npm install && npm run build)              # @undra/runtime from the Release's asset
xcodebuild -project ios/Smoke.xcodeproj -scheme Smoke -destination 'generic/platform=iOS Simulator' build
(cd android && ./gradlew :app:assembleDebug)          # com.github.shreypdev.undra:*:v1.0.0 from JitPack
undra bindgen --check                                 # the bindings init wrote are current
```

The iOS build resolves the Swift package `undra` 1.0.0 from github.com (Xcode's package list shows it); the first
resolution clones the repository (about 47 MiB).

### 9. Announce

Only now: the post, the links. Everything a reader runs exists.

## When a channel fails

A tag is immutable and projects pin it: never move or delete one. Fix forward with `v1.0.1` (steps 4 to 8 with
`1.0.1`), and meanwhile steer users away:

* **The workflow stopped half-way** (a network error, a GitHub outage): every publish step is idempotent. Re-run
  the failed job (`gh run rerun <run-id> --failed`) or `gh workflow run release.yml --ref v1.0.0 -f publish=true`.
  An existing Release must hold the same `checksums.txt`, else the run fails: cut a patch.
* **A broken GitHub Release**: `gh release edit v1.0.0 --prerelease` stops the installer's "latest" from choosing it
  (`gh release edit <previous tag> --latest` pins the previous one). Do not delete it: projects pin its assets.
* **JitPack's build failed**: read the log (step 7). A failure on JitPack's side (a timeout, a missing SDK
  component) can be retried from https://jitpack.io/#shreypdev/undra (the tag's row); a failure in the repository is
  a patch release. Android apps of `v1.0.0` cannot build until a tag builds on JitPack.
* **The Swift package or the crates**: a broken tag is a patch release; `undra upgrade` moves projects to it.
* **cargo**: nothing to roll back; users pin `--tag`.

## The dry run

Run it on any branch, any time (GitHub offers manual runs of a workflow only once its file is on the default
branch); it builds, packages and tests everything and publishes nothing:

```sh
gh workflow run release.yml --ref <branch> -f publish=false
gh run watch                       # pick the run
gh run download <run-id>           # the workflow artifacts, to look at them
```

A green dry run shows that the four binaries build and report `undra <version> (<sha>)`; `checksums.txt` verifies;
the three npm tarballs install by URL from a local web server with no registry, every peer satisfied;
`site/install.sh` downloads, verifies and installs the CLI's tarball from a local web server. The job summaries
list the checksums and the glibc version the Linux binaries need. What only a real tag shows: that
`gh release create` succeeds, that JitPack builds the tag, and that SwiftPM resolves the repository from
github.com: steps 3, 7 and 8.

## Maintenance

* **Linux binaries need glibc at least as new as the runner's.** The matrix builds on `ubuntu-22.04` and
  `ubuntu-22.04-arm` on purpose, so the floor is glibc 2.35 (Ubuntu 22.04, Debian 12, RHEL 9 and derivatives); the
  build job's summary prints the exact floor. When GitHub retires the 22.04 images, moving the two Linux `os:` values
  to 24.04 raises the floor to 2.39; keeping 2.35 then means building in a 22.04 container or with `cargo zigbuild`.
* **macOS binaries are not notarised.** A binary fetched by `curl` carries no quarantine flag and runs; one
  downloaded in a browser needs `xattr -d com.apple.quarantine`.
* **Action pins.** Every third-party action in `release.yml` is a commit SHA with its version in a comment. To update
  one: `gh api repos/<owner>/<name>/git/ref/tags/<tag> --jq .object.sha` (for an annotated tag, follow it to the
  commit), edit the SHA and the comment, run the dry run.
* **Homebrew, later** (ADR-063, amendment of 2026-10-03): there is no tap. `brew install undra` comes through
  homebrew-core, which needs the repository to be 30 days old and notable (75 stars, or 30 forks or watchers). The
  formula is then a pull request to `Homebrew/homebrew-core`, not a step of this workflow. At that point
  `undra upgrade` gets its Homebrew branch back (`Channel` in `crates/undra-cli/src/commands/upgrade.rs`: a binary
  in a Homebrew cellar names `brew upgrade undra`) and the install blocks of the site and README gain the command.
* **Moving to the registries** (ADR-063): Maven Central is the `MAVEN` constant of `crates/undra-cli/src/dist.rs`
  (`dev.undra`, no version prefix, no extra repository) plus a publishing job; the npm registry is the npm packages
  published as well as attached. `undra upgrade` moves existing projects either way.
* **Tests for the pieces**: `bash scripts/bump-version.test.sh`, `bash packaging/test-install.sh` (builds the CLI, or
  `UNDRA_BIN`), `bash packaging/pack-npm.sh --out <dir> && bash packaging/test-npm-assets.sh --release-dir <dir>`,
  and the whole thing: `bash packaging/rehearse-launch.sh`.
