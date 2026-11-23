# moa v1 slice 6b: the console

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The cockpit's `◆ moa` panel becomes the console: it shows moa's conversation as it
happens (Markdown, tool lines, turns, session events), takes your prompts and `/commands`,
turns commands in moa's text into buttons, and never breaks on an entry it can't draw. A
recorded real-adapter session replays in CI.

**Architecture:**
- **The daemon stays the only one that speaks to moa's agent.** The console is a panel like
  the others: it subscribes to the `feed` topic, sends `console.prompt|cancel|command|page|
  new`, and draws. Closing it touches nothing.
- **Three small daemon additions** feed it: session event lines (in memory, on the `feed`
  topic), the button bar (stored in `meta`, on the `feed` topic), and `console.command`, which
  parses a `/command` with `moactl`'s own grammar and runs it as that op. No LLM turn.
- **moa-view stays pure** (no IO, no ratatui): a Markdown renderer into `Line`s, the console's
  line builder, buttons as clickable spans, and the input line. moa-tui only wires keys,
  clicks and the topic in.
- **The replay smoke test** uses three testkit pieces: `acp-tap` (a recording proxy, run by
  hand), `acp-scrub` (cleans a recording before commit), and `stub-acp --replay` (plays it back
  in CI).

```
 feed topic: #<id> rows, state, bar, ev:<seq> ─► Data.feed/moa/bar/events ─► views::console
 keys ─► console input ─► Enter: "/…" ─► console.command ─► (daemon) moactl grammar ─► Op
                                  else ─► console.prompt          card ◄─ reply text
 Esc while thinking ─► console.cancel     PgUp at the top ─► console.page {before}
 button (Enter/click) ─► console.command "<its command>"
```

**Tech Stack:** Rust; the existing crates. New: `pulldown-cmark` (the newest release on the day
Task 5 starts, pinned exactly, `default-features = false`) in moa-view; `shlex` (`=2.0.1`,
already in `Cargo.lock`) in moa-daemon.

**Spec:** `docs/superpowers/specs/2026-11-22-moa-slice-6-central-agent-design.md` ("6b" and the
"My calls" bullets naming 6b: the recording, presence, session event lines), which the user
approved. It builds on the main spec `docs/superpowers/specs/2026-11-15-moa-rust-design.md`
§6.11 ("The console view") and §6.4 (`console.*`, topic `feed`, paged), §11.6–11.7. The spec
wins where they differ.

Roadmap: "Slice 6", item 3. Builds on 6a (#85–#87).

## Builds on (names to use)

- **The feed topic** (`moa_core::publish`): `FeedRow { id, thread, at, kind, message_id,
  automatic, body, state }` keyed `#<id>`, `MoaRow { phase, thread, why }` keyed `state`,
  `FEED_SHOWN = 100`, `feed_rows` (publish.rs ~394), `State::feed_tail`. Kinds in
  `central::feed::kind` (`YOU MOA THOUGHT TOOL PLAN SYSTEM ERROR`), `text_body`/`body_text`.
  Tool body `{title, kind, input}`, state = its status; plan body `{entries: [{content, status,
  priority}]}`.
- **Ops:** `Op::ConsolePrompt { text, wait }`, `Op::ConsoleCancel` (op.rs ~252), `Op::writes`,
  `Op::name`. `Input::Client { conn, req, caller, op }` (effect.rs). The verb grammar:
  `moa_proto::cli::Verb` (clap `Subcommand`) and `Verb::to_op(&VerbCtx)`.
- **Threads:** `central::threads::choose`, `MoaThread { kind: Daily | … }`.
- **Hooks:** `hooks::apply` (hooks.rs 143), whose `After::Apply` branch has the session before
  and after; `live::display_state`, `DisplayState`.
- **moa-view** (see each task): `data.rs` (`TOPICS`, `Data`, `replace`/`put`/`delete`, the
  `Topic::Feed` no-op arms at ~148/191/217), `line.rs` (`Tone`, `Span`, `Item`, `Line`,
  `debug_lines`), `text.rs` (`clean`, `width`, `cut`, `wrap`, `ellipsize`, `SPIN`), `list.rs`
  (`ListState`), `editor.rs` (`Editor`), `keys.rs` (`Key`, `actions`), `views/mod.rs`
  (`build`, the `View::Console` placeholder at ~51, `spinner`), `panel/mod.rs` (`Panel`,
  `Panel::key` with the Picker's early branch ~461, `Panel::mouse` ~479, `Out::Send`,
  `replied`, `note`, `event`), `geometry.rs`, `fixture.rs` (`lantern()`).
- **moa-tui:** `run.rs` (`topics()` 161, `Loop::take` 823, `act` 673, `key_of`/`mouse_of`,
  the 100 ms tick only while something is animated), `draw.rs`, `theme.rs`,
  `tests/render.rs` (insta over `TestBackend`, `WIDTHS = [40, 60, 80, 120, 200]`, `VIEWS`).
- **moa-testkit:** `stub-acp` and its `StubStep`s, `stub_moa_toml()`,
  `moa_toml_with_adapter(&Path)`; `crates/moa-daemon/tests/moa.rs`'s rig;
  `crates/moa/tests/moa_e2e.rs`.

## Calls for you to check

The most important come first.

1. **Commands run as `moactl` verbs.** `/ls`, `/brief`, `/attention`, `/open`, `/resume`,
   `/focus`, `/term`, `/layout`, `/close`, `/archive`, `/suggestions` and the hidden
   `/suggestion yes|no|later N` are parsed by the daemon with `moactl`'s own grammar and run as
   those ops. So a command and the same `moactl` line can never disagree. `/help` lists the
   commands, `/new` starts a new thread, `/bar` sets the button bar, and `/quit` closes the
   console's pane. `/auto` and `/sleep`
   come with 6c's wake-ups, and `/setup` with slice 8. Until then they say
   `/auto comes in a later version of moa` (B4).
2. **A command's answer is a card in your console only.** It isn't saved in `feed`, and moa
   doesn't see it. A restart of the panel clears it. Prompts and moa's answers are saved as
   before (B4).
3. **The button bar:** moa sets it with `moactl bar /brief /ls …`, and you override it with
   `/bar /brief …`. Your list wins until you clear it with a bare `/bar`. The default is
   `/brief /attention /ls /help`. It can hold up to 8 commands, each a valid command of up to
   40 characters (B5).
4. **Session event lines** (`▶ started`, `○ ended`, `⚠ needs you`, `● finished a turn`):
   - **Where they live:** the daemon keeps the last 200 in memory and puts them on the `feed`
     topic. They aren't saved, so a restart starts the list empty. The tree still shows each
     session's state.
   - **How the console folds them:** one line per session, placed at its newest event, so it
     moves to the bottom. For example `● Lantern · 3 turns since 21:10 ↩ open`.
   - **Turn text:** the session's last line shows while auto is off, which in 6b is always
     (B3).
5. **The input line is one line.** It has a cursor and a history: Up and Down step through
   your earlier prompts, taken from the feed. A paste's newlines become spaces. Enter sends a
   prompt, or a command when the line starts with `/` (B7).
6. **Keys in the console:** everything you type goes to the input.
   - **Esc:** cancels moa's turn while it's thinking, and otherwise clears the input.
   - **When the input is empty:** Tab and BTab hop between buttons, and Enter presses one.
   - **Scrolling:** PgUp, PgDn and the wheel scroll.
   - **The bottom:** at the bottom the view follows new rows. Scrolled up, it stays where
     you are and shows `↓ N new`.

   Ctrl-a still reaches tmux (B8).
7. **Older rows:** PgUp past the oldest row loaded asks the daemon for 100 more, through
   `console.page`. A thread change shows one dim separator, `── Mon 23 Nov · thread 0193ab12
   ──` (B2).
8. **The frame:** `◆ moa · BRAIN`, plus a head line with the thread and moa's state:
   connecting, `○ off` ("type to start moa"), starting, `⠋ thinking… Esc cancels`,
   `● ready`, or `✗ failed: why`. `◷ N for moa` waits for 6c and slice 7, which give it
   something to count (B9).
9. **Errors:** the same error text shows at most once per 30 s. An entry that can't be drawn
   shows as one dim line, `· an entry moa couldn't show (#<id>)`. A property test feeds
   random bodies to the builder (B10).
10. **The recording:** you make it, or I do with your go-ahead for that one run, in Task 13.
    It runs against a scratch brain and is scrubbed by `acp-scrub` before it's committed. A
    flake check fails when the recording's adapter version isn't the packaged one, so a bump
    forces a new recording (B11).

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged. Each gets:
- `scripts/verify`;
- per-task reviews;
- one `/code-review medium --fix` over its branch;
- green CI.

Bodies end with the try-it, when there is one.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-6b-1` | 1–4 | `v1 slice 6b (1): the console's daemon side` |
| p2 | `v1/slice-6b-2` | 5–7 | `v1 slice 6b (2): Markdown, the console's lines, buttons` |
| p3 | `v1/slice-6b-3` | 8–10 | `v1 slice 6b (3): typing to moa` |
| p4 | `v1/slice-6b-4` | 11–13 | `v1 slice 6b (4): the replay smoke test, end to end` |

p1 also carries this plan.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| B1 | **Presence:** `central::console_attached(state) -> bool` = some registered panel (`ui.panel`) shows `View::Console`. Nothing uses it in 6b; 6c reads it. | The design's "a console is attached" (6b), with one owner. |
| B2 | **Paging:** `Op::ConsolePage { before: i64, #[serde(default = "page_n")] n: u16 }` (`console.page`, a read; `n` 1–200, default 100) → `Done.data = {"rows": [FeedRow…]}`, the `n` rows with `id < before`, oldest first, from every thread; text `N older rows`. The store reads them (a new `Store` read, not the state). The console keeps paged rows in the panel (`ConsoleUi.older`) and merges them with the topic's tail by id, so an overlap never shows twice. A thread change between two shown rows draws one `Dim` separator `── <Ddd DD Mon> · thread <first 8 of the id> ──`. | Spec §6.4 "feed (console, paged)"; the topic stays 100 rows. |
| B3 | **Session events:** `State.session_events: VecDeque<SessionEvent>` (cap `EVENTS_KEPT = 200`), `SessionEvent { seq: u64, at: UtcMs, session: SessionId, what: EventWhat }`, `EventWhat = Started \| Ended \| NeedsYou \| Finished \| Asleep`, pushed by `hooks::apply`'s `After::Apply` from the display state before → after (→ `Working` from none/`Ended`: `Started`; → `Ended`: `Ended`; → `Waiting`/`Question`: `NeedsYou`; `Working` → `Done`/`Idle`: `Finished`; → `Asleep`: `Asleep`), and by `hooks::asleep`. Published on `feed` as `ev:<seq>` rows (`publish::EventRow { seq, at, session, what }`, `what` in snake case). Not persisted. The console folds them: one line per session at its newest event, `▶ <name> started`, `○ <name> ended` (none when its newest is `Asleep`), `⚠ <name> needs you ↩ answer`, `● <name> · N turns since HH:MM ↩ open` (N = its `Finished` events since its last `Started`/`NeedsYou`, HH:MM the first of them), with the session's last line (`live::last_line` of its stored text) under it, dim, cut to one line. `↩ answer` and `↩ open` are buttons for `/focus <id>`. | The design: "the daemon writes them, from the session events it already handles"; the console folds them. In memory keeps the 100-row feed tail for moa's conversation. |
| B4 | **Commands:** `Op::ConsoleCommand { line: String }` (`console.command`). The daemon's server (where a request becomes `Input::Client`) rewrites it before the core sees it: trim; it must start with `/`; split with `shlex`; `/help` → the command list (text, no op); `/quit` never reaches the daemon: the console closes its own pane, as the panel's close does; `/new` → `Op::ConsoleNew(NoParams)`; `/auto`, `/sleep`, `/setup` → refused `/<name> comes in a later version of moa`; `/bar …` → `Op::BarSet { commands, user: true }`; `/suggestions` → `moactl suggestion ls`; everything else → `Verb::try_parse_from(["moactl", name, args…])` then `to_op(&VerbCtx { cwd: None, home })`, refused unless the verb is one of `ls brief attention open resume focus term layout close archive suggestion`. Clap's error becomes the refusal's first line. The reply is the inner op's reply. `Op::ConsoleNew` (`console.new`, writes): the next prompt opens a new thread of kind `Manual`, even the same day; reply `the next prompt starts a new thread`. The console draws each reply as a card (B6) in its own memory. | Spec §6.11's command list; "console commands run in the daemon with no LLM turn". One grammar for `moactl` and the console. |
| B5 | **The button bar:** `Op::BarSet { commands: Vec<String>, #[serde(default)] user: bool }` (`bar.set`, writes), verb `moactl bar [COMMAND…]` (`user: false`). Each command is a `/command` line B4 accepts (checked by the same parser, without running it), ≤ 40 characters; ≤ 8 of them; else refused with the first bad one. Stored in `meta` as JSON: `console.bar.moa` / `console.bar.you`; an empty list removes the key. Published on `feed` as row `bar`: `publish::BarRow { commands, by: "you" \| "moa" \| "default" }`, you over moa over the default `["/brief", "/attention", "/ls", "/help"]`. The console draws it under the input line as buttons. `bar` joins the reserved brain names. | The user's call (2026-11-18): a runtime list moa sets and you override. |
| B6 | **What the console draws** (`views::console`), in id/time order, newest at the bottom: `you`: `› ` + the text (You tone, wrapped with a 2-column hanging indent). A moa turn (rows after a `you` row, or from an automatic prompt): a header `◆ moa  HH:MM` (Brain tone, bold; `· on its own` when the turn's prompt was automatic), then every row with a 2-column gutter `│ `: `moa` as Markdown (B12); `thought` dim, its first line + ` …`; `tool` `⚙ <title>: <command>` (command = `input.command` when it's a string, else nothing) then `✓` completed, `✗` failed, `⊘` cancelled, or the spinner (animated) while pending/in progress; a run of more than 3 tool rows shows `⚙ N more tool calls` (dim) then its last 3; `plan` one line per entry `☐`/`◐`/`☑` by status; `system` `· text` (Dim); `error` `✗ text` (Fail). Session event lines (B3) at their time. Command cards: `▸ /<line>` (Dim) then the reply text, Markdown-rendered in a box gutter `┊ `. Everything goes through `text::clean`. | Spec §6.11's console view list. |
| B7 | **The input line:** `Editor` gains a cursor (`at`, a grapheme index): Left/Right/Home/End move, Backspace deletes before it, Char and paste insert at it; the footer prompts keep working (their cursor is at the end). `ConsoleUi.history: Vec<String>` = the `you` rows' texts (oldest first) plus prompts sent from this panel; Up/Down step through it, keeping the draft. Enter with text: starts with `/` → `Op::ConsoleCommand { line }`; else `Op::ConsolePrompt { text, wait: false }`; the input clears when the send is accepted, and stays (with the refusal in the footer) when it's refused. Pasted newlines become spaces. | One line is enough for 6b; spec: "input line with cursor and history". |
| B8 | **Keys:** the console gets its own early branch in `Panel::key` (like the Picker's), so letters never become list moves. Ctrl-c interrupts as everywhere. Esc: thinking → `Op::ConsoleCancel`; else clears the input. Empty input: Tab/BTab move `ConsoleUi.focus` over the visible buttons (wrapping), Enter presses it. PgUp/PgDn page; at the top, PgUp sends `console.page { before: <oldest loaded id> }` once (no second request while one is out; none after a page comes back short). The wheel scrolls 3 lines. **Following:** at the bottom the view follows new lines; scrolled up it stays and the head line shows `↓ N new` (N = lines added below). The top line shows `↑ N more` when lines are above the view. | Spec: "Tab/BTab hop between buttons when the input is empty, Enter presses, click presses"; "PgUp/PgDn, wheel, ↑ N more". |
| B9 | **The frame:** title `◆ moa · <brain>`; one head line (geometry's `head`, as the Picker has): `thread <8> · <state>`, state from `MoaRow.phase`: link down → `connecting…`; `off` → `○ off · type to start moa`; `starting` → `◌ starting`; `thinking` → spinner + ` thinking… Esc cancels` (animated); `ready` → `● ready`; `failed` → `✗ <why>` (Fail). Then the body (feed), then the input line (cursor shown), then the bar. | Spec's frame list, minus `◷ N for moa` (nothing to count before 6c/7). |
| B10 | **Errors and degraded entries:** `CoreEvent::Error` texts are shown in the console as `✗` lines (panel memory), the same text at most once per 30 s (`ConsoleUi.shown: HashMap<String, UtcMs>`); the footer keeps working as now. `views::console` never panics: a row whose body isn't what its kind needs draws `· an entry moa couldn't show (#<id>)` (Dim). | Spec: "errors render once per message per 30 s; a failing entry is drawn degraded". |
| B11 | **The recording:** `acp-tap` (testkit bin) runs the program in `MOA_ACP_TAP_TARGET` with its own args, forwards stdin/stdout byte-for-byte, and appends each JSON-RPC line as `{"dir": "in"\|"out", "msg": …}` to `MOA_ACP_TAP_FILE`; stderr passes through. `acp-scrub <in> <out> --replace FROM=TO… --deny REGEX…`: replaces every `FROM`, then the home dir, the user name and the host name it finds in the file's paths, with `/home/user`, `user`, `host`; then fails (exit 1, naming the line) if any `--deny` pattern or an email address remains. The committed file starts with `{"adapter": "claude-agent-acp", "version": "<v>"}`. `stub-acp --replay <file>`: answers each request from the client with the next recorded response to the same method (ids rewritten), and sends the recorded agent messages between them in order. A flake check, `checks.<system>.recording`, fails when the header's version isn't `claude-agent-acp.version`. | Spec §11.6 and the design: a manual, scrubbed recording; CI only replays it. |
| B12 | **Markdown** (`moa_view::markdown::render`, Task 5's signature), with pulldown-cmark's tables, task lists and strikethrough: headings `▌ ` + bold (H1 Head tone, others bold); bullets `•`/`◦`/`▪` by depth, numbered `N.`, task boxes `☑`/`☐`, all with hanging indents; quotes `│ ` (Dim); fenced code (Dim, each line cut at the width, never wrapped, no language line); rules `─` across the width; tables box-drawn (`┌┬┐│├┼┤└┴┘─`) when every column fits the width, else one bullet per row `• a · b · c`; bold; strikethrough as plain; inline code (Work tone); links as their text only; images as `[image]`; HTML as its text. Coloured glyphs: `✓` Ok, `✗` Fail, `⚠` Warn, `●` Work. Words wrap by display width; a button (inline code matching B13) is one unbreakable unit, moved whole to the next line, and cut with `…` only when wider than the whole width. | Spec §6.11's Markdown list. |
| B13 | **Buttons:** `Span.button: Option<u16>` (an index into the view's `Vec<Button>`), `Button { label: String, command: String }`. In moa's text, inline code that is a `/command` B4 knows becomes `[<that text>]`; `moactl resume\|focus\|open\|close\|archive X` (no flags) becomes `[↩ <name>]` for session `X` (`Project · title`, or the title, or `X`, `text::ellipsize`d to 40 columns), refreshed from `Data.sessions` on every build, and presses `/<verb> X`; anything else stays inline code. A click on a button's cells presses it (`row_at` plus the span's columns); a click elsewhere does what it does now. The focused button is drawn reversed. | Spec: "buttons: backticked /commands and moactl resume\|focus\|open\|close\|archive X; session buttons show the session's name". |

## Global Constraints

The roadmap's constraints apply, as do every earlier slice plan's and 6a's. Also:

- **Reuse before writing.** Search the workspace before adding a helper. A near-copy is an
  **Important** finding, and each report lists what it reused.
- **Building and verifying:**
  - cargo only through `nix develop --command cargo …`, with `cargo nextest run`;
  - `scripts/verify quick` before every commit, and the full `scripts/verify` once per PR;
  - never run cargo while an e2e test runs.
- **moa-view stays pure:** no IO, no ratatui and no crossterm. Every builder is
  `(data, ctx, ui, width) → lines`.
- **No line is wider than the width it was built for,** at every width from 40 to 200. A
  proptest per new builder checks this.
- **Every string from data goes through `text::clean`** before it's measured or drawn.
- **No polling** (spec U5). The spinner animates only while a spinner line is visible (the
  existing `Line.animated`). Nothing new ticks, and paging happens only on a key.
- **Pinned exactly:** `pulldown-cmark = "=<x.y.z>"` and `shlex = "=2.0.1"`. `Cargo.lock` is
  committed.
- **Tests:**
  - only under `/tmp` roots;
  - never the real `claude`, the real adapter, a tmux server named `moa`/`moa-work`, or a real
    brain;
  - every wait goes through `wait_for` with a 10 s limit;
  - console snapshots use `fixture::lantern()` plus feed rows made up in the fixture;
  - `acp-tap` is never run by a test against anything but `stub-acp`.
- **Public repo:**
  - fixtures and the synthetic recording use Lantern, Kite, Harbor, Beacon, `acct-a` and
    `~/private`;
  - the real recording is committed only after `acp-scrub` passes;
  - its `--deny` list (names, hosts, accounts) is given by hand and never committed.
- **Log lines never hold record contents.**
- **Plain words** in everything the user sees: "moa", "moa's agent", "thread". Never "ACP",
  "adapter" or "session/update".

## Review Focus

1. **Odd text in moa's answers:**
   - **The inputs:** a table wider than the screen, lists nested 8 deep, an unterminated code
     fence, a 10,000-line answer, ZWJ and RTL text, and a session name with a wide glyph inside
     a button. All at width 40.
   - **Expected:** no panic, and no line wider than the width. A button is never split. A
     table that doesn't fit becomes bullets.
   - **Pinned by:** `markdown_never_overflows` (proptest), `a_wide_table_becomes_bullets`,
     `buttons_never_split` (Task 5), and `odd_text_in_buttons` (Task 7).
2. **A long feed while you read:**
   - **The inputs:** 1,000 rows, scrolled up, while moa answers; paging that overlaps the
     topic's tail; paging past the first row ever.
   - **Expected:** the view doesn't move and says `↓ N new`. No row shows twice. A short page
     stops further requests.
   - **Pinned by:** `scrolled_up_stays_put` and `paging_never_duplicates` (Task 8), and
     `the_first_row_stops_paging` (Task 8).
3. **Typing while things change:**
   - **The inputs:** Enter while moa is thinking; the link going down mid-draft; Esc while
     idle.
   - **Expected:** the refusal shows in the footer and the draft stays. While the link is down
     the draft stays and the head line says `connecting…`. Esc clears the input.
   - **Pinned by:** `a_refused_prompt_keeps_the_draft`, `the_link_going_down_keeps_the_draft`
     and `esc_clears_when_idle` (Task 8).
4. **Errors that repeat, and entries that can't be drawn:**
   - **The inputs:** the same error 100 times in 30 s; a `tool` row whose body is a string; a
     `plan` row with no entries; an unknown kind.
   - **Expected:** the error shows once, and each bad row is one dim line.
   - **Pinned by:** `an_error_shows_once_per_30_s` (Task 8), and `bad_rows_draw_degraded` plus
     the `console_never_panics` proptest (Task 6).
5. **Buttons whose session changed:**
   - **The inputs:** `moactl focus X` where X was renamed, archived, or never existed.
   - **Expected:** a rename shows the new name on the next build. An unknown id shows as the
     id, and pressing it shows the daemon's refusal.
   - **Pinned by:** `session_buttons_follow_renames` and `an_unknown_session_button_says_why`
     (Task 7).

---

## Task 1: paging, `/new`, presence

**Files:** `crates/moa-core/src/op.rs` (`ConsolePage`, `ConsoleNew`), `central/mod.rs`
(`console_attached`, the new-thread flag), `central/threads.rs` (`choose` honours it, kind
`Manual`), `core.rs` (the ops); `crates/moa-store` (the page read); tests in
`central/threads.rs`'s tests, `crates/moa-store/tests/`, `crates/moa-daemon/tests/moa.rs`.

**Interfaces:**
- Produces: `Op::ConsolePage { before, n }` → `{"rows": [FeedRow]}`; `Op::ConsoleNew`;
  `central::console_attached(&State) -> bool`.

- [ ] **Step 1: Failing tests:**
  - `a_page_is_the_rows_before_oldest_first`: 250 rows; `before = 201, n = 100` gives ids
    101–200 in order; `n = 0` and `n = 201` are refused (`n is 1 to 200`).
  - `a_page_spans_threads`: rows from two threads come back together, by id.
  - `new_starts_a_thread_the_same_day`: after `console.new`, the next prompt sends a new
    thread (`ThreadHow::New` with the system prompt), `moa_threads` gains a `Manual` row, and
    the prompt after that uses it.
  - `console_attached_follows_panels`: false; then a `ui.panel` with view `console` makes it
    true; then that connection's close makes it false.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B1, B2's op and B4's `ConsoleNew`.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: console.page, console.new, and whether a console is attached`.

## Task 2: session event lines

**Files:** `crates/moa-core/src/hooks.rs`, `state.rs` (`session_events`), `publish.rs`
(`EventRow`, `ev:` rows in `feed_rows`); tests in `hooks`' and `publish`'s tests.

**Interfaces:**
- Produces: `SessionEvent`, `EventWhat`, `publish::EventRow`, `EVENTS_KEPT = 200`.

- [ ] **Step 1: Failing tests:**
  - `each_change_makes_one_event`: the hook flows `start → working → stop → waiting →
    session_end` give `Started, Finished, NeedsYou, Ended` in order, with increasing `seq`.
  - `a_duplicate_hook_makes_no_event`.
  - `events_ride_the_feed_topic`: the topic has `ev:<seq>` rows beside `#<id>` and `state`.
  - `only_200_are_kept`: the 201st push drops the oldest, and its row is removed from the
    topic.
  - `going_to_sleep_is_asleep`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B3's daemon side.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: session events on the feed topic`.

## Task 3: `console.command`

**Files:** `crates/moa-core/src/op.rs` (`ConsoleCommand`), `crates/moa-daemon/src/server.rs`
(the rewrite, B4), `crates/moa-daemon/Cargo.toml` (`shlex`); tests in a new
`crates/moa-daemon/tests/console.rs` (in-process daemon, as `tests/moa.rs` does).

**Interfaces:**
- Consumes: `Verb`, `Verb::to_op`, `Op::ConsoleNew` (Task 1).
- Produces: `Op::ConsoleCommand { line }`, and the command list `COMMANDS: &[(&str, &str)]`
  (name and one-line help, in the spec's order) in `moa_core::console`. Task 9's palette and
  Task 7's buttons read it.

- [ ] **Step 1: Failing tests:**
  - `ls_runs_as_moactl_ls`: the reply's text equals `moactl ls`'s, on the same brain.
  - `focus_takes_a_quoted_title`: `/focus "Rust scaffold"`.
  - `help_lists_every_command`.
  - `later_commands_say_so`: `/auto` →
    `/auto comes in a later version of moa`.
  - `unknown_and_bad_lines_are_refused`: `ls` (no slash) →
    `a command starts with /`; `/nope` → `there's no /nope command; /help lists them`;
    `/resume` with no id → clap's first line.
  - `a_command_never_starts_moa`: `spawn::launches()` doesn't change across all of the above.
  - `twenty_verbs_launch_nothing` still passes.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B4.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: console commands run as moactl verbs`.

## Task 4: the button bar

**Files:** `crates/moa-core/src/op.rs` (`BarSet`), `crates/moa-proto/src/cli.rs` (verb `bar`),
`crates/moa-core/src/config.rs` (`bar` reserved), `publish.rs` (`BarRow`), the core's op
handler, `server.rs` (`/bar` → `user: true`); tests beside each.

**Interfaces:**
- Produces: `Op::BarSet { commands, user }`, `publish::BarRow { commands, by }`,
  `DEFAULT_BAR`.

- [ ] **Step 1: Failing tests:**
  - `the_default_bar`: a fresh brain's `bar` row is the default, by `default`.
  - `moa_sets_and_you_override`: `moactl bar /ls` → `by: moa`; `/bar /brief` → `by: you`, then
    `moactl bar /help` changes nothing visible; a bare `/bar` → moa's `/help`.
  - `a_bad_bar_is_refused`: 9 commands; a 41-character one; `/nope`; `ls`.
  - `the_bar_survives_a_restart`.
  - `every_verb_and_subcommand_is_reserved` passes with `bar`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B5.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the console's button bar`.

## Task 5: the Markdown renderer

**Files:** new `crates/moa-view/src/markdown.rs`; `line.rs` (`Span.button`, `Button`);
`text.rs` (span-aware wrapping, if `wrap` can't be extended); `crates/moa-view/Cargo.toml`
(`pulldown-cmark`); tests in `markdown.rs` (insta on `debug_lines`, proptest).

**Interfaces:**
- Produces: `markdown::render(text: &str, width: u16, buttons: &mut Vec<Button>, find:
  &dyn Fn(&str) -> Option<Button>) -> Vec<Line>`. `find` turns an inline code's text into a
  button, or `None`; Task 7 supplies it. `Button { label, command }`, `Span.button:
  Option<u16>`.

- [ ] **Step 1: Failing tests** (`debug_lines` snapshots at widths 40 and 80 unless noted):
  - one snapshot per construct in B12;
  - `hanging_indents`: a wrapped bullet's second line starts under its text;
  - `a_wide_table_becomes_bullets` (width 40), and `a_fitting_table_is_boxed` (width 80);
  - `buttons_never_split`: `find` = every code span is a button; at widths 10–60 every
    button's label is whole on one line;
  - `an_unterminated_fence_is_code_to_the_end`;
  - `markdown_never_overflows`: a proptest over generated Markdown (headings, lists to depth
    8, tables, code, CJK/ZWJ/RTL text) at widths 40–200. No line is wider than the width,
    and there is no panic.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B12** (pin pulldown-cmark exactly; record the version in the report).
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: a Markdown renderer for the console`.

## Task 6: the console's lines

**Files:** `crates/moa-view/src/data.rs` (`Topic::Feed` into `TOPICS`; `Data.feed:
BTreeMap<i64, FeedRow>`, `Data.moa: Option<MoaRow>`, `Data.bar: Option<BarRow>`,
`Data.events: BTreeMap<u64, EventRow>`), new `views/console.rs`, `views/mod.rs` (the
placeholder goes), `fixture.rs` (`console_rows()`: a made-up day with every kind); tests in
`views/console.rs` and `moa-tui/tests/render.rs` (`VIEWS` gains the console).

**Interfaces:**
- Consumes: Task 5's `render`; Tasks 2 and 4's rows.
- Produces: `views::console::build(data, ctx, ui: &ConsoleUi, width) -> ConsoleView {
  lines: Vec<Line>, buttons: Vec<Button> }`. `ConsoleUi { older: BTreeMap<i64, FeedRow>,
  cards: Vec<Card>, errors: Vec<(UtcMs, String)>, … }` in `views/console.rs` (Tasks 7–8 add
  fields). `Card { at, line, text }`. The panel holds it as `Panel.console: ConsoleUi` (reset by `set_view`).

- [ ] **Step 1: Failing tests:**
  - `debug_lines` snapshots of `console_rows()` at widths 40, 80 and 120: turns under
    `◆ moa  HH:MM`, the gutter, `· on its own`, every kind, a 5-tool run folded, the event
    lines folded (B3), a card, and a thread separator;
  - `render.rs`'s pixel snapshots for the console at all `WIDTHS`;
  - `bad_rows_draw_degraded`: a string `tool` body, a `plan` with no entries, and kind
    `nope` each draw one dim line naming the id;
  - `console_never_panics`: a proptest over random kinds and JSON bodies (strings, numbers,
    nested objects) at widths 40–200, with no line wider than the width;
  - `the_thinking_line_animates`: an in-progress tool sets `animated`, and a finished one
    doesn't.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B6, B3's folding, B2's separator and B10's degraded rows.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the console draws moa's conversation`.

## Task 7: buttons

**Files:** `views/console.rs` (`find` for B13, the bar under the input), `panel/mod.rs`
(console click hit-testing by span columns, `ConsoleUi.focus`), `moa-tui/src/draw.rs`
(reversed focused button); tests in `views/console.rs` and `panel`'s tests.

**Interfaces:**
- Consumes: `moa_core::console::COMMANDS` (Task 3), `Data.sessions`.
- Produces: `Panel` presses a button as `Out::Send(Op::ConsoleCommand { line: button.command
  })`.

- [ ] **Step 1: Failing tests:**
  - `commands_in_text_become_buttons`: `` `/brief` `` → `[/brief]`; `` `moactl focus
    0193…` `` → `[↩ Lantern · Rust scaffold]`; `` `moactl ls --all` `` stays code;
  - `session_buttons_follow_renames`;
  - `an_unknown_session_button_says_why`: it shows the id, and a press sends `/focus <id>`
    (the reply's refusal shows in the footer);
  - `odd_text_in_buttons`: a title with ZWJ, RTL text and a wide glyph, at width 40, is cut
    to ≤ 40 columns and isn't split;
  - `a_click_on_a_button_presses_it` and `a_click_beside_it_does_not`;
  - `tab_hops_and_enter_presses`: with the input empty, Tab moves over the buttons (bar
    included), BTab goes back, wrapping at both ends, and Enter sends the focused one.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B13 and B5's bar drawing.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: buttons in the console`.

## Task 8: the input line, keys, the frame

**Files:** `editor.rs` (cursor), `panel/mod.rs` (the console's early branch), new
`panel/console_keys.rs`, `geometry.rs` (the console's head line and input rows),
`views/console.rs` (`↑ N more`, `↓ N new`, following), `moa-tui/src/run.rs` (the feed
topic, `CoreEvent::Error` into the console, paging replies); tests in `panel`'s tests and
`run.rs`'s loop tests.

**Interfaces:**
- Consumes: Tasks 1, 6 and 7.
- Produces: `ConsoleUi.{draft: Editor, history, hist_at, follow: bool, below: usize, paging:
  Option<Tag>, exhausted: bool, shown}`.

- [ ] **Step 1: Failing tests:**
  - `editor_moves_its_cursor`: insert in the middle, Home/End, Backspace before the cursor,
    and a ZWJ grapheme counted as one;
  - `the_footer_prompts_still_work` (the existing editor tests pass unchanged);
  - `enter_sends_a_prompt_or_a_command`, `letters_never_move_the_list`;
  - `up_and_down_walk_the_history`: the history comes from the `you` rows, and the draft
    returns at the end;
  - `esc_cancels_while_thinking`, `esc_clears_when_idle`;
  - `a_refused_prompt_keeps_the_draft`, `the_link_going_down_keeps_the_draft`;
  - `scrolled_up_stays_put` (shows `↓ 3 new`), `at_the_bottom_it_follows`;
  - `paging_never_duplicates`: the topic holds 101–200, and a page of 50–149 merges to
    50–200 once each; `the_first_row_stops_paging`; `one_page_request_at_a_time`;
  - `an_error_shows_once_per_30_s`;
  - the head line's snapshots for each phase, at widths 40 and 80.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B7, B8, B9 and B10's error lines.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: typing to moa in the console`.

## Task 9: the `/` palette

**Files:** `views/console.rs` (the palette lines above the input), `panel/console_keys.rs`;
tests beside them.

**Interfaces:**
- Consumes: `COMMANDS`, `Data.sessions`, `Data.layouts`.

- [ ] **Step 1: Failing tests:**
  - `slash_opens_the_palette`: `/` shows the commands (hidden ones excluded), at most 8
    lines; `/re` narrows them to `/resume`;
  - `after_resume_it_lists_sessions`: `/resume ` lists session titles (live, not archived),
    and the typed text narrows them; it lists layout names after `/layout `;
  - `tab_completes_and_up_down_choose`: Tab replaces the word with the chosen entry; for a
    session, the inserted text is its id, quoted only if needed;
  - `esc_closes_the_palette_first`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement the palette** (spec §6.11: commands, then session titles after
  `/resume|focus|close|archive `, layout names after `/layout `).
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the console's / palette`.

## Task 10: the console in the cockpit

**Files:** `crates/moa/tests/` (a new `console_e2e.rs`, tmux sandbox, as `cockpit_e2e` does);
fixes found by it.

- [ ] **Step 1: Write the tests** (the real binary on a `TempBrain`, a tmux sandbox server,
  and `stub-acp` scripted):
  - `typing_in_the_console_reaches_moa`: send keys `hello` and Enter to the console pane.
    `capture-pane` shows `› hello`, `◆ moa`, and the stub's words.
  - `a_button_runs_its_command`: the stub says `` try `/ls` ``. Tab and Enter bring up a card
    with `moactl ls`'s text.
  - `esc_cancels_a_hang`: a `Hang` turn. Esc makes the head line `● ready` and the feed says
    `cancelled`.
  - `closing_the_console_leaves_the_turn`: kill the console pane mid-`Wait`, release the stub,
    and moa's row is complete in `feed`.

  Each runs 10 times under load, and each is made to fail once on purpose.
- [ ] **Step 2: Run them.** Expected: PASS.
- [ ] **Step 3: Commit.** `v1: the console end to end`.

**Try it** (p3's body): the 6a try-it's sandbox, then `moa --sandbox`. In the `◆ moa`
panel, type `hello`. Then try `/ls`, Tab to the button, and Enter.

## Task 11: tap, scrub, replay

**Files:** new `crates/moa-testkit/src/bin/acp-tap.rs` and `acp-scrub.rs`; `stub-acp.rs`
(`--replay`); a made-up `crates/moa-testkit/recordings/synthetic.jsonl` (written by hand in
the real adapter's shapes, as of the version packaged); tests in `crates/moa-testkit/tests/`.

**Interfaces:**
- Produces: the three tools of B11.

- [ ] **Step 1: Failing tests:**
  - `the_tap_records_both_ways`: `acp-tap` in front of `stub-acp` with a script. The file
    holds each line once, `in` and `out`, in order, and the client sees the same bytes as
    without the tap.
  - `scrub_replaces_and_denies`: a file with a home path, an email and a deny word. The output
    has the placeholders, and the deny case exits 1, naming the line.
  - `replay_answers_in_order`: `stub-acp --replay synthetic.jsonl` against the host gives the
    same `AcpIn` sequence as the recording's agent side.
  - `a_replay_out_of_step_says_so`: a request the recording doesn't have next exits with
    `replay expected session/prompt, got session/load`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement B11's tools.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: record, scrub and replay moa's agent`.

## Task 12: the replay smoke test and the version check

**Files:** `crates/moa-daemon/tests/moa.rs` (`a_recorded_session_replays`), `flake.nix`
(`checks.<system>.recording`); the recording's snapshot.

- [ ] **Step 1: Write `a_recorded_session_replays`** (the daemon with the account's `acp` =
  `stub-acp` and `MOA_STUB_REPLAY=<recording>`; the prompt is the recording's first prompt).
  The feed's rows (kind, body, state; ids and times dropped) match an insta snapshot, and
  nothing is logged at error.
- [ ] **Step 2: Add the flake check** (B11). It passes with the synthetic file's header set to
  the packaged version. It fails, naming both versions, when the header says otherwise; show
  the failure once, then revert.
- [ ] **Step 3: Run `scripts/verify`** (full, so `nix flake check` runs). Expected: PASS.
- [ ] **Step 4: Commit.** `v1: the replay smoke test`.

## Task 13: the real recording (by hand, with your go-ahead)

**Stop and ask before Step 2.** This is the only step that runs the real adapter and a real
account.

- [ ] **Step 1: Prepare.** Build the workspace. Set up a scratch brain under `/tmp`, whose
  `moa.toml` account has `acp` = `acp-tap`, with `MOA_ACP_TAP_TARGET` = the dev shell's
  `claude-agent-acp` and `MOA_ACP_TAP_FILE` set. Write the deny list (names, hosts, accounts)
  to a file outside the repo.
- [ ] **Step 2: Record (the user, or Claude with the user's go-ahead):**
  - `moa --sandbox` on the scratch brain;
  - in the console: `hello`, then a prompt that makes moa run `moactl ls` (allowed), then one
    that makes it ask for a permission, which is refused until 6c;
  - quit.
- [ ] **Step 3: Scrub:** `acp-scrub <tap> crates/moa-testkit/recordings/claude-agent-acp.jsonl
  --replace … --deny …`. Read the result by eye before committing.
- [ ] **Step 4: Point Task 12's test at it** and update its snapshot. Run `scripts/verify`.
- [ ] **Step 5: Commit.** `v1: a recorded session with the real adapter`.

## Carried, and where each goes

- **To 6c:**
  - permission cards and `⚠ answer y/a/n`;
  - `◷ N for moa`;
  - `/auto` and `/sleep`;
  - the console's `moa is reading N session reports…` line;
  - the watchdog's card.
- **To 6d:** suggestion cards (`✦ moa suggests`), and the opening brief.
- **To slice 7:** ask cards (`✉ NAME asks moa`).
- **To slice 8:** `/setup` and the setup card.
- **Multi-line input:** later, if one line proves short.
