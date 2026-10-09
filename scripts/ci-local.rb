#!/usr/bin/env ruby
# frozen_string_literal: true

# The engine behind scripts/ci-local.sh: runs the steps of the GitHub workflows (CI, Bench, Two cores, Site) on this
# machine, from the workflow files themselves, so what runs here cannot drift from what runs there.
#
# For every job it reads `.github/workflows/<name>.yml`, checks the tools its `uses:` steps would have provisioned
# (the pinned Rust and its targets, Node, the JDK), and runs each `run:` step with the workflow's `env:`, the step's
# `working-directory`, and `bash --noprofile --norc -eo pipefail`, as GitHub does. A step that cannot run here (apt,
# sudo, a hosted-runner image, an emulator) is listed with its reason in SKIPS below and in the summary: nothing is
# skipped silently, and `--list` shows every step's disposition without running anything.
#
#   ci-local.rb --root DIR [--workflows ci,bench,site,two-cores] [--only ci/rust,ci/ts] [--skip ci/android]
#                          [--slow [--rounds 3] [--burners 8]] [--list] [-v] [--logs DIR]
#
# `--slow` is the slow-runner pass: only the steps in SLOW_STEPS (the timing-sensitive test suites: the Swift, Kotlin and
# TypeScript runtime tests, the contract grid's runners, the real-time recipe) and the Rust tests in SLOW_EXTRA (the workspace's
# tests but those that drive a compiler, bench/tests, the dev-reload tests) run, each with CPU burners (`yes`, at normal priority, `--burners`, default 8) occupying the cores for the length of the
# step, RUST_TEST_THREADS=4 and CARGO_BUILD_JOBS=4 (a hosted runner has four vCPUs), repeated `--rounds` times. The
# workflow's `cargo test --workspace` is not run as such: the tests of the three packages that drive a compiler are a build, and
# starved they tell nothing about timing (SLOW_EXTRA).
# Nothing here runs under `taskpolicy -b`: that class of QoS together with burners at normal priority leaves the test with no CPU
# at all (a step that would take a minute does not finish). It reuses what a normal pass built and installed in the same clone
# (run that first), so what is slowed is the tests and not the compiler. A test that fails only there depends on the speed of
# the machine: fix it at its cause.
#
# Every `cargo test` of a workflow step runs with `--no-fail-fast` (SUBSTITUTIONS), so one pass lists every failing test
# binary and not only the first. The burners cannot outlive ci-local (see `start_burners`).

require "fileutils"
require "json"
require "open3"
require "optparse"
require "rbconfig"
require "shellwords"
require "tmpdir"
require "yaml"

Encoding.default_external = Encoding::UTF_8
Encoding.default_internal = Encoding::UTF_8

module CiLocal
  WORKFLOWS = %w[ci bench two-cores site].freeze

  # Jobs that do not run here unless asked for, by "workflow/job", and why.
  JOB_SKIPS = {
    "two-cores/android" => "needs the Android emulator (KVM on the runner); the emulator CI jobs are on hold until the founder says go",
    "site/deploy" => "publishes to GitHub Pages (main only, needs the Pages environment)",
    "ci/ffi-asan" => "Rust under ASan runs on the Linux x86_64 target; macOS's linker rejects an ASan build of crates that use `inventory` " \
                     "(ld: initializer pointer has no target). The C ABI under ASan runs in ci/rust and ci/macos, Miri in ci/ffi-miri"
  }.freeze

  # Steps that cannot run on this machine, by "workflow/job" (a regexp) and the step's name: the reason is printed.
  # They are runner provisioning (what scripts/env.sh and a developer's machine have already), never a test.
  SKIPS = [
    [%r{\Aci/(kotlin|contracts)\z}, /\AInstall kotlinc\z/, "provisioning: scripts/env.sh puts kotlinc on PATH"],
    [%r{\Aci/(kotlin|contracts)\z}, /\AFetch kotlinx-coroutines\z/, "provisioning: scripts/env.sh sets UNDRA_KOTLINX_COROUTINES and UNDRA_SQLITE_JDBC"],
    [%r{\Atwo-cores/jvm-and-node\z}, /\AInstall kotlinc and kotlinx-coroutines\z/, "provisioning: scripts/env.sh puts kotlinc and the jars on PATH"],
    [%r{\Aci/react-native\z}, /\Aclang 18 and its sanitizer runtime\z/, "provisioning: apt and sysctl on the runner; macOS's clang has the sanitizers (CXX falls back to clang++)"],
    [%r{\Atwo-cores/ios(-release)?\z}, /\AThe newest stable Xcode\z/, "provisioning: sudo xcode-select; this machine's Xcode is used (DEVELOPER_DIR from scripts/env.sh)"],
    [%r{\Asite/build\z}, /\AInstall binaryen\z/, "provisioning: apt on the runner (checked: wasm-opt is on PATH)"],
    [%r{\Abench/size\z}, /\AInstall binaryen version_133\z/, "provisioning: the Linux tarball of binaryen version_133 (the Size gate checks wasm-opt reports 133)"],
    [%r{\Aci/bazel-(example(-macos)?(-minimums)?|android)\z}, /\AInstall Bazelisk\z/, "provisioning: `brew install bazelisk` once (checked: bazel is on PATH); the job's own pin is .bazelversion"],
    [%r{\Aci/bazel-example(-minimums)?\z}, /\Allvm-symbolizer for the symbols test\z/, "provisioning: apt on the runner; on macOS the symbols test reads the dSYM with atos"]
  ].freeze

  # Lines of a step's script that are runner provisioning and are dropped (the rest of the step runs). Accepting the
  # Android SDK licences and installing SDK packages are changes to the machine, so they are not done from here.
  STRIP_LINES = [
    [/sdkmanager/, "Android SDK provisioning (sdkmanager): packages and licences are installed by hand once, docs/ONBOARDING.md"]
  ].freeze

  # Text of a step's script rewritten for the host: [pattern, replacement, why].
  SUBSTITUTIONS = [
    [/\bcargo test\b(?![^\n]*--no-fail-fast)/, "cargo test --no-fail-fast",
     "a failing test binary must not hide the ones after it: one pass lists every failure (CI stops at the first, and costs a push per failure)"]
  ].freeze

  # The timing-sensitive test steps: what `--slow` runs. "workflow/job" => step-name patterns.
  SLOW_STEPS = {
    "ci/rust" => [/\AThe real-time recipe/],
    "ci/ts" => [/\ATest\z/, /\ATesting kit/, /\ADevtools page/],
    "ci/kotlin" => [/\ATest \(incl\. JNI smoke/],
    "ci/wasm-ffi" => [/\Awasm acceptance/],
    "ci/macos" => [/\ASwift tests\z/, /\ASwift-over-C-ABI/],
    "ci/contracts" => [/\ARun the scenario runners\z/],
    "ci/contracts-swift" => [/\ARun the Swift scenario runner\z/],
    "ci/react-native" => [/\AUnit tests/, /\AContract scenarios through NativeTransport/],
    "two-cores/jvm-and-node" => [/\AJVM\z/, /\ANode\z/]
  }.freeze

  # The Rust tests `--slow` runs in place of the workflow's `cargo test --workspace`: "workflow/job" => [name, command]. They run
  # in the job's environment after its provisioning, as steps of their own (they are not in the workflow, whose
  # `cargo test --workspace --exclude undra-cli` runs them with everything else). What is left out is the three packages whose
  # tests drive a compiler (undra-bindgen's goldens and typechecks, undra-macros' trybuild cases, undra-cli's `undra build`s):
  # starved, those are a build, and they assert nothing about time. Every other package's tests run, because many read the clock
  # (undra-ffi, undra-runtime, undra-query and undra-transport bound waits and deadlines); bench/tests run as a step of ci/rust's
  # and the dev-server tests of undra-cli as a step of ci/rust-cli's.
  SLOW_EXTRA = {
    "ci/rust" => [
      ["the workspace's tests but those that drive a compiler (undra-bindgen, undra-macros, undra-cli)",
       "cargo test --no-fail-fast --workspace --exclude undra-bindgen --exclude undra-macros --exclude undra-cli --exclude undra-bench"],
      ["bench/tests: the stress scenarios, their fault-injection tests and the budgets (debug)", "cargo test --no-fail-fast -p undra-bench --tests"]
    ],
    "ci/rust-cli" => [
      ["undra-cli: the dev-server and dev-reload tests", "cargo test --no-fail-fast -p undra-cli --test dev --test dev_reload --test dev_devtools"]
    ]
  }.freeze

  # A tiny evaluator for the `${{ }}` expressions the workflows use: dotted context lookups, strings, `&& || ! == !=`,
  # parentheses, and startsWith / contains / format / always / success / failure. GitHub's truthiness: "" and 0 are false.
  class Expr
    TOKEN = /\s*(&&|\|\||==|!=|!|\(|\)|,|'(?:[^']|'')*'|[A-Za-z_][\w.\-]*|\d+(?:\.\d+)?)/

    def initialize(lookup)
      @lookup = lookup
    end

    def evaluate(text)
      @tokens = []
      rest = text.strip
      until rest.empty?
        m = TOKEN.match(rest)
        raise ArgumentError, "cannot parse `#{text}` at `#{rest}`" unless m && m.begin(0).zero?

        @tokens << m[1]
        rest = rest[m.end(0)..].strip
      end
      @pos = 0
      value = parse_or
      raise ArgumentError, "trailing tokens in `#{text}`" unless @pos == @tokens.size

      value
    end

    def self.truthy?(value)
      !(value.nil? || value == false || value == 0 || value == "")
    end

    private

    def peek
      @tokens[@pos]
    end

    def take
      @pos += 1
      @tokens[@pos - 1]
    end

    def parse_or
      value = parse_and
      while peek == "||"
        take
        other = parse_and
        value = Expr.truthy?(value) ? value : other
      end
      value
    end

    def parse_and
      value = parse_eq
      while peek == "&&"
        take
        other = parse_eq
        value = Expr.truthy?(value) ? other : value
      end
      value
    end

    def parse_eq
      value = parse_unary
      while %w[== !=].include?(peek)
        op = take
        other = parse_unary
        same = value.to_s.casecmp?(other.to_s)
        value = op == "==" ? same : !same
      end
      value
    end

    def parse_unary
      return !Expr.truthy?((take && parse_unary)) if peek == "!"

      parse_primary
    end

    def parse_primary
      token = take
      raise ArgumentError, "unexpected end of expression" if token.nil?

      if token == "("
        value = parse_or
        take
        return value
      end
      return token[1..-2].gsub("''", "'") if token.start_with?("'")
      return token.include?(".") ? token.to_f : token.to_i if token.match?(/\A\d/)

      if peek == "("
        take
        args = []
        until peek == ")"
          args << parse_or
          take if peek == ","
        end
        take
        return call(token, args)
      end
      @lookup.call(token)
    end

    def call(name, args)
      case name
      when "always", "success" then true
      when "failure", "cancelled" then false
      when "startsWith" then args[0].to_s.downcase.start_with?(args[1].to_s.downcase)
      when "endsWith" then args[0].to_s.downcase.end_with?(args[1].to_s.downcase)
      when "contains" then args[0].to_s.downcase.include?(args[1].to_s.downcase)
      when "format" then args[0].to_s.gsub(/\{(\d+)\}/) { args[Regexp.last_match(1).to_i + 1].to_s }
      else raise ArgumentError, "unsupported function #{name}()"
      end
    end
  end

  Result = Struct.new(:workflow, :job, :name, :status, :note, :seconds, :skipped, keyword_init: true)

  class Runner
    attr_reader :results

    def initialize(opts)
      @opts = opts
      @root = File.realpath(opts[:root])
      @results = []
      @burners = []
      @outputs = Hash.new { |h, k| h[k] = {} }
      @tmp = Dir.mktmpdir("ci-local-")
      @logs = opts[:logs] || File.join(@tmp, "logs")
      FileUtils.mkdir_p(@logs)
      @branch = opts[:branch] || sh_out("git", "rev-parse", "--abbrev-ref", "HEAD", chdir: @root).strip
      @branch = "wt/ci-local" if @branch == "HEAD" || @branch.empty?
      @sha = sh_out("git", "rev-parse", "HEAD", chdir: @root).strip
    end

    # ---- selection ----------------------------------------------------------------------------------------------

    def jobs
      list = []
      @opts[:workflows].each do |wf|
        file = File.join(@root, ".github", "workflows", "#{wf}.yml")
        next unless File.file?(file)

        yaml = YAML.safe_load(File.read(file), aliases: true)
        (yaml["jobs"] || {}).each do |id, job|
          list << { wf: wf, id: id, key: "#{wf}/#{id}", job: job, wf_env: yaml["env"] || {} }
        end
      end
      list.select { |j| selected?(j[:key], j[:id]) && (!@opts[:slow] || SLOW_STEPS.key?(j[:key]) || SLOW_EXTRA.key?(j[:key])) }
    end

    def selected?(key, id)
      only = @opts[:only]
      skip = @opts[:skip]
      return false if skip.any? { |s| s == key || s == id }

      only.empty? || only.any? { |o| o == key || o == id }
    end

    # ---- context ------------------------------------------------------------------------------------------------

    def lookup(path)
      case path
      when "github.workspace" then @root
      when "github.event_name" then "push"
      when "github.ref" then "refs/heads/#{@branch}"
      when "github.ref_name" then @branch
      when "github.sha" then @sha
      when "github.event.before" then base_sha
      when "runner.temp" then @tmp
      when "runner.os" then "macOS"
      when %r{\Asteps\.([\w-]+)\.outputs\.([\w-]+)\z} then @outputs[Regexp.last_match(1)][Regexp.last_match(2)].to_s
      else ""
      end
    end

    def base_sha
      %w[origin/main main].each do |ref|
        return sh_out("git", "merge-base", "HEAD", ref, chdir: @root).strip
      rescue StandardError
        next
      end
      ""
    end

    def expand(text)
      text.to_s.gsub(/\$\{\{(.*?)\}\}/m) do
        value = Expr.new(method(:lookup)).evaluate(Regexp.last_match(1))
        value == true ? "true" : value == false ? "false" : value.to_s
      end
    end

    def condition(job_or_step)
      cond = job_or_step["if"]
      return true if cond.nil?

      Expr.truthy?(Expr.new(method(:lookup)).evaluate(cond.to_s.sub(/\A\$\{\{(.*)\}\}\z/m, '\1')))
    end

    # ---- running ------------------------------------------------------------------------------------------------

    def run_all
      list = jobs
      if list.empty?
        warn "ci-local: no job matches"
        return false
      end
      rounds = @opts[:slow] ? @opts[:rounds] : 1
      install_traps
      (1..rounds).each do |round|
        puts "\n=== slow-runner pass, round #{round} of #{rounds} ===" if @opts[:slow]
        list.each { |j| run_job(j, round) }
      end
      summary
    ensure
      stop_burners
    end

    def run_job(j, round)
      key = j[:key]
      job = j[:job]
      label = round_label(key, round)
      if (why = JOB_SKIPS[key])
        report(j, label, :skipped, why)
        return
      end
      unless condition(job)
        report(j, label, :skipped, "the job's condition is false for a push to #{@branch}")
        return
      end
      runs_on = Array(job["runs-on"]).join(",")
      if runs_on.start_with?("macos") && !RbConfig::CONFIG["host_os"].include?("darwin")
        report(j, label, :skipped, "needs macOS")
        return
      end
      puts "\n--- #{label}: #{job["name"] || j[:id]} (runs-on #{runs_on}#{runs_on.start_with?("ubuntu") ? ", run here on #{RbConfig::CONFIG["host_os"]}" : ""})"
      env = base_env(j)
      skipped = []
      failed = nil
      t0 = Time.now
      @outputs.clear
      job["steps"].each_with_index do |step, i|
        name = step["name"] || step["uses"] || "step #{i + 1}"
        if step["uses"]
          problem = provision(step, env)
          if problem
            failed = [name, problem]
            puts "    SETUP  #{name}: #{problem}"
            break
          end
          next
        end
        next unless step["run"]
        if @opts[:slow] && !slow_step?(key, name)
          next
        end
        unless condition(step)
          skipped << [name, "its condition is false for a push to #{@branch}"]
          next
        end
        if (rule = SKIPS.find { |job_re, step_re, _| key.match?(job_re) && name.match?(step_re) })
          skipped << [name, rule[2]]
          puts "    skip   #{name}  (#{rule[2]})"
          next
        end
        if @opts[:list]
          puts "    run    #{name}"
          next
        end
        ok, secs = run_step(j, step, name, env, i)
        if ok
          puts format("    ok     %-70s %s", name[0, 70], fmt_secs(secs))
        elsif step["continue-on-error"]
          puts format("    FAIL   %-70s %s (continue-on-error)", name[0, 70], fmt_secs(secs))
        else
          failed = [name, "exit status != 0 (log: #{@last_log})"]
          puts format("    FAIL   %-70s %s", name[0, 70], fmt_secs(secs))
          break
        end
      end
      if @opts[:list]
        (SLOW_EXTRA[key] || []).each { |extra, _| puts "    run    #{extra}  (slow pass only)" } if @opts[:slow]
        return
      end

      failed ||= run_slow_extras(j, env)
      report(j, label, failed ? :failed : :passed, failed ? "#{failed[0]}: #{failed[1]}" : "", Time.now - t0, skipped)
    end

    # The Rust test targets of SLOW_EXTRA, in the slow pass, in the environment the job's steps left. Returns [name, why] of the
    # first that failed (the rest still run: one pass lists every failure), or nil.
    def run_slow_extras(j, env)
      return nil unless @opts[:slow]

      failed = nil
      (SLOW_EXTRA[j[:key]] || []).each_with_index do |(name, command), n|
        ok, secs = run_step(j, { "name" => name, "run" => command }, name, env, 100 + n)
        puts format("    %-6s %-70s %s", ok ? "ok" : "FAIL", name[0, 70], fmt_secs(secs))
        failed ||= [name, "exit status != 0 (log: #{@last_log})"] unless ok
      end
      failed
    end

    def round_label(key, round)
      @opts[:slow] ? "#{key} (slow, round #{round})" : key
    end

    def slow_step?(key, name)
      (SLOW_STEPS[key] || []).any? { |re| name.match?(re) }
    end

    def report(j, label, status, note, seconds = 0, skipped = [])
      @results << Result.new(workflow: j[:wf], job: label, name: j[:job]["name"], status: status, note: note, seconds: seconds,
                             skipped: skipped)
      puts "    #{status.to_s.upcase}: #{note}" if status == :skipped
    end

    def base_env(j)
      env = {}
      [j[:wf_env], j[:job]["env"] || {}].each { |h| h.each { |k, v| env[k.to_s] = expand(v) } }
      env["GITHUB_WORKSPACE"] = @root
      env["RUNNER_TEMP"] = @tmp
      env["CI"] = "true"
      env["GITHUB_ACTIONS"] = "true"
      env["GITHUB_REF"] = "refs/heads/#{@branch}"
      env["GITHUB_SHA"] = @sha
      env["UNDRA_CI_LOCAL"] = "1"
      # A path that only exists on a hosted runner (/home/runner/...): the machine's own, from scripts/env.sh, is used.
      env.keys.each do |k|
        next unless env[k].start_with?("/home/runner/")

        if ENV[k] then env.delete(k) else env[k] = env[k].sub(%r{\A/home/runner/}, "#{Dir.home}/") end
      end
      env["CXX"] = "clang++" if env["CXX"] == "clang++-18" && !system("command -v clang++-18 >/dev/null 2>&1")
      if @opts[:slow]
        env["RUST_TEST_THREADS"] = "4"
        env["CARGO_BUILD_JOBS"] = "4"
        env["VITEST_MAX_WORKERS"] = "4"
      end
      env
    end

    # Checks what a `uses:` step would have provisioned, and exports what it would have exported. Returns a problem.
    def provision(step, env)
      uses = step["uses"]
      with = step["with"] || {}
      case uses
      when %r{\Adtolnay/rust-toolchain@(.+)\z}
        pin = Regexp.last_match(1)
        want_targets = with["targets"].to_s.split(/[\s,]+/).reject(&:empty?)
        want_components = with["components"].to_s.split(/[\s,]+/).reject(&:empty?)
        candidates = rust_toolchains_for(pin)
        return "Rust #{pin} is not installed (rustup toolchain install #{pin})" if candidates.empty?

        # The installed toolchain that is the pinned release (a `stable` that is 1.99.0 is 1.99.0) and has what the job asks for.
        found = candidates.find do |t|
          (want_targets - installed_targets(t)).empty? &&
            want_components.all? { |c| installed_components(t).any? { |h| h.start_with?(c) } }
        end
        unless found
          t = candidates.first
          lacking = (want_targets - installed_targets(t)).map { |x| "rustup target add --toolchain #{t} #{x}" } +
                    want_components.reject { |c| installed_components(t).any? { |h| h.start_with?(c) } }.map { |x| "rustup component add --toolchain #{t} #{x}" }
          return "Rust #{pin} (#{t}) lacks what the job installs: #{lacking.join("; ")}"
        end
        env["RUSTUP_TOOLCHAIN"] = found
      when %r{\Aactions/setup-node@}
        want = with["node-version"].to_s
        have = `node -p "process.versions.node" 2>/dev/null`.strip
        return "Node #{want} is required, this machine has #{have.empty? ? "none" : have}" unless have.split(".").first == want.split(".").first
      when %r{\Aactions/setup-java@}
        want = with["java-version"].to_s
        have = `java -version 2>&1`[/version "(\d+)/, 1].to_s
        return "JDK #{want} is required, this machine has #{have.empty? ? "none" : have}" unless have == want.split(".").first
      when %r{\Areactivecircus/android-emulator-runner@}
        return "the emulator step cannot run here (the emulator jobs are on hold)"
      end
      nil
    end

    def installed_toolchains
      @toolchains ||= `rustup toolchain list 2>/dev/null`.lines.map { |l| l.split.first.to_s }
    end

    # Installed toolchains that are `pin`: its own name (1.99.0-<host>, nightly-<host>) first, then any whose rustc says so.
    def rust_toolchains_for(pin)
      @pins ||= {}
      @pins[pin] ||= begin
        named, others = installed_toolchains.partition { |t| t == pin || t.start_with?("#{pin}-") }
        by_version = others.select { |t| `rustc +#{t.shellescape} --version 2>/dev/null`.start_with?("rustc #{pin} ") }
        named + by_version
      end
    end

    def installed_targets(toolchain)
      @targets ||= {}
      @targets[toolchain] ||= `rustup target list --installed --toolchain #{toolchain.shellescape} 2>/dev/null`.split
    end

    def installed_components(toolchain)
      @components ||= {}
      @components[toolchain] ||= `rustup component list --installed --toolchain #{toolchain.shellescape} 2>/dev/null`.split
    end

    def run_step(j, step, name, job_env, index)
      env = job_env.dup
      (step["env"] || {}).each { |k, v| env[k.to_s] = expand(v) }
      env.keys.each do |k|
        next unless env[k].start_with?("/home/runner/")

        if ENV[k] then env.delete(k) else env[k] = env[k].sub(%r{\A/home/runner/}, "#{Dir.home}/") end
      end
      env["CXX"] = "clang++" if env["CXX"] == "clang++-18" && !system("command -v clang++-18 >/dev/null 2>&1")
      gh_path = File.join(@tmp, "github_path")
      gh_env = File.join(@tmp, "github_env")
      gh_out = File.join(@tmp, "github_output")
      [gh_path, gh_env, gh_out].each { |f| FileUtils.rm_f(f) && FileUtils.touch(f) }
      env["GITHUB_PATH"] = gh_path
      env["GITHUB_ENV"] = gh_env
      env["GITHUB_OUTPUT"] = gh_out
      body = expand(step["run"])
      dropped = []
      body = body.lines.reject do |line|
        rule = STRIP_LINES.find { |re, _| line.match?(re) }
        dropped << rule[1] if rule
        rule
      end.join
      SUBSTITUTIONS.each { |re, to, _| body = body.gsub(re, to) }
      puts "           (dropped: #{dropped.uniq.join("; ")})" unless dropped.empty?
      script = File.join(@tmp, "step-#{j[:wf]}-#{j[:id]}-#{index}.sh")
      File.write(script, body)
      wd = File.join(@root, expand(step["working-directory"] || "."))
      @last_log = File.join(@logs, "#{j[:wf]}-#{j[:id]}-#{format("%02d", index)}-#{name.gsub(/[^A-Za-z0-9]+/, "-")[0, 40]}.log")
      # Never under `taskpolicy -b`: background QoS next to burners at normal priority leaves the step no CPU at all.
      cmd = ["bash", "--noprofile", "--norc", "-eo", "pipefail", script]
      t0 = Time.now
      tail = []
      status = nil
      begin
        start_burners if @opts[:slow]
        File.open(@last_log, "w") do |log|
          Open3.popen2e(env, *cmd, chdir: wd, pgroup: true) do |stdin, out, wait|
            stdin.close
            @child = wait.pid
            timer = Thread.new do
              sleep(@opts[:step_timeout] * 60)
              log.puts "ci-local: step timed out after #{@opts[:step_timeout]} minutes, killing it"
              kill_group(wait.pid)
            end
            begin
              out.each_line do |line|
                log.write(line)
                tail << line
                tail.shift while tail.size > 80
                $stdout.write(line) if @opts[:verbose]
              end
              status = wait.value
            ensure
              timer.kill
            end
          end
        end
      ensure
        # Whatever happened to the step (it failed, it was killed, ci-local was interrupted), the burners stop with it.
        @child = nil
        stop_burners
      end
      apply_exports(env_target: job_env, path: gh_path, env_file: gh_env, output: gh_out, step: step)
      ok = status.success?
      unless ok || @opts[:verbose]
        puts "    ---- last lines of #{@last_log}"
        tail.last(60).each { |l| puts "    | #{l}" }
      end
      [ok, Time.now - t0]
    end

    def apply_exports(env_target:, path:, env_file:, output:, step:)
      extra = File.read(path).lines.map(&:strip).reject(&:empty?)
      env_target["PATH"] = (extra + [env_target["PATH"] || ENV["PATH"]]).join(":") unless extra.empty?
      File.read(env_file).lines.each do |l|
        k, v = l.strip.split("=", 2)
        env_target[k] = v if k && v
      end
      return unless step["id"]

      File.read(output).lines.each do |l|
        k, v = l.strip.split("=", 2)
        @outputs[step["id"]][k] = v if k && v
      end
    end

    def kill_group(pid)
      Process.kill("-TERM", pid)
      sleep 3
      Process.kill("-KILL", pid)
    rescue StandardError
      nil
    end

    # ---- the slow-runner pass: burners --------------------------------------------------------------------------

    # One burner is a shell that runs `yes` and cannot outlive ci-local: `yes` dies with the shell (EXIT, TERM, HUP and INT are
    # trapped), and the shell dies within a second of its parent, even when the parent was killed with SIGKILL and trapped nothing.
    # (`sleep 1 & wait` and not a bare `sleep 1`: bash runs a trap only when the foreground command ends, and `wait` is cut short
    # by it, so stopping a burner takes milliseconds, not a second.)
    BURNER = <<~'SH'
      parent=$PPID
      yes >/dev/null 2>&1 &
      burner=$!
      trap 'kill "$burner" 2>/dev/null; wait "$burner" 2>/dev/null; exit 0' EXIT TERM HUP INT
      while kill -0 "$parent" 2>/dev/null; do
        sleep 1 &
        wait $!
      done
    SH

    def start_burners
      @opts[:burners].times { @burners << Process.spawn("sh", "-c", BURNER, out: File::NULL, err: File::NULL) }
    end

    def install_traps
      at_exit { stop_burners }
      %w[INT TERM HUP QUIT].each do |sig|
        Signal.trap(sig) do
          stop_burners
          kill_group(@child) if @child
          exit 130
        end
      end
    end

    def stop_burners
      pids = @burners.dup
      @burners.clear
      pids.each do |pid|
        Process.kill("TERM", pid)
      rescue StandardError
        nil
      end
      pids.each do |pid|
        Process.wait(pid)
      rescue StandardError
        nil
      end
    end

    # ---- output -------------------------------------------------------------------------------------------------

    def fmt_secs(secs)
      secs >= 60 ? format("%dm%02ds", secs / 60, secs % 60) : format("%.1fs", secs)
    end

    def sh_out(*cmd, chdir:)
      out, status = Open3.capture2(*cmd, chdir: chdir)
      raise "#{cmd.join(" ")} failed" unless status.success?

      out
    end

    def summary
      return true if @opts[:list]

      puts "\n=== ci-local summary (#{@sha[0, 12]} on #{@branch}) ==="
      @results.each do |r|
        mark = { passed: "ok  ", failed: "FAIL", skipped: "skip" }[r.status]
        puts format("  %s  %-44s %s  %s", mark, r.job, r.status == :passed ? fmt_secs(r.seconds) : "", r.status == :passed ? "" : r.note)
      end
      not_run = @results.flat_map { |r| r.skipped.map { |name, why| [r.job, name, why] } }
      unless not_run.empty?
        puts "\nSteps that did not run here, and why:"
        not_run.each { |job, name, why| puts "  #{job}: #{name} -- #{why}" }
      end
      failed = @results.select { |r| r.status == :failed }
      puts(failed.empty? ? "\nci-local: every job ran green." : "\nci-local: #{failed.size} job(s) FAILED (logs: #{@logs}).")
      puts "\nWhat only a hosted runner can still tell you: Linux behaviour (glibc, loopback buffers, inotify), macOS 15's own URLSession/XCTest,"
      puts "the runner's speed beyond the slow pass, the Android emulator and the iOS 15/16 simulator runtimes."
      failed.empty?
    end
  end
end

opts = { workflows: CiLocal::WORKFLOWS, only: [], skip: [], slow: false, rounds: 3, burners: 8, list: false,
         verbose: false, root: nil, branch: nil, logs: nil, step_timeout: 120 }
OptionParser.new do |o|
  o.on("--root DIR") { |v| opts[:root] = v }
  o.on("--branch NAME") { |v| opts[:branch] = v }
  o.on("--workflows LIST") { |v| opts[:workflows] = v.split(",") }
  o.on("--only LIST") { |v| opts[:only] = v.split(",") }
  o.on("--skip LIST") { |v| opts[:skip] = v.split(",") }
  o.on("--slow") { opts[:slow] = true }
  o.on("--rounds N", Integer) { |v| opts[:rounds] = v }
  o.on("--burners N", Integer) { |v| opts[:burners] = v }
  o.on("--list") { opts[:list] = true }
  o.on("--logs DIR") { |v| opts[:logs] = v }
  o.on("--step-timeout MINUTES", Integer) { |v| opts[:step_timeout] = v }
  o.on("-v", "--verbose") { opts[:verbose] = true }
end.parse!
abort "ci-local.rb: --root is required (scripts/ci-local.sh passes it)" unless opts[:root]

runner = CiLocal::Runner.new(opts)
exit(runner.run_all ? 0 : 1)
