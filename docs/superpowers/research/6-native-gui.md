# 6. Native GUI for moa: toolkits, live terminals, shared view-models, web UI

Research date: 2026-11-15. Versions were read from the crates.io API on that date unless
another source is named. "dl90" means crates.io downloads in the last 90 days, a rough
proxy for how many people use a crate.

**TL;DR.** Build the GUI as the **daemon-served web UI**, which the draft constitution
already requires. Use server-rendered HTML fragments pushed over SSE, with no npm and no
build step, and offer it in a **thin Tauri 2 (or bare `wry`) window** for people who want
a desktop app. Both render the same Rust view-models as the TUI. Make live terminals a
second step. Ship "open in Ghostty" plus read-only live previews first, then interactive
terminals bridged through the tmux control-mode connection the daemon already holds.
**Runner-up:** a fully native **GPUI** app (`gpui-kit` + `gpui-component` +
`alacritty_terminal`/`libghostty-vt`). Pick it if "native" must mean "no web view at all"
and we accept a lot of churn.

---

## 1. Rust GUI toolkits, as of November 2026

### Summary table

| Toolkit | Latest (date) | Maturity | macOS look | Markdown | Accessibility | Keyboard-first | Nix on darwin | Licence |
|---|---|---|---|---|---|---|---|---|
| **iced** | 0.14.0 (2025-12-07) | pre-1.0; 0.14 billed as the last experimental release before 1.0. Used by COSMIC | custom-drawn, COSMIC-ish, not Mac-like | built-in `markdown` widget + `iced_highlighter` | **none upstream** (issue #552 open since 2020; AccessKit only in forks) | OK: subscriptions, focus ops; you build the keymap yourself | easy (pure Rust, wgpu) | MIT |
| **egui / eframe** | 0.37.0 (2026-10-15), dl90 9.9M | mature, very popular, immediate-mode | "tool/debug UI" look, themable but not Mac-like | `egui_commonmark` 0.26 | **AccessKit on by default** in eframe | good (Tab focus, input queries) | easy | MIT/Apache |
| **GPUI** (Zed) | crates.io `gpui` frozen at 0.2.2 (2025-10-22). Usable through Longbridge's `gpui-pre` 0.3.12 snapshots (2026-11-09) and `gpui-kit` | powers Zed (v1.22, 2026-09-30). API unstable, "often breaking changes" | most polished of the Rust options; CoreText text, Metal | `gpui-component` 0.9.1 ships markdown + tree-sitter | AccessKit work exists in forks; upstream status unverified | **best**: actions, keymaps and contexts, as in Zed | needs `runtime_shaders` feature to avoid Xcode `metal` (nixpkgs zed does this) | Apache-2.0 |
| **Slint** | 1.18.1 (2026-09-21) | 1.x stable, desktop push since late 2025 | `cupertino` style (default is `fluent`) | `StyledText` + `@markdown()` since 1.16 (2026-04), "initial step" | yes: `accessible-*` properties | `FocusScope`; shortcut API was still in progress in 2025 | easy | **GPLv3 or Royalty-free 2.0 (needs attribution) or commercial** |
| **Tauri 2** | 2.12.5 (3.0.0-alpha.6, 2026-11-06) | mature, very large community | system WKWebView; HTML/CSS, so "Mac-like" only if styled that way | anything web (HTML) | WebKit's: best of all Rust options | whatever you build in HTML/JS | `cargo-tauri` 2.12.4 in nixpkgs | MIT/Apache |
| **Dioxus** | 0.7.10 (0.8.0-alpha.3, Oct 2026) | desktop (webview) mode mature. Native renderer (Blitz 0.3 beta) still alpha-grade | webview, or Blitz custom render | HTML in webview | webview: WebKit. Blitz: AccessKit, partial | DIY | `dx` CLI, Rust only | MIT/Apache |
| **Makepad** | makepad-widgets 1.0.0 (2025-05-13), dl90 ~1.7k | 1.0 shipped with "rough edges". Little crates.io activity since | custom shader UI | own | weak | DIY | OK | MIT/Apache |
| **Xilem / Masonry** | 0.4.0 (2025-10-29), dl90 ~3–4k | research-grade, no 2026 release | custom (Vello) | none built in | AccessKit-first design | DIY | OK | Apache-2.0 |
| **cxx-qt** (Qt/QML) | 0.10.0 (2026-08-24) | stable bridge. Qt itself very mature | Qt macOS style, quite native | Qt `TextEdit` markdown | Qt a11y, good | good | heavy (Qt6 + C++ + QML) | MIT/Apache bridge; Qt LGPL |
| **SwiftUI/AppKit + Rust daemon** | n/a | most native | **truly native** | `AttributedString(markdown:)` / third-party | **best** | **best** (menus, `.keyboardShortcut`) | **hard**: needs Xcode/xcodebuild, outside the Nix sandbox | n/a |

### Notes per toolkit

**iced.** The Elm architecture (message → update → view) fits a daemon event stream well,
and 0.14 added reactive rendering, headless testing, IME and hot reloading
([iced 0.14.0 release](https://github.com/iced-rs/iced/releases/tag/0.14.0)). There is
no upstream accessibility yet: the tracking issue
[iced#552](https://github.com/iced-rs/iced/issues/552) is still open, and AccessKit exists
only in forks such as `toddy-iced`
([docs.rs](https://docs.rs/crate/toddy-iced/0.6.2)). There is a terminal widget,
`iced_term` 0.8.0 (iced 0.14 + alacritty_terminal 0.25.1, 2026-03-27). It is a small
project (dl90 ~6.8k). The look is fine, but it doesn't feel like a Mac app.

**egui.** It's the fastest way to get something on screen, it has the most users, and
AccessKit is on by default ([eframe docs](https://docs.rs/crate/eframe/latest)). Because
it is immediate-mode, a living board with animations is easy to repaint but hard to make
feel polished. The terminal widget `egui_term` is stale (0.1.0, egui 0.31, April 2025, "does not
provide full terminal features") ([lib.rs](https://lib.rs/crates/egui_term)). It's a good
fit for an internal tool, but not for the "fun is a requirement" bar.

**GPUI.** Zed's framework is the most capable pure-Rust option for a keyboard-first,
polished, GPU-rendered app with embedded terminals. Zed's own terminal panel is GPUI plus
`alacritty_terminal`. The catch is distribution:
- `gpui` on crates.io has been frozen at 0.2.2 since October 2025. Zed main split it into
  `gpui`, `gpui_platform`, `gpui_web` and `gpui_macros`, none of which are published
  ([blog survey, 2026-10](https://baeseokjae.github.io/posts/native-app-for-coding-agents-rust-gpui/)).
- In practice people use **Longbridge's `gpui-pre-*` snapshot crates** ("gpui-pre snapshot
  of zed@d2a4fb6", 0.3.12, 2026-11-09) through `gpui-kit`, plus `gpui-component` 0.9.1
  (60+ components, markdown, tree-sitter)
  ([longbridge/gpui-kit](https://github.com/longbridge/gpui-kit),
  [docs.rs gpui-pre](https://docs.rs/crate/gpui-pre/0.3.2)). Mixing gpui channels gives
  confusing trait mismatches, so pin everything and commit `Cargo.lock`.
- On macOS, the default build shells out to Xcode's proprietary `metal` compiler.
  nixpkgs' zed package enables `gpui_platform/runtime_shaders` on darwin to avoid this
  ([nixpkgs commit](https://code.ornl.gov/nix/nixpkgs/-/commit/2c9d900c6e33deb81cebb85e83e23d26fbce3198),
  [Zed macOS build docs](https://zed.dev/docs/development/macos)).
- Accessibility: AccessKit integration appears in forks (`rgpui`, `open-gpui`). In May 2025
  Zed had no a11y tree, and I could not confirm the upstream state in 2026.
- **Precedent very close to moa:** *Sheprd* (formerly `herdr-gpui`) is an unaffiliated
  GPUI client for the herdr agent daemon. It pins gpui 0.3.10 from `gpui-pre`, ships a Nix
  flake, gets **terminal cells or raw output streamed from the daemon**, and the daemon
  owns the processes, so closing the GUI leaves sessions running
  ([README](https://raw.githubusercontent.com/penso/herdr-gpui/main/README.md)). *Paneflow*
  (GPUI + alacritty_terminal 0.26) is another example. Zed's macOS DMG is 112 MB, so don't
  expect a small binary.

**Slint.** The licence is the issue for an MIT project. The options are GPLv3,
Royalty-free 2.0 (which requires showing "made with Slint" attribution), or commercial
([Slint pricing/licence FAQ](https://slint.dev/pricing),
[desktop-ready post](https://slint.dev/blog/making-slint-desktop-ready)). Otherwise it is
solid: a `cupertino` style
([styles](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/style/)), a full
set of `accessible-*` properties
([common properties](https://docs.slint.dev/latest/docs/slint/reference/common/)), and
markdown through `StyledText` since 1.16
([1.16 release](https://slint.dev/blog/slint-1.16-released)). It has no terminal widget.
Its DSL (`.slint`) is a second language.

**Tauri 2.** It runs no Node and no V8 at runtime. It uses WKWebView on macOS
(JavaScriptCore) and WebKitGTK on Linux, so it meets the "no Electron, no V8" constraint.
**The frontend does not need npm:**
- Install the CLI with `cargo install tauri-cli` (nixpkgs has `cargo-tauri` 2.12.4), or
  scaffold with `cargo create-tauri-app`, which has a vanilla HTML/CSS/JS template
  ([Tauri v2 docs](https://v2.tauri.app/llms.txt)).
- Point `build.frontendDist` at a folder of plain files and leave
  `beforeBuildCommand`/`beforeDevCommand` empty.
- Set `app.withGlobalTauri: true` to get `window.__TAURI__` without importing
  `@tauri-apps/api` from npm. An early v2 schema had this key in the wrong section
  ([tauri#9248](https://github.com/tauri-apps/tauri/issues/9248)), so verify it on 2.12.
- Simpler still: the window can just load `http://127.0.0.1:<port>` served by the
  daemon. Then Tauri adds native chrome (menu bar, global hotkey, notifications, tray,
  dock badge) and **no second UI codebase**.
- Tauri 3.0 is in alpha (Nov 2026). It moves runtime selection into crates, including a
  CEF runtime ([release notes](https://newreleases.io/project/github/tauri-apps/tauri/release/tauri-v3.0.0-alpha.0)).
  Stay on 2.x for now.
- Even lighter: `wry` 0.57 + `tao` 0.37 (Tauri's own webview and window crates) make a
  ~200-line Rust shell with no CLI or config. With zero code, Safari's "Add to Dock"
  (macOS 14+) turns the daemon's URL into a windowed app.
- Atuin Desktop is the model of a Rust CLI/TUI project with a Tauri desktop app. Its
  runtime layer has no Tauri dependencies
  ([Atuin Desktop OSS](https://blog.atuin.sh/atuin-desktop-open-source/)).

**Dioxus.** 0.7 shipped "Dioxus Native" on Blitz, its own HTML/CSS engine. The 0.8 alphas
(Blitz 0.3.0-beta.4) keep improving it
([v0.8.0-alpha.0](https://newreleases.io/project/github/DioxusLabs/dioxus/release/v0.8.0-alpha.0)),
but the Blitz README still warns of "many bugs and missing features"
([blitz](https://github.com/DioxusLabs/blitz)). Desktop mode (wry webview) is mature, and
the same RSX code compiles to web (WASM) through the `dx` CLI with no npm. It is a valid
alternative to hand-written HTML if we want the UI in Rust, but terminals would still mean
JS interop.

**Makepad, Xilem.** Not now. Makepad has had no crates release since 1.0 in May 2025.
Xilem/Masonry 0.4 (October 2025) is still research-grade
([Linebender, Oct 2025](https://linebender.org/blog/tmil-22/)).

**cxx-qt.** The 0.8 release (December 2025) stabilised the build system, and 0.10 came out in
August 2026 ([KDAB/cxx-qt releases](https://github.com/KDAB/cxx-qt/releases)). Qt gives
good a11y and a native-ish feel, but Qt6 + C++ + QML on nix-darwin is a heavy toolchain
for one person, and QML is yet another language.

**SwiftUI/AppKit talking to the Rust daemon.** This gives the best Mac feel,
accessibility and menu/keyboard integration. The daemon boundary means we don't need FFI
at all: Swift talks JSON over the daemon's socket. UniFFI 0.32
([docs](https://mozilla.github.io/uniffi-rs/latest/swift/xcode.html)) or Crux
([shell guide](https://redbadger.github.io/crux/part-1/shell/apple/)) are only needed to
share the view-model crate. Precedents:
- **cmux**: Swift + AppKit + libghostty (via `GhosttyKit.xcframework` built from source),
  a sidebar of agent tabs, GPL ([cmux](https://cmux.com/built-on-ghostty)).
- **herdrm**: a SwiftUI client for herdr's socket that uses SwiftTerm for terminals.
- **SwiftTerm**: an AppKit `NSView` terminal with Metal rendering, v1.15
  ([SwiftTerm](https://github.com/migueldeicaza/SwiftTerm)).

Costs: a second language, macOS only (the Linux nice-to-have is lost), and builds that
need Xcode, which nix-darwin can't sandbox well.

---

## 2. Embedding live terminals

### Emulator cores (bytes in, screen grid out)

| Crate | Version | Notes |
|---|---|---|
| `alacritty_terminal` | 0.26.0 (2026-04-06), dl90 1.6M | The default choice. Used by Zed, Paneflow, `iced_term` and `egui_term`. Mature grid, scrollback, selection and damage tracking. No renderer: you draw cells. |
| `libghostty-vt` (Rust bindings) | 0.2.2 (2026-09-28), dl90 142k | Wraps **libghostty-vt 1.0** (C ABI stable since **2026-11-06**, MIT): `Terminal`, `RenderState`, **key encoder**. Builds with Zig by default, or links a system lib through pkg-config. `!Send`, so drive it from one thread. Bindings still pre-1.0. Used by herdr; JetBrains IDEs use libghostty for their terminals ([Hashimoto, libghostty 1.0](https://mitchellh.com/writing/libghostty-1-0), [docs.rs](https://docs.rs/libghostty-vt)). Highest fidelity for modern TUIs. Only the **VT** part is 1.0. The full embedding/surface API that cmux uses is not declared stable. |
| `vt100` | 0.18.1 (2026-10-19), dl90 9.9M | Small parser plus screen model with `contents_formatted()` and diffing. Used by mprocs and others. Ideal for **daemon-side previews** and cheap snapshots. Less complete than the two above. |
| wezterm `term` | only forks on crates.io (`tattoy-wezterm-term` 0.1.0-fork.5, 2025-07) | Very complete, but not published upstream, so it means a git dependency on the wezterm monorepo. Avoid. |
| `termwiz` | 0.23.3 (2025-03) | wezterm's terminal-apps toolkit, not a GUI emulator. |

Widgets: `iced_term` 0.8 (alive, small), `egui_term` 0.1 (stale). GPUI has no published
terminal widget. Zed's `terminal_view` isn't on crates.io, so people copy the pattern (see
Sheprd and Paneflow). For the web: **xterm.js** (VS Code, Zellij's web client) or
**ghostty-web** 0.4 (Coder; libghostty-vt as about 400 KB of WASM behind an xterm.js-
compatible API, pre-1.0) ([coder/ghostty-web](https://github.com/coder/ghostty-web)).

### Getting a tmux pane's bytes into a GUI

**A. Each GUI terminal runs `tmux attach` in its own PTY.** The GUI's emulator runs
`tmux attach -t <grouped-session>` through `portable-pty`, and the emulator sees tmux's
rendered output.
- Pros: trivial and full-fidelity. Input, mouse, copy mode and resize all just work,
  because tmux does the work.
- Cons: one tmux client per tile; tmux's status line and keys show through. Hide them
  with a per-tile **grouped session** (`new-session -t <main> -s moa-gui-N`,
  `status off`), which also gives each tile its own current window. Window size is decided
  by tmux's `window-size` policy across all clients, so a small TUI and a big GUI tile
  showing the same window fight over its size.

**B. Use the daemon's single control-mode connection (`tmux -CC`).** The draft
constitution already wants one long-lived `tmux -C`. Its `%output %<pane> <octal-escaped
bytes>` stream carries **raw application output per pane**, so the daemon (or the GUI)
feeds it into one emulator per pane.
- Input goes back with `send-keys -H <hex>`, or `send -l` for literal text.
- Size per window: `refresh-client -C @<win>:WxH`. The local tmux 3.8 man page documents
  the `window ID:widthxheight` form.
- Flow control: `refresh-client -f pause-after=N` switches to `%extended-output`, and
  `%pause`/`%continue` plus `refresh-client -A '%<pane>:on|off|pause|continue'` let the
  daemon stop receiving output from panes nobody is watching.
- Initial screen and resync: `capture-pane -p -e -J` plus cursor position.
- Sources: [tmux wiki: Control Mode](https://github.com/tmux/tmux/wiki/Control-Mode); the
  tmux(1) man page. Parsers exist (`tmuxctl` 0.1, `tmux-cmc`), but they're small;
  the protocol is simple enough to own. iTerm2 is the reference client. Ghostty 1.3 (March
  2026) has a control-mode *parser* that isn't yet wired to its GUI, and cmux has a beta
  "remote tmux mirroring" built on `-CC` (needs tmux ≥ 3.2)
  ([cmux docs](https://cmux.com/docs/remote-tmux)).
- Pros: no extra tmux clients, one event-driven stream (fits the "no polling" rule),
  per-pane on/off, and the daemon can keep a `vt100`/`libghostty-vt` model of **every**
  pane. That model is useful far beyond the GUI: live thumbnails in the TUI tree, "what is
  the agent showing right now" for the central agent, and web previews.
- Cons: we own an emulator and its fidelity. Claude Code and Codex are rich TUIs with
  synchronized output, truecolor, wide glyphs and OSC 8 links. libghostty-vt or
  alacritty_terminal handle these; vt100 is weaker. Keyboard fidelity is bounded by what
  `send-keys` can express: modifier combos and extended keys work, but there's no kitty
  keyboard protocol. Mouse and copy mode would need reimplementing (or `send -X`
  copy-mode commands). Bytes ≥ 0x80 may be escaped too, so the stream is several times
  larger than raw for non-ASCII. That's fine locally.

**C. Own the PTYs (herdr's model).** This drops tmux. Out of scope: the constitution
chose tmux as the process host.

**Performance.** Not a concern at moa's scale: a handful of visible agents, local
transport. Paneflow-style coalescing works: drain output for 4 ms or 100 events, then
one repaint. Zed and Ghostty prove the rendering side. The real costs are the
engineering ones above.

### Verdict: cockpit or companion?

A "GUI cockpit with real terminals" is **feasible**, and there are now several
precedents: cmux, Sheprd, the herdr desktop app (released around 2026-10-30; "the window
arranges terminals, the core runs them"), and Zellij's web client. But:
1. the user's real terminal is Ghostty and the TUI comes first. In a GUI cockpit, an
   embedded terminal is always slightly worse than Ghostty: keys, fonts, IME, copy, links.
2. Two clients viewing one pane at different sizes is a real tmux problem with either
   approach.

**Recommendation: build it in stages.**
1. **Companion.** The GUI or web UI shows tree, board, chat and status, and "Open" focuses
   the agent in Ghostty (`open -na Ghostty.app --args -e tmux attach -t …`; check the
   exact flags. Ghostty's scripting API is planned for 1.4) or in the moa TUI.
2. **Live read-only previews.** The daemon keeps a `vt100` (later `libghostty-vt`) model
   per pane from control-mode `%output`, and clients render it as a thumbnail or peek
   pane. This is cheap, and it delivers most of the "alive" feel.
3. **Interactive embedded terminal** (approach B, with per-window `refresh-client -C`).
   Only do this if stages 1–2 leave a real gap. In the browser/Tauri, use vendored
   xterm.js or ghostty-web. In a native GPUI app, use alacritty_terminal or libghostty-vt.

---

## 3. Sharing code between TUI and GUI

Pattern: **daemon owns the state → `moa-view` crate turns it into view-models → thin
renderers.**

- **`moa-proto`**: the daemon wire types (serde), a versioned *snapshot + ordered event
  stream* (`seq` numbers, so a client resyncs with "events since N").
- **`moa-view`** (pure Rust; no ratatui, HTML or GPUI dependency): `TreeRow { depth, guide,
  glyph, label, state, badges, anim }`, `BoardColumn/Card { priority, due }`,
  `ChatItem { markdown, actions: Vec<Action> }`, `Prompt { question, choices: yes/later/no }`,
  `StatusLine`. Also the **`Action` enum and the default keymap table**, so a key does
  the same thing in every client, plus pure reducers for client-local state (selection,
  collapse, filter). Unit-test it with snapshot tests.
- **Renderers**:
  - `moa-tui` with ratatui 0.30.
  - The web UI: askama 0.16 or maud templates that render the *same* view-models to HTML
    fragments.
  - Later, optionally, GPUI or SwiftUI. Swift reads `moa-view` JSON from the daemon, or
    links it through UniFFI.

Examples in the wild:
- **Zellij**: one server process with two clients, the terminal client and a web client
  (0.43+, xterm.js over two WebSockets, served by the embedded server)
  ([Zellij 0.43](https://zellij.dev/news/web-client-multiple-pane-actions/),
  [heise](https://heise.de/-10549868)).
- **herdr**: herdr-core (server) with the TUI, the desktop app, a CLI, and third-party
  GPUI (Sheprd) and SwiftUI (herdrm) clients on its socket
  ([herdr remote docs](https://herdr.dev/docs/remote)).
- **aoe**: a TUI plus an `aoe serve` web dashboard, still flagged beta.
- **Atuin**: the CLI/TUI plus Atuin Desktop (Tauri), with a runtime layer kept free of
  Tauri.
- **Crux** (Red Badger): an explicit "core emits ViewModel, shells render" framework, used
  by Proton (Rust Nation 2026). It shows the pattern even if we don't use the crate
  ([crux](https://github.com/redbadger/crux)).
- gitui is TUI-only and isn't a useful example here.

---

## 4. A daemon-served web UI without an npm build step

Options, best fit first:

1. **Server-rendered hypermedia over SSE.** The daemon (axum 0.8) renders `moa-view`
   into HTML fragments (askama or maud) and **pushes them over Server-Sent Events**.
   - **Datastar 1.0** (released 16 April, most likely 2026; ~11 KB, one `<script>`; Rust SDK `datastar` 0.4.1
     with axum integration) is built exactly for "server pushes DOM patches and signals
     over SSE" ([data-star.dev SDKs](https://data-star.dev/reference/sdks)).
   - **htmx 4.0** (2026-08-28) with its SSE extension is the more conservative choice
     ([htmx 4 announcement](https://four.htmx.org/announcements/2026-08-28-htmx-4.0.0-is-released)).
   - Either one is a **single vendored JS file** committed to the repo or embedded with
     `include_bytes!`. No package manager at all.
   - Keyboard-first handling is a few dozen lines of plain ES-module JS (`<script
     type="module">`, importmaps if needed) that maps keys to `Action`s posted back to
     the daemon.
   - Markdown renders server-side with `pulldown-cmark`, and buttons and cards are
     `<button data-on-click=…>`. Animations ("alive", pulsing "waiting on you") are CSS.
2. **Rust→WASM SPA** (Leptos 0.8.23 CSR with **trunk** 0.22, which is in nixpkgs; or
   Dioxus web through `dx`). There's no npm, and a release build is about 170 KB of WASM
   (44 KB gzipped) in one measurement
   ([Leptos CSR deploy](https://book.leptos.dev/deployment/csr.html)). It adds a
   `wasm32` target, wasm-bindgen version pinning and a second build to the Nix flake, and
   it duplicates state on the client. It's only worth it if the UI grows a lot of
   client-side interaction.
3. **Terminals in the browser.** Vendor **xterm.js** (MIT; the `@xterm/xterm` dist files)
   or **ghostty-web** as static files. Pin them by hash with Nix `fetchurl` from the
   registry tarball, or commit them. That avoids the npm client and the proxy's
   quarantine of new versions (an old, pinned version isn't affected anyway). The
   alternative needs no JS terminal library at all: the daemon's per-pane emulator sends
   **row diffs** and the browser renders `<pre>` rows of `<span>`s. That is enough for
   previews, and maybe for interaction.

Security: bind to `127.0.0.1` or a Unix socket, use a per-boot token in the URL or a
cookie (Zellij uses user-generated tokens), check `Origin`, and don't offer remote access
until there's an explicit design for it. Tailscale is the obvious next step.

---

## 5. Recommendation

### Pick: the web UI first, then a thin Tauri 2 window (or a bare `wry` shell) for desktop users

Reasons:
- The constitution **already requires** a daemon-served web UI. Making it *the* GUI means
  one UI codebase, not two.
- Server-rendered `moa-view` fragments over SSE (Datastar or htmx, vendored) mean **no npm
  and no build step**. Nearly all logic lives in Rust and is shared with the TUI.
- Everything the GUI needs is easiest in HTML/CSS: Markdown chat, clickable command
  buttons, yes/later/no cards, a drag-and-drop board, a nested tree with live glyphs, and
  playful animation. WebKit also brings the best accessibility and text rendering of any
  option here.
- The "native window" costs little: Tauri 2 (`cargo-tauri` is in nixpkgs, vanilla
  template, `withGlobalTauri`, no Node or V8 at runtime) loads the daemon's URL. It adds a
  menu bar, a global "summon moa" hotkey, native notifications for "waiting on you", a
  dock badge, and a tray. It works on Linux too (WebKitGTK).
- Live terminals arrive in stages (§2). xterm.js and ghostty-web are proven in browsers
  (VS Code, Zellij, Coder).

Risks:
- "Native" purists may see a webview as non-native. We need CSS discipline to feel like a
  Mac app: system font, vibrancy and native menus through Tauri.
- WebKitGTK on Linux is slower and quirkier than WKWebView. It's acceptable, since Linux
  is a nice-to-have.
- Vendored JS (Datastar/htmx/xterm.js) needs a manual update policy, and we must check the
  licence of every vendored file.
- An SSE + HTML-patch architecture puts fragment rendering on the daemon. Keep it in the
  client-facing layer, not the core, so a slow browser can't stall the daemon.
- Embedded interactive terminals stay the hardest part whichever toolkit we pick: fidelity,
  keys, and tmux size conflicts.

### Runner-up: a native GPUI app (`gpui-kit`/`gpui-pre` + `gpui-component` + `alacritty_terminal` or `libghostty-vt`)

When it would win: the user insists on "no web view", or the terminal cockpit becomes the
main product. GPUI is keyboard-first by design (actions and keymaps), Zed-grade polished,
and has proven terminal embedding (Zed, Sheprd for herdr, Paneflow). Sheprd is almost
exactly the moa architecture: a daemon that owns the terminals and a GPUI client that
streams cells.

Risks:
- Upstream doesn't publish to crates.io. We'd depend on Longbridge's snapshots, with
  frequent breaking changes and pinned toolchains (Zed pins Rust 1.95).
- On Nix/darwin we must use `runtime_shaders` (or Xcode's `metal`).
- Accessibility is unverified upstream.
- Binaries are 100+ MB, and a frictionless macOS install needs a paid Developer ID.
- It would be a second UI codebase next to the web UI the constitution already requires.

Mitigation if we go this way later: because all logic lives in `moa-view`, a GPUI client
is "just another renderer" and can be added without touching the daemon.

### Not recommended now

- **SwiftUI/AppKit**: best feel, but a second language, macOS only, and Xcode-bound
  builds outside Nix. Revisit only if macOS polish becomes the priority. libghostty's
  surface API (what cmux uses) isn't stable yet, so SwiftTerm would be the safe terminal.
- **iced**: no accessibility, and a weaker Mac look.
- **egui**: a tool look, and a stale terminal widget.
- **Slint**: the licence needs attribution, a DSL, and no terminal widget.
- **Dioxus Native/Blitz, Makepad, Xilem**: too early or too quiet.
- **cxx-qt**: a heavy toolchain.

---

## Sources (beyond the inline links)

- crates.io API, queried 2026-11-15, for every version, date and dl90 figure:
  `https://crates.io/api/v1/crates/<name>`.
- iced 0.14 coverage: [Phoronix via scour](https://scour.ing/p/https://www.phoronix.com/news/Iced-0.14-Rust-GUI-LIbrary), [docs.rs iced 0.14.0](https://docs.rs/crate/iced/latest), [iced_widget markdown](https://docs.rs/crate/iced_widget/latest/source/src/markdown.rs).
- egui changelog: [egui 0.34.0 release](https://newreleases.io/project/github/emilk/egui/release/0.34.0).
- GPUI accessibility (forks): [rgpui a11y source](https://docs.rs/crate/rgpui/0.2.2/source/src/_accessibility.rs).
- libghostty 1.0 coverage: [AlternativeTo](https://alternativeto.net/news/2026/11/libghostty-1-0-brings-a-stable-c-abi-for-terminals/), [RuntimeWire](https://runtimewire.com/article/libghostty-1-0-mitchell-hashimoto-stable-c-abi).
- tmux control-mode crates: [tmuxctl](https://docs.rs/crate/tmuxctl/0.1.0), [tmux_interface](https://docs.rs/tmux_interface).
- Ghostty tmux control-mode status: [Ghostty vs iTerm2 notes](https://tslateman.github.io/tutor/how/terminal-emulators).
- cmux: [manaflow-ai/cmux](https://github.com/manaflow-ai/cmux), [contributing (GhosttyKit build)](https://www.mintlify.com/manaflow-ai/cmux/resources/contributing).
- herdr app launch: [zeli.app, 2026-10-30](https://zeli.app/story/50298436); herdr TUI uses libghostty-vt: [flaviocopes deep dive](https://flaviocopes.com/herdr.md).
- UniFFI/Swift: [UniFFI Swift + Xcode](https://mozilla.github.io/uniffi-rs/latest/swift/xcode.html), [cargo-swift](https://github.com/antoniusnaumann/cargo-swift).
- Tauri no-Node CLI: [Tauri v2 docs index](https://v2.tauri.app/llms.txt).
- Leptos binary size: [Leptos book](https://book.leptos.dev/deployment/binary_size.html), [issoh.co.jp measurement](https://www.issoh.co.jp/md/tech/9089).
- Datastar releases: [GitHub releases](https://github.com/starfederation/datastar/releases).
- aoe web dashboard: [Better Stack guide](https://betterstack.com/community/guides/ai/agent-of-empires/).
- Local: `tmux -V` → 3.8; `man tmux` (refresh-client `-C @win:WxH`, `-A pane:on|off|pause|continue`).

Not verified: the exact Ghostty CLI flags for opening a command in a new macOS window;
`app.withGlobalTauri` on Tauri 2.12; upstream GPUI accessibility status; Datastar 1.0's
exact year (16 April, most likely 2026); the Slint pricing page and licence text, which
disagree on whether attribution is required.
