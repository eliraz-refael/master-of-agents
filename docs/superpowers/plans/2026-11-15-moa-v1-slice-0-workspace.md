# moa v1 slice 0: workspace, Nix, CI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust workspace that builds a `moa` binary with Nix and with plain cargo, a
tmux sandbox that tests can use, and a required `ci` check that builds and tests on Linux
(tmux 3.4) and macOS (tmux latest and 3.8 via Nix).

**Architecture:** This is a Cargo workspace under `crates/`. Slice 0 adds two crates, `moa`
(the binary) and `moa-testkit` (test helpers). Later slices add theirs (roadmap P2). The flake
is built on crane and rust-overlay, and it reads the same `rust-toolchain.toml` that rustup
uses.

**Tech Stack:** Rust edition 2024, clap 4 (derive), assert_cmd, tempfile, Nix flakes
(crane, rust-overlay), GitHub Actions, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md` (§3.9, §10 slice 0, §11.4),
plus the roadmap `docs/superpowers/plans/2026-11-15-moa-v1-roadmap.md` (Global Constraints,
P2, P3, P12).

## Global Constraints

The roadmap's Global Constraints section applies. These matter most in this slice:

- The required check must be named exactly **`ci`**.
- Linux CI uses tmux **3.4**, the minimum. Nix provides tmux **3.8**.
- The build works with Nix (aarch64-darwin first) **and** with plain `cargo`.
- Pin GitHub Actions by full commit SHA with a version comment, like the existing
  `actions/checkout` line.
- **v1 is never put on PATH before the switch-over.** No `nix profile install` and no
  `cargo install`. The user's `moa` and `moactl` commands are the spike's. Run v1 with
  `nix run .` or `cargo run -p moa`.
- The spike's parse checks stay in CI until the spike is removed.

## Review Focus

1. **A sandbox that inherits the developer's environment or login shell.** If the test process has
   `MOA_*`, `CLAUDE_CONFIG_DIR` or `TMUX` set, or a real `~/.tmux.conf`, none of it may reach
   the sandbox server or its panes (lessons 5–6: a test cockpit once reached a real
   account). The pane's shell must be `/bin/sh`, never the account's login shell (fish here,
   `nologin` for the Nix build user). Pinned by Task 2's `env_isolation` and
   `home_shell_and_config_are_the_sandboxes` tests.
2. **Run inside the user's tmux.** `TMUX` is set when the tests run from a moa pane. The
   sandbox must still start its own server and never touch the outer one. Pinned by Task 2
   (the env is cleared, and `TMUX` is never passed).
3. **A long `$TMPDIR` on macOS.** The tmux socket path must fit in 104 bytes. Pinned by Task
   2's `socket_path_fits` test.
4. **A sandbox server left running** after a test panics. Pinned by Task 2's `Drop` test.
   The server is gone after drop.
5. **CI passing while a job was skipped.** The `ci` aggregator must fail when any needed
   job failed **or was cancelled or skipped**. Pinned by Task 4's aggregator condition.

---

### Task 1: Cargo workspace and `moa --version`

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `crates/moa/Cargo.toml`,
  `crates/moa/src/main.rs`, `crates/moa/tests/cli.rs`
- Modify: `.gitignore` (add `/target`, `/result`, `/result-*`)

**Interfaces:**
- Produces: the workspace (members `crates/*`), `[workspace.package]`,
  `[workspace.lints]` and `[workspace.dependencies]`, which every later crate uses with
  `version.workspace = true`, `edition.workspace = true`, `license.workspace = true`,
  `lints.workspace = true`. Also the binary `moa`, whose clap root later slices extend
  with subcommands.

- [ ] **Step 1: Write the workspace manifest and toolchain file**

`rust-toolchain.toml`:
- `channel` = the newest stable Rust release that rust-overlay offers on the day of
  implementation. Write it as an exact `1.x.y`, never `stable`, so Nix and rustup agree.
- `components = ["rustfmt", "clippy"]`.

`Cargo.toml` decides the following:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "<same 1.x as rust-toolchain.toml>"
license = "MIT"

[workspace.lints.rust]
unsafe_code = "deny"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }

[workspace.dependencies]
clap = { version = "4", features = ["derive"] }
assert_cmd = "2"
tempfile = "3"
```

The exact patch versions are whatever `cargo add` resolves at implementation time.
`Cargo.lock` is committed.

- [ ] **Step 2b: Write a minimal `flake.nix` with only `devShells.default`.** On the
  user's Mac a plain `nix shell nixpkgs#cargo …` can't link (`ld: library not found for
  -liconv`), so the dev shell comes first. It holds:
  - the toolchain from `rust-toolchain.toml` (rust-overlay `fromRustupToolchainFile`);
  - `tmux`, `cargo-nextest` and `cargo-insta`;
  - `libiconv` on darwin.

  Task 3 adds the package and checks. The inputs, systems and toolchain are as Task 3
  describes.

- [ ] **Step 2: Write the failing CLI test**

`crates/moa/tests/cli.rs`:

```rust
use assert_cmd::Command;

#[test]
fn version_prints_name_and_version() {
    Command::cargo_bin("moa").unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout("moa 0.1.0\n");
}

#[test]
fn bare_moa_says_it_is_not_ready_and_points_to_the_spike() {
    // Until the switch-over, v1 must never be mistaken for the working moa.
    Command::cargo_bin("moa").unwrap()
        .assert()
        .code(2)
        .stderr(predicates::str::contains("moa v1 is still being built"));
}
```

Add `predicates = "3"` to the workspace dependencies and as a dev-dependency of `moa`.

- [ ] **Step 3: Run it and see it fail**

Run: `nix develop -c cargo nextest run -p moa` (the dev shell from Step 2b).
Expected: a compile failure, because `crates/moa/src/main.rs` doesn't exist.

- [ ] **Step 4: Implement `crates/moa/src/main.rs`**

- A clap `Parser` named `moa`, with `version` from the crate version, and an optional
  subcommand enum that has no variants yet.
- With no subcommand, print this exact line to stderr and exit with code 2:
  `moa v1 is still being built; use the spike's moa for now.`
- `crates/moa/Cargo.toml`:
  - `[[bin]] name = "moa"`;
  - `publish = false`;
  - `lints.workspace = true`.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cargo nextest run -p moa`, then `cargo clippy --workspace --all-targets -- -D
warnings`, then `cargo fmt --check`.
Expected: 2 passed, with no clippy or fmt output.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore flake.nix flake.lock crates/moa
git commit -m "v1: cargo workspace and the moa binary (--version)"
```

---

### Task 2: `moa-testkit` tmux sandbox

**Files:**
- Create: `crates/moa-testkit/Cargo.toml`, `crates/moa-testkit/src/lib.rs`,
  `crates/moa-testkit/src/sandbox.rs`, `crates/moa-testkit/tests/sandbox.rs`,
  `crates/moa-testkit/tests/env_isolation.rs`

**Interfaces:**
- Produces (used from slice 3 on):
  - `pub struct Sandbox`
  - `Sandbox::start() -> std::io::Result<Sandbox>`
  - `Sandbox::tmux(&self, args: &[&str]) -> std::io::Result<String>`: returns stdout with
    trailing newlines trimmed. On a non-zero exit it returns `Err` carrying stderr.
  - `Sandbox::socket_name(&self) -> &str`
  - `Sandbox::home(&self) -> &std::path::Path`
  - `Sandbox::env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)>`: the exact
    environment the sandbox gives tmux, so later tests can run `moa` against it.
  - `impl Drop for Sandbox`: runs `kill-server` and waits until the server process is gone.
  - `pub fn wait_for<T>(limit: Duration, f: impl FnMut() -> Option<T>) -> Option<T>`.

- [ ] **Step 1: Write the failing tests**

`crates/moa-testkit/tests/sandbox.rs`:

```rust
use moa_testkit::Sandbox;

#[test]
fn runs_a_supported_tmux() {
    let sb = Sandbox::start().unwrap();
    let v = sb.tmux(&["display-message", "-p", "#{version}"]).unwrap();
    // "3.4", "3.8", "3.5a", "next-3.9": take the first "major.minor" digits
    let digits: String = v.trim_start_matches("next-").chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut it = digits.split('.').map(|n| n.parse::<u32>().unwrap());
    let (major, minor) = (it.next().unwrap(), it.next().unwrap());
    assert!((major, minor) >= (3, 4), "tmux {v} is older than 3.4");
}

#[test]
fn socket_path_fits() {
    let sb = Sandbox::start().unwrap();
    let p = sb.tmux(&["display-message", "-p", "#{socket_path}"]).unwrap();
    assert!(p.len() < 104, "socket path is {} bytes: {p}", p.len());
    assert!(p.ends_with(sb.socket_name()));
}

#[test]
fn home_shell_and_config_are_the_sandboxes() {
    let sb = Sandbox::start().unwrap();
    let home = sb.tmux(&["show-environment", "-g", "HOME"]).unwrap();
    assert_eq!(home, format!("HOME={}", sb.home().display()));
    assert_ne!(sb.home(), std::path::Path::new(&std::env::var("HOME").unwrap()));
    // the sandbox conf, not the user's: /bin/sh panes and tmux's default prefix
    assert_eq!(sb.tmux(&["show", "-gv", "default-shell"]).unwrap(), "/bin/sh");
    assert_eq!(sb.tmux(&["show", "-gv", "prefix"]).unwrap(), "C-b");
    // panes start in the sandbox home, not in the repo the tests run from
    assert_eq!(sb.tmux(&["display-message", "-p", "-t", "sandbox", "#{pane_current_path}"]).unwrap(),
               std::fs::canonicalize(sb.home()).unwrap().display().to_string());
}

#[test]
fn server_is_gone_after_drop() {
    let sb = Sandbox::start().unwrap();
    let pid: u32 = sb.tmux(&["display-message", "-p", "#{pid}"]).unwrap().parse().unwrap();
    drop(sb);
    let alive = std::process::Command::new("kill").args(["-0", &pid.to_string()])
        .status().unwrap().success();
    assert!(!alive, "tmux server {pid} still running after drop");
}

#[test]
fn two_sandboxes_do_not_share_a_server() {
    let (a, b) = (Sandbox::start().unwrap(), Sandbox::start().unwrap());
    assert_ne!(a.socket_name(), b.socket_name());
    let pa = a.tmux(&["display-message", "-p", "#{pid}"]).unwrap();
    let pb = b.tmux(&["display-message", "-p", "#{pid}"]).unwrap();
    assert_ne!(pa, pb);
}
```

`crates/moa-testkit/tests/env_isolation.rs` is its own test binary, so setting the
environment can't race other tests:

```rust
#[test]
fn parent_env_never_reaches_the_sandbox() {
    #[allow(unsafe_code)] // roadmap P15: tests may set the env in a one-test binary
    // SAFETY: this test binary has exactly one test, so no other thread reads the env.
    unsafe {
        std::env::set_var("MOA_BRAIN", "leak");
        std::env::set_var("CLAUDE_CONFIG_DIR", "/leak");
        std::env::set_var("TMUX", "/tmp/leak,1,0");
    }
    let sb = moa_testkit::Sandbox::start().unwrap();
    let global = sb.tmux(&["show-environment", "-g"]).unwrap();
    for bad in ["MOA_", "CLAUDE_", "TMUX="] {
        assert!(!global.lines().any(|l| l.starts_with(bad)), "{bad} leaked:\n{global}");
    }
    // and a process in a pane doesn't see them either: dump its env to a file (no typing,
    // no screen scraping, no dependence on the user's shell)
    let out = sb.home().join("env.txt");
    sb.tmux(&["new-window", "-d", &format!("env > {}.tmp && mv {0}.tmp {0}", out.display())]).unwrap();
    let env = moa_testkit::wait_for(std::time::Duration::from_secs(5), || std::fs::read_to_string(&out).ok())
        .expect("the pane never wrote its env");
    assert!(!env.contains("leak"), "a pane saw a leaked variable:\n{env}");
    assert!(env.lines().any(|l| l == format!("HOME={}", sb.home().display())));
}
```

`moa_testkit::wait_for<T>(limit: Duration, f: impl FnMut() -> Option<T>) -> Option<T>` tries
`f` every 20 ms until it returns `Some` or the limit passes. It's for tests only (tests may
wait; the product never polls), and `Drop` uses it too.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo nextest run -p moa-testkit`
Expected: a compile failure, because `moa_testkit::Sandbox` doesn't exist.

- [ ] **Step 3: Implement `Sandbox` in `crates/moa-testkit/src/sandbox.rs`**

The decisions:

- **Socket name:** `moa-test-<pid>-<n>`, where `n` is a process-wide `AtomicU32`.
- **Directories:**
  - One `tempfile::TempDir` holds `home/` and `tmux.conf`.
  - The tmux socket directory is a **second, short** temp dir:
    `tempfile::Builder::new().prefix("moa").tempdir_in("/tmp")`. tmux reports the real
    path, and on macOS the default `$TMPDIR` (`/private/var/folders/…/T/`) leaves only a
    few bytes under the 104-byte limit.
- **`tmux.conf`:** holds one line, `set -g default-shell /bin/sh`. Without `SHELL`, tmux
  falls back to the account's login shell. That's fish on the user's Mac, and `nologin`
  for the Nix build user, where every pane dies at once and takes the server with it.
- **Environment:** every tmux command runs with `env_clear()` and then exactly these:
  - `PATH` (copied from the parent);
  - `HOME=<dir>/home`;
  - `SHELL=/bin/sh`;
  - `TMUX_TMPDIR=<socket dir>`;
  - `TERM=xterm-256color`.

  `TMUX` is never passed.
- **Start:** `tmux -u -L <name> -f <dir>/tmux.conf new-session -d -s sandbox -c <dir>/home
  -x 200 -y 50`. `-u` forces UTF-8 without depending on a locale.
- **Drop:** read `#{pid}`, run `kill-server`, then `wait_for(2 s)` until `kill -0 <pid>`
  fails. Ignore errors; a drop never panics.

`crates/moa-testkit/Cargo.toml`: `publish = false`, with `tempfile` as a normal dependency
(the testkit is only ever a dev-dependency of other crates).

- [ ] **Step 4: Run them and see them pass**

Run: `cargo nextest run -p moa-testkit`
Expected: 6 passed. Also run it from inside a tmux pane (`TMUX` set) and see it still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/moa-testkit Cargo.toml Cargo.lock
git commit -m "v1: moa-testkit tmux sandbox with an isolated env and home"
```

---

### Task 3: Nix flake (crane + rust-overlay)

**Files:**
- Modify: `flake.nix`, `flake.lock` (both created by Task 1 with the dev shell)

**Interfaces:**
- Produces:
  - `packages.<system>.default` (the `moa` binary; `meta.mainProgram = "moa"`);
  - `checks.<system>.{moa, clippy, fmt, nextest}`;
  - `devShells.<system>.default` (toolchain from `rust-toolchain.toml`, plus `tmux`,
    `cargo-nextest`, `cargo-insta`).

  The systems are `aarch64-darwin`, `x86_64-linux` and `aarch64-linux`. nixos-unstable
  has dropped `x86_64-darwin`, so it's left out.

- [ ] **Step 1: Extend `flake.nix` with the package and checks**

Decisions (the implementer writes the Nix):

- **Inputs:**
  - `nixpkgs` (`nixos-unstable`);
  - `crane`;
  - `rust-overlay` (with `inputs.nixpkgs.follows = "nixpkgs"`).
- **Toolchain:** `craneLib = (crane.mkLib pkgs).overrideToolchain (p:
  p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)`.
- **Source:** `craneLib.cleanCargoSource ./.`. Later slices widen this with a filter when
  crates embed non-Rust files (SQL, fixtures).
- **Common args:**
  - `strictDeps = true`;
  - `buildInputs = lib.optionals stdenv.isDarwin [ pkgs.libiconv ]`, because linking fails
    on darwin without it (R4 §7);
  - `nativeCheckInputs = [ pkgs.tmux ]`.
- **Dependencies:** `cargoArtifacts = craneLib.buildDepsOnly commonArgs`, shared by every
  derivation.
- **The package:** `buildPackage` with `doCheck = false`; the tests run in the `nextest`
  check.
- **The checks:**
  - `clippy` = `cargoClippy` with `cargoClippyExtraArgs = "--all-targets -- -D warnings"`;
  - `fmt` = `cargoFmt`;
  - `nextest` = `cargoNextest` with `nativeBuildInputs = [ pkgs.tmux ]`.

- [ ] **Step 2: Lock and check**

Run: `nix flake lock && nix flake check -L`
Expected: every check builds. The `nextest` log shows `8 passed` (2 from `moa`, 6 from
`moa-testkit`).

- [ ] **Step 3: Check the package and the dev shell**

Run: `nix run . -- --version`
Expected: `moa 0.1.0`

Run: `nix develop -c sh -c 'cargo --version && tmux -V && cargo nextest --version'`
Expected: three version lines, with the cargo version matching `rust-toolchain.toml`.

- [ ] **Step 4: Commit**

```bash
git add flake.nix flake.lock
git commit -m "v1: nix flake (crane, rust-overlay) with clippy/fmt/nextest checks"
```

---

### Task 4: CI, with the required `ci` check over linux, macos and nix

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md` (a short "Building v1" section)

**Interfaces:**
- Consumes: Task 3's `checks`, and Task 1's `rust-toolchain.toml` (read by rustup on the
  runners).
- Produces: the jobs `linux`, `macos` and `nix`, plus the required `ci` aggregator.

- [ ] **Step 1: Rewrite `.github/workflows/ci.yml`**

```yaml
name: ci
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
jobs:
  linux:   # ubuntu-24.04's apt tmux is 3.4: the minimum version moa supports
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@<sha> # newest major
      - name: Spike parses (frozen prototype; removed with the spike)
        run: |
          python3 -m py_compile spike/moa-proto/*.py
          for f in spike/moa-proto/moa spike/moa-proto/moactl spike/master-chat/*.sh spike/master-chat/hooks/*.sh spike/master-chat/master/hooks/*.sh; do bash -n "$f"; done
      - run: sudo apt-get update && sudo apt-get install -y tmux && tmux -V
      - run: rustup toolchain install   # reads rust-toolchain.toml (`rustup show` stopped installing in 1.28)
      - uses: Swatinem/rust-cache@<sha> # v2.x
      - uses: taiki-e/install-action@<sha> # v2.x
        with: { tool: cargo-nextest }
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo nextest run --workspace
  macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@<sha> # newest major
      - run: brew install tmux && tmux -V
      - run: rustup toolchain install
      - uses: Swatinem/rust-cache@<sha> # v2.x
      - uses: taiki-e/install-action@<sha> # v2.x
        with: { tool: cargo-nextest }
      - run: cargo nextest run --workspace
  nix:     # aarch64-darwin, the user's platform; nixpkgs tmux (3.8)
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@<sha> # newest major
      - uses: cachix/install-nix-action@<sha> # v31.x
      - run: nix flake check -L
  ci:      # the required check: green only if every job above succeeded
    if: always()
    needs: [linux, macos, nix]
    runs-on: ubuntu-latest
    steps:
      - run: |
          for r in "${{ needs.linux.result }}" "${{ needs.macos.result }}" "${{ needs.nix.result }}"; do
            [ "$r" = success ] || { echo "a required job ended with: $r"; exit 1; }
          done
```

Replace each `<sha>` with the full commit SHA of that action's latest release, and keep
the version comment. `actions/checkout` moves to its newest major release; v4 runs on
Node 20.

- [ ] **Step 2: Add the README section**

Under a heading `## Building v1 (in progress)`, add four lines:

- `nix develop` (or rustup plus tmux ≥ 3.4);
- `cargo nextest run --workspace`;
- `nix run . -- --version`;
- "v1 is not installed on PATH until the switch-over; `moa` on PATH is the spike."

- [ ] **Step 3: Push the branch and open the PR**

```bash
git add .github/workflows/ci.yml README.md
git commit -m "v1: CI runs linux (tmux 3.4), macos and nix; ci aggregates them"
git push -u origin v1/slice-0-workspace
gh pr create --title "v1 slice 0: workspace, Nix, CI" --body "<what it adds; Restart needed: nothing; the spike is untouched>"
```

- [ ] **Step 4: See CI pass**

Run: `gh pr checks --watch`
Expected:
- `linux`, `macos`, `nix` and `ci` all pass.
- The `linux` log shows `tmux 3.4`.
- The `nix` log shows `nextest` running 8 tests.

- [ ] **Step 5: Prove the aggregator fails closed**

Push a throwaway commit that makes `cargo fmt --check` fail, and see `ci` go red. Then drop
the commit with `git reset --hard HEAD~1 && git push --force-with-lease`, and see `ci` go
green again.

- [ ] **Step 6: Review and hand over**

Run `/code-review medium --fix` on the PR, push the fixes, and wait for CI to go green.
Then tell the user:
- what landed;
- how to try it (`nix run . -- --version` on the branch);
- "Restart needed: nothing; the spike is untouched."
