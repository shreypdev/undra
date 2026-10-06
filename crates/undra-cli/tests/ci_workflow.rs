//! The CI workflow `undra init` writes (`.github/workflows/undra.yml`): it parses, it names the jobs
//! of the core and of every app, it pins the Undra release once at the top, and the core the project
//! starts with passes the checks the core job runs.
//!
//! The YAML is read by the small parser below (the subset GitHub workflows use: block mappings and
//! sequences, plain and quoted scalars, flow sequences, `|` and `>` block scalars); `ruby -ryaml`
//! and `actionlint` check it as well when they are installed.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use common::{TempDir, flag, has_tool, init_project, run_ok, undra};

/// A parsed YAML value.
#[derive(Debug, Clone, PartialEq)]
enum Yaml {
    Str(String),
    Seq(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> &str {
        match self {
            Yaml::Str(s) => s,
            other => panic!("not a string: {other:?}"),
        }
    }

    fn seq(&self) -> &[Yaml] {
        match self {
            Yaml::Seq(items) => items,
            other => panic!("not a sequence: {other:?}"),
        }
    }

    fn keys(&self) -> Vec<&str> {
        match self {
            Yaml::Map(entries) => entries.iter().map(|(k, _)| k.as_str()).collect(),
            other => panic!("not a mapping: {other:?}"),
        }
    }
}

struct Line<'a> {
    indent: usize,
    text: &'a str,
}

fn parse_yaml(text: &str) -> Yaml {
    let lines: Vec<Line> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| Line {
            indent: l.len() - l.trim_start().len(),
            text: l.trim_start(),
        })
        .collect();
    // Block scalars need the blank lines and comment-looking lines of their content: parse from the raw text.
    let raw: Vec<&str> = text.lines().collect();
    let mut at = 0;
    let value = block(&raw, &lines, &mut at, 0);
    assert_eq!(
        at,
        lines.len(),
        "unparsed lines from {}",
        lines.get(at).map_or("", |l| l.text)
    );
    value
}

fn strip_comment(s: &str) -> &str {
    let mut quote: Option<char> = None;
    let bytes: Vec<char> = s.chars().collect();
    let mut offset = 0;
    for (i, c) in bytes.iter().enumerate() {
        match (quote, *c) {
            (None, '"' | '\'') => quote = Some(*c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if i == 0 || bytes[i - 1] == ' ' => return s[..offset].trim_end(),
            _ => {}
        }
        offset += c.len_utf8();
    }
    s.trim_end()
}

fn scalar(s: &str) -> String {
    let s = strip_comment(s).trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    s.to_owned()
}

/// A value written on the same line as its key: a scalar or a flow sequence.
fn inline(s: &str) -> Yaml {
    let s = strip_comment(s).trim();
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return Yaml::Seq(
            inner
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| Yaml::Str(scalar(p)))
                .collect(),
        );
    }
    Yaml::Str(scalar(s))
}

fn split_key(text: &str) -> Option<(&str, &str)> {
    // `key: value` or `key:`; the colon ends a key that has no quote or space before it.
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '{' | '[') => depth += 1,
            (None, '}' | ']') => depth -= 1,
            (None, ':') if depth == 0 => {
                let rest = &text[i + 1..];
                if rest.is_empty() || rest.starts_with(' ') {
                    return Some((
                        text[..i].trim().trim_matches(['"', '\'']),
                        rest.trim_start(),
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// The raw lines of a block scalar that starts after `lines[*at]`: everything more indented than `parent`.
fn block_scalar(
    raw: &[&str],
    lines: &[Line],
    at: &mut usize,
    parent: usize,
    folded: bool,
) -> String {
    // Find where the line `lines[*at - 1]` is in `raw`, by counting the non-blank, non-comment lines.
    let mut seen = 0;
    let mut pos = 0;
    for (i, l) in raw.iter().enumerate() {
        if !l.trim().is_empty() && !l.trim_start().starts_with('#') {
            seen += 1;
            if seen == *at {
                pos = i + 1;
                break;
            }
        }
    }
    let mut content = Vec::new();
    let mut consumed_lines = 0;
    let mut i = pos;
    while i < raw.len() {
        let l = raw[i];
        let indent = l.len() - l.trim_start().len();
        if !l.trim().is_empty() && indent <= parent {
            break;
        }
        content.push(l);
        if !l.trim().is_empty() {
            consumed_lines += 1;
        }
        i += 1;
    }
    *at += consumed_lines;
    let base = content
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let _ = lines;
    let body: Vec<String> = content
        .iter()
        .map(|l| {
            if l.len() >= base {
                l[base..].to_owned()
            } else {
                String::new()
            }
        })
        .collect();
    let joined = if folded {
        body.join(" ")
    } else {
        body.join("\n")
    };
    joined.trim_end().to_owned()
}

fn block(raw: &[&str], lines: &[Line], at: &mut usize, indent: usize) -> Yaml {
    let first = &lines[*at];
    if first.text.starts_with("- ") || first.text == "-" {
        let mut items = Vec::new();
        while *at < lines.len()
            && lines[*at].indent == indent
            && (lines[*at].text.starts_with("- ") || lines[*at].text == "-")
        {
            let rest = lines[*at].text[1..].trim_start().to_owned();
            let item_indent = indent + 2;
            *at += 1;
            if rest.is_empty() {
                items.push(block(raw, lines, at, lines[*at].indent));
            } else if let Some((key, value)) = split_key(&rest) {
                // A mapping whose first key shares the line with the dash.
                let mut entries = vec![(
                    key.to_owned(),
                    entry_value(raw, lines, at, item_indent, value),
                )];
                while *at < lines.len()
                    && lines[*at].indent == item_indent
                    && !lines[*at].text.starts_with("- ")
                {
                    let (k, v) = split_key(lines[*at].text).expect("a key").to_owned_pair();
                    *at += 1;
                    entries.push((k, entry_value(raw, lines, at, item_indent, &v)));
                }
                items.push(Yaml::Map(entries));
            } else {
                items.push(inline(&rest));
            }
        }
        return Yaml::Seq(items);
    }
    let mut entries = Vec::new();
    while *at < lines.len() && lines[*at].indent == indent && !lines[*at].text.starts_with("- ") {
        let (k, v) = split_key(lines[*at].text)
            .unwrap_or_else(|| panic!("not a `key: value` line: {}", lines[*at].text))
            .to_owned_pair();
        *at += 1;
        entries.push((k, entry_value(raw, lines, at, indent, &v)));
    }
    Yaml::Map(entries)
}

trait OwnedPair {
    fn to_owned_pair(self) -> (String, String);
}
impl OwnedPair for (&str, &str) {
    fn to_owned_pair(self) -> (String, String) {
        (self.0.to_owned(), self.1.to_owned())
    }
}

/// The value of a key at `indent` whose text after the colon is `rest`.
fn entry_value(raw: &[&str], lines: &[Line], at: &mut usize, indent: usize, rest: &str) -> Yaml {
    let rest = strip_comment(rest).trim();
    if matches!(rest, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
        return Yaml::Str(block_scalar(raw, lines, at, indent, rest.starts_with('>')));
    }
    if rest.is_empty() {
        // A nested block, a sequence at the same indent (`key:` then `- item`), or an empty value.
        if *at < lines.len() && lines[*at].indent > indent {
            return block(raw, lines, at, lines[*at].indent);
        }
        if *at < lines.len() && lines[*at].indent == indent && lines[*at].text.starts_with("- ") {
            return block(raw, lines, at, indent);
        }
        return Yaml::Str(String::new());
    }
    inline(rest)
}

fn workflow_of(project: &Path) -> (String, Yaml) {
    let text = std::fs::read_to_string(project.join(".github/workflows/undra.yml"))
        .expect("`undra init` wrote .github/workflows/undra.yml");
    let yaml = parse_yaml(&text);
    (text, yaml)
}

/// A project made without `--undra-path`, as a user makes one (the temporary directory is not inside a checkout).
fn init_released(name: &str, platforms: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new(name);
    run_ok(
        undra()
            .args(["init", name, "--platforms", platforms, "--dir"])
            .arg(dir.path()),
    );
    let root = dir.path().join(name);
    (dir, root)
}

#[test]
fn the_parser_reads_what_it_must() {
    let yaml = parse_yaml(
        "name: X\nenv:\n  A: \"1.2.3\"\non:\n  push:\n    branches: [main]\njobs:\n  one:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - name: Two\n        run: |\n          echo a\n          echo \"# not a comment\"\n      - name: Three\n        with:\n          targets: a, b\n        run: >\n          x\n          y\n",
    );
    assert_eq!(yaml.get("name").unwrap().str(), "X");
    assert_eq!(yaml.get("env").unwrap().get("A").unwrap().str(), "1.2.3");
    assert_eq!(
        yaml.get("on")
            .unwrap()
            .get("push")
            .unwrap()
            .get("branches")
            .unwrap()
            .seq(),
        [Yaml::Str("main".into())]
    );
    let steps = yaml
        .get("jobs")
        .unwrap()
        .get("one")
        .unwrap()
        .get("steps")
        .unwrap()
        .seq();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0].get("uses").unwrap().str(), "actions/checkout@v4");
    assert_eq!(
        steps[1].get("run").unwrap().str(),
        "echo a\necho \"# not a comment\""
    );
    assert_eq!(
        steps[2].get("with").unwrap().get("targets").unwrap().str(),
        "a, b"
    );
    assert_eq!(steps[2].get("run").unwrap().str(), "x y");
}

#[test]
fn the_workflow_parses_and_names_every_job() {
    let (_dir, root) = init_released("ciall", "ios,android,web");
    let (text, yaml) = workflow_of(&root);
    assert_eq!(yaml.get("name").unwrap().str(), "Undra");
    let jobs = yaml.get("jobs").expect("jobs");
    assert_eq!(jobs.keys(), ["core", "ios", "android", "web"], "{text}");
    let runs_on: BTreeMap<&str, &str> = jobs
        .keys()
        .into_iter()
        .map(|j| (j, jobs.get(j).unwrap().get("runs-on").unwrap().str()))
        .collect();
    assert_eq!(runs_on["core"], "ubuntu-latest");
    assert_eq!(runs_on["web"], "ubuntu-latest");
    assert_eq!(runs_on["android"], "ubuntu-latest");
    assert_eq!(runs_on["ios"], "macos-14");
    // Every step is a `uses` or a `run`; every job installs undra the way docs/RELEASING.md says.
    for job in jobs.keys() {
        let steps = jobs.get(job).unwrap().get("steps").unwrap().seq();
        assert!(steps.len() >= 4, "{job}");
        for step in steps {
            assert!(
                step.get("uses").is_some() || step.get("run").is_some(),
                "{job}: {step:?}"
            );
        }
        let install = steps
            .iter()
            .find(|s| s.get("run").is_some_and(|r| r.str().contains("install.sh")))
            .unwrap_or_else(|| panic!("{job} does not install undra"));
        assert!(
            install
                .get("run")
                .unwrap()
                .str()
                .contains(
                    "curl -fsSL \"https://raw.githubusercontent.com/shreypdev/undra/v${UNDRA_VERSION}/site/install.sh\" | sh"
                ),
            "the installer of the pinned release, from its tag: {install:?}"
        );
        assert!(
            install.get("run").unwrap().str().contains("$GITHUB_PATH"),
            "{install:?}"
        );
    }
    let runs = |job: &str| -> String {
        jobs.get(job)
            .unwrap()
            .get("steps")
            .unwrap()
            .seq()
            .iter()
            .filter_map(|s| s.get("run").map(|r| r.str().to_owned()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // The core: fmt, clippy -D warnings, test, bindgen --check.
    let core = runs("core");
    for needle in [
        "cargo fmt --all --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
        "undra bindgen --check",
    ] {
        assert!(core.contains(needle), "core lacks {needle}:\n{core}");
    }
    // The web app: Node 24, npm ci, test, build.
    let web = jobs.get("web").unwrap().get("steps").unwrap().seq();
    let node = web
        .iter()
        .find(|s| {
            s.get("uses")
                .is_some_and(|u| u.str().starts_with("actions/setup-node"))
        })
        .expect("setup-node");
    assert_eq!(
        node.get("with").unwrap().get("node-version").unwrap().str(),
        "24"
    );
    let web_runs = runs("web");
    for needle in ["npm ci", "npm test --if-present", "npm run build"] {
        assert!(web_runs.contains(needle), "web lacks {needle}:\n{web_runs}");
    }
    // Android: assembleDebug, with NDK r27.
    let android = runs("android");
    assert!(
        android.contains("ndk;27.2.12479018")
            && android.contains("ANDROID_NDK_HOME")
            && !android.contains("cargo-ndk"),
        "{android}"
    );
    assert!(android.contains("./gradlew assembleDebug"), "{android}");
    // iOS: xcodebuild.
    let ios = runs("ios");
    assert!(
        ios.starts_with("curl")
            && ios.contains(
                "xcodebuild -project ios/Ciall.xcodeproj -scheme Ciall -sdk iphonesimulator"
            ),
        "{ios}"
    );
}

#[test]
fn the_workflow_holds_no_secret_and_reads_only() {
    // It runs on pull requests, forks' included: read-only token, no secret, no pull_request_target.
    let (_dir, root) = init_released("cisafe", "ios,android,web");
    let (text, yaml) = workflow_of(&root);
    assert_eq!(
        yaml.get("permissions")
            .unwrap()
            .get("contents")
            .unwrap()
            .str(),
        "read"
    );
    assert_eq!(yaml.get("permissions").unwrap().keys(), ["contents"]);
    for needle in [
        "secrets.",
        "pull_request_target",
        "GITHUB_TOKEN",
        "write-all",
    ] {
        assert!(!text.contains(needle), "{needle} in:\n{text}");
    }
    // Nothing is piped to a shell from a moving URL: the installer is the pinned release's.
    for line in text
        .lines()
        .filter(|l| l.contains("| sh") || l.contains("| bash"))
    {
        assert!(line.contains("v${UNDRA_VERSION}/"), "{line}");
    }
}

#[test]
fn the_undra_version_is_pinned_once_at_the_top() {
    let (_dir, root) = init_released("cipin", "web");
    let (text, yaml) = workflow_of(&root);
    let env = yaml.get("env").unwrap();
    assert_eq!(
        env.get("UNDRA_VERSION").unwrap().str(),
        env!("CARGO_PKG_VERSION")
    );
    // It is the first thing after the name, before the triggers and the jobs.
    let keys = yaml.keys();
    assert_eq!(&keys[..2], ["name", "env"], "{keys:?}");
    // The install step does not name a version of its own: it reads the variable the install script reads.
    assert!(!text.contains("UNDRA_VERSION="), "{text}");
    assert!(
        text.lines()
            .filter(|l| l.contains(&format!("\"{}\"", env!("CARGO_PKG_VERSION"))))
            .count()
            == 1,
        "{text}"
    );
}

#[test]
fn only_the_apps_of_the_project_get_a_job() {
    let (_dir, root) = init_released("ciweb", "web");
    let (_, yaml) = workflow_of(&root);
    assert_eq!(yaml.get("jobs").unwrap().keys(), ["core", "web"]);
    let (_dir, root) = init_released("ciandroid", "android,ios");
    let (_, yaml) = workflow_of(&root);
    assert_eq!(yaml.get("jobs").unwrap().keys(), ["core", "ios", "android"]);
}

#[test]
fn a_project_that_uses_a_local_checkout_has_no_workflow() {
    // CI cannot see the checkout a path dependency points at.
    let project = init_project("cipath", "web");
    assert!(!project.root.join(".github").exists());
    let readme = std::fs::read_to_string(project.root.join("README.md")).unwrap();
    assert!(!readme.contains(".github/"), "{readme}");
    let (_dir, root) = init_released("cirel", "web");
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(
        readme.contains(".github/       the CI workflow (undra.yml)"),
        "{readme}"
    );
}

#[test]
fn ruby_and_actionlint_accept_it_when_they_are_installed() {
    let (_dir, root) = init_released("cilint", "ios,android,web");
    let file = root.join(".github/workflows/undra.yml");
    if has_tool("ruby", "--version") {
        let out = Command::new("ruby")
            .args(["-ryaml", "-e", "doc = YAML.safe_load(File.read(ARGV[0]), aliases: false); abort('no jobs') unless doc['jobs'].keys == %w[core ios android web]"])
            .arg(&file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ruby's YAML parser rejects it: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        eprintln!("skipped the ruby check: ruby is not installed");
    }
    if has_tool("actionlint", "-version") {
        let out = Command::new("actionlint")
            .arg(&file)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "actionlint: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        eprintln!("skipped the actionlint check: actionlint is not installed");
    }
}

#[test]
fn the_core_a_project_starts_with_passes_the_formatting_check_of_the_core_job() {
    let (_dir, root) = init_released("cifmt", "web");
    if !has_tool("rustfmt", "--version") {
        assert!(
            !flag("UNDRA_REQUIRE_TOOLCHAINS"),
            "rustfmt is not installed (UNDRA_REQUIRE_TOOLCHAINS=1)"
        );
        eprintln!("skipped: rustfmt is not installed");
        return;
    }
    // `cargo fmt --all --check` formats the core with the default style and the edition of its Cargo.toml.
    let out = Command::new("rustfmt")
        .args(["--edition", "2024", "--check", "--config-path"])
        .arg(root.join("no-rustfmt.toml"))
        .arg(root.join("core/src/lib.rs"))
        .current_dir(&root)
        .output()
        .unwrap();
    // (rustfmt wants the config path to exist: the default style is what an absent file means.)
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("config") {
        let out = Command::new("rustfmt")
            .args(["--edition", "2024", "--check"])
            .arg(root.join("core/src/lib.rs"))
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
