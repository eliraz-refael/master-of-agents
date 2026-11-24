# moa v1 slice 6c: permissions, reports, the watchdog

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** moa asks before it acts, and you answer with one key. What your sessions report
reaches moa exactly once, in a wake-up or with your next prompt. A turn that hangs is cancelled,
then restarted. When moa's account fails, a card offers another one. After 6c moa is safe to
leave running.

**Architecture:**
- **Everything new is core logic.** Reports, the one channel, permissions, the watchdog and the
  account card live in `moa_core::central`. Time is an input, as everywhere. Two new timers
  (`Wake`, `Watchdog`) are deadlines the core sets and cancels.
- **Reports are rows.** A new `reports` table holds one row per (session, kind). A hook event
  queues one in the same commit that applies the event. The commit that saves the `you` row
  carrying a report deletes it, before the prompt is sent.
- **One channel.** `central::channel` decides what automatic prompt goes next and when. In 6c
  its only source is the wake-up. Slice 7 adds asks as a second source.
- **A permission request is a feed row** of kind `permission`, updated in place with your
  answer. The core keeps the open ones in memory (`Moa.asking`): they live and die with the
  agent's run. The console draws them as cards and answers them with y/a/n.
- **The daemon and the host barely change.** The host passes each option's name. The acp actor
  already stops the old agent before it starts a new one, which is the watchdog's kill.

```
 hook (turn ended / needs you) ─► hooks::apply ─► Change::Report (same commit)
 end of every input ─► channel::run ─► Wake timer │ due + idle + auto ─► wake prompt
 console.prompt / wake ─► prompt(): commit {you row (+ reports' text), DeleteReport…} ─► send
 AcpIn::Permission ─► `permission` row (pending) + Moa.asking ─► card │ y/a/n ─► console.permission
 no news for 5 min ─► Cancel + card ─► 30 s ─► Start (new generation) ─► reopen thread
 auth/billing failure ─► account card ─► `/account NAME` (you only) ─► next prompt
```

**Tech Stack:** Rust; the existing crates. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-22-moa-slice-6-central-agent-design.md` ("6c", "Must
never happen", and the "My calls" bullets on report queues and presence), which the user
approved. It builds on the main spec `docs/superpowers/specs/2026-11-15-moa-rust-design.md`
§6.11 (watchdog, permissions, C24, hidden prompts, reports and wake-ups, the console's frame
and cards), §6.8 (timers), §6.6 (the status line), §11.1 and §11.6. The main spec wins where
they differ.

Roadmap: "Slice 6", items 4, 5 and 7, plus C24 from item 2. Builds on 6a (#85–#87) and 6b
(#88–#91).

## Builds on (names to use)

- **moa's core** (`crates/moa-core/src/central/`):
  - `mod.rs`: `Moa`, `Phase`, `Pending`, `Turn`, `Waiter`, `send`, `prompt` (the one commit of
    a `you` row, then `AcpCmd::Prompt`), `started`, `open`, `opened`, `exited`, `cancel`,
    `reload`, `console_attached`, `answer`, `said`;
  - `turn.rs`: `update`, `permission` (today's refusal), `ended`, `cut_short`,
    `cancelled_tools`, `new_row`, `record`, `write`;
  - `feed.rs`: `kind::*`, `text_body`, `body_text`, `row`;
  - `wire.rs`: `AcpCmd`, `AcpIn`, `PermOption { id, kind }`, `PermKind`, `StopReason`;
  - `threads.rs`: `choose`, `How`; `launch.rs`: `spec`.
- **The core's frame:** `core::handle` and its end-of-input runners (`sleep::run`,
  `terminals::run`, `status::run`); `Tx::{new, commit}`, `post_commit`; `effect::{TimerKey,
  rearm, Effect::SetTimer}`; `Input::Timer`.
- **Hooks:** `hooks::apply` and `applied_changes` (the `After::Apply` branch, also used by
  `apply_held`); `HookKind::{TurnEnded { last_message }, NeedsPermission { tool }, NeedsAnswer
  { what }}`; `ops::launch::title_of`.
- **State and store:** `Snapshot`, `NextIds`, `Table`, `State::meta`, `Change::Meta`,
  `State::caller` (`Caller::Moa`), `Deps.agent_dir`; the store's `rows.rs` (apply) and
  `snapshot.rs` (load), beside `nags`; `sql/0001_v1.sql` (edited in place until the switch,
  P6).
- **Publishing:** `publish::{MoaRow, FeedRow, feed_rows}`, `State::publish_moa_state`.
- **Config:** `config.rs` (`Config`, `RawConfig`, `TABLES`, `SleepAfter`, `hours_ms`);
  `sleep.rs`'s `threshold`.
- **Status line:** `tmux/status.rs` (`counts_text`, `sync`).
- **Console commands:** `moa_core::console` (`COMMANDS`, `LATER`, `parse_with`, `Parsed`);
  `Op` (`op.rs`: `writes`, `name`, `METHODS`).
- **The host:** `moa_agents::acp::convert::{perm_option, permission}`, `Adapter::answer`; the
  daemon's acp actor stops the last agent before a `Start` (`acp.rs`, `Actor::start`).
- **moa-view:** `views/console.rs` (`Draw::entry`, `Card`, `ConsoleUi`, `head`, the card
  gutter), `panel/console_keys.rs` (`console_key_inner`, `press`), `markdown::render`'s button
  finder (B13).
- **CLI:** `crates/moa/src/console.rs` (`Verb::{Prompt, Cancel, Feed}`).
- **Tests:**
  - core: `tests/common/central.rs` (`moa`, `first_turn`, `hear`, `prompt_at`, `saved`,
    `prompts_sent`, `feed_rows`, `toml`), `tests/central_turns.rs`, the proptest in
    `tests/sleep.rs`;
  - daemon: `tests/common/rig.rs` (`Rig`, `Up`, `FakeTime::advance`, `logged`, `wait_saved`,
    `rows`, `feed`), `hook_line` in `tests/hooks.rs`, `tests/moa.rs`;
  - e2e: `crates/moa/tests/console_e2e.rs` (`Console`), `moa_e2e.rs`;
  - testkit: `StubStep::{Ask, Wait, Hang, HangHard, Die}`, `write_stub_script`,
    `stub_moa_toml`.

## Calls for you to check

The most important come first.

1. **Wakes don't wait for a console (K4).** With auto on, a wake runs once reports settle,
   whether or not a console is attached. That is the spec's rule (auto, idle, no permission
   waiting). The other choice is to wake only while a console is attached, and let reports
   ride on your next prompt otherwise. That saves turns overnight but leaves moa's picture
   stale until you type.
2. **A delivered report is deleted (K2, K3).** It goes in the same commit that saves the `you`
   row carrying it. That row keeps the reports' text, so the feed is the record. A daemon that
   dies between that commit and the send has delivered it: it is never sent twice.
3. **Your prompt carries at most 8 sessions' reports (K3).** They go before your text, and
   the rest wait. Your console line shows `· with N session reports`. The 100,000-character
   limit counts only your own text.
4. **moa's account (K8, K9).**
   - **Detection is by the error's text.** The phrase list includes usage limits, not only
     auth and billing, because a spent limit is when another account helps most.
   - **Switching is `/account NAME`, from the console only.** It is stored in `meta` over
     `moa.toml`'s `moa_account`, so moa.toml is never edited. moa itself is refused.
5. **The watchdog (K6).**
   - **It restarts at once.** It doesn't wait for your next prompt. It reopens today's thread
     with no prompt.
   - **Its two limits are settings too,** so the e2e tests run in seconds.
   - **It pauses** while a permission waits for you.
   - **A late timer re-arms.** One that fires more than 60 s late (the Mac slept) gives the
     turn a fresh 5 minutes.
6. **Permissions (K5).**
   - **The pending table is in memory,** and each request is a `permission` row in the feed.
     After a daemon restart an unanswered one shows `⊘ not answered`.
   - **Several queue, oldest first.**
   - **`n` answers `reject_once`,** falling back to `reject_always`.
7. **Settings live in a new `[moa]` table** of `moa.toml`, for every brain (K1).
8. **`/auto` and `/sleep` are stored in `meta`** and take effect at once (K10).
   - `/auto` is on by default.
   - `/sleep` wins over `sleep_after_hours` until `/sleep on`.
9. **Wakes skip a failed start (K4).** When moa's agent last failed to start, reports ride on
   your next prompt.
10. **The time of the last wake is in memory.** After a daemon restart the first wake may
    come sooner than 180 s after the one before.

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged. Each gets:
- `scripts/verify`;
- per-task reviews;
- one `/code-review medium --fix` over its branch;
- green CI.

Bodies end with the try-it, when there is one.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-6c-1` | 1–3 | `v1 slice 6c (1): session reports reach moa once` |
| p2 | `v1/slice-6c-2` | 4–5 | `v1 slice 6c (2): moa asks, you answer` |
| p3 | `v1/slice-6c-3` | 6–9 | `v1 slice 6c (3): the watchdog, moa's account, the console's new lines` |
| p4 | `v1/slice-6c-4` | 10–11 | `v1 slice 6c (4): the stub scenarios, end to end` |

p1 also carries this plan. p2 keeps the core and the console's card together, so `main` never
has a permission nobody can answer.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| K1 | **Settings:** a `[moa]` table in `moa.toml`, `deny_unknown_fields`, each key checked by hand as `[ui]`'s are. Keys and defaults: `wake_settle_seconds = 45` (0 or more), `wake_gap_seconds = 180` (0 or more), `wake_batch = 8` (1–100), `watchdog_seconds = 300` (1 or more), `watchdog_kill_seconds = 30` (1 or more), whole numbers. `config::MoaConfig { wake_settle_ms: i64, wake_gap_ms: i64, wake_batch: usize, watchdog_ms: i64, watchdog_kill_ms: i64 }` with `Default`, in `Config.moa`; `TABLES` gains `moa`. A bad value is one line, `moa.toml: wake_batch in [moa] must be a whole number from 1 to 100` (each key its own range words). | Spec: "All three are settings." The watchdog's two limits are settings too, so e2e tests needn't wait 5 min. |
| K2 | **Reports:** table `reports (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, kind TEXT NOT NULL, queued_at INTEGER NOT NULL, last_at INTEGER NOT NULL, turns TEXT NOT NULL, UNIQUE (session_id, kind)) STRICT`, `turns` a JSON array of strings. `model::Report { id, session_id, kind: ReportKind, queued_at, last_at, turns }`, `ReportKind = TurnEnded \| NeedsYou` (`turn_ended`, `needs_you`; another kind on load is a `LoadNote`). `Change::Report(Report)`, `Change::DeleteReport(i64)`, topic `Feed`, row key `#<id>`. `hooks::applied_changes` queues one for an applied (not history-only, not duplicate) event: `TurnEnded` with a non-empty trimmed `last_message` → `TurnEnded`, the text cut to `LAST_MAX = 2600` characters, appended to the session's queued row (keeping the last `TURNS_KEPT = 6`); `NeedsPermission { tool }` → `NeedsYou` with `permission: <tool>`; `NeedsAnswer { what }` → `NeedsYou` with `what` (each cut to `LAST_MAX` too); a `NeedsYou` row keeps only its newest text. A new row's `queued_at` is the event's `at`, and a later report keeps it (its place in line); `last_at` moves. None for a session whose `cwd` is moa's own folder (`Deps.agent_dir`) or under it. | Spec §6.11 and the design's call: queues kept in the database. |
| K3 | **The one channel:** `central::channel`, `Source = You \| Wake` (slice 7 adds `Ask`). `Pending.source` and `Turn.source` replace nothing (`automatic` = `source != You`). The text a prompt carries is built in `prompt()`, at the commit, from the queue as it is then: the batch is the queued reports in `(queued_at, id)` order, until `wake_batch` sessions (all of a chosen session's reports go). The one commit holds the `you` row and a `Change::DeleteReport` per carried report; `AcpCmd::Prompt` follows it. **Wake:** row `automatic`, body `{"text": <the wake text>, "reports": N}`. **Your prompt:** body `{"text": <your text>, "reports": N, "carried": <the reports block>}` (no keys when N = 0); moa gets `<brief?>\n\n---\n\n<block>\n\n---\n\n<your text>`. A wake that finds nothing left to carry sends nothing. Texts are in K11. | Spec: "each report reaches moa exactly once"; the design: "delivered in the same transaction that writes the prompt". |
| K4 | **When to wake:** with `R` the queue, `settled = min(max(last_at) + settle, min(queued_at) + gap)`, `due = max(settled, last_wake + gap)` (`Moa.last_wake: Option<UtcMs>`, in memory; none means no gap). Gates: auto on (`meta` `moa.auto` isn't `off`); phase `Off` or `Ready`; no `pending`, no `turn`, nothing in `asking`, not replaying, not restarting; `launch::spec` is `Ok`. `channel::run` runs at the end of every input (after `terminals::run`), re-arms `TimerKey::Wake` at `due` (cancelled when `R` is empty or a gate is shut), and sends the wake when `now ≥ due` and the gates are open. | Spec: "after reports settle (45 s since the last) unless the oldest waited the full gap; at most one wake per 180 s; at most 8 sessions per wake". |
| K5 | **Permissions:** `AcpIn::Permission` in the turn's thread → a feed row of kind `permission`, body `{"title", "options": [{"kind", "name"}]}` (kinds as the protocol names them; ids never leave the core), state `pending`; and `Moa.asking: BTreeMap<i64, Asking { req: u64, options: Vec<PermOption> }>` by row id. Outside a turn it is cancelled at once (`choice: None`), with no row. `Op::ConsolePermission { id: Option<i64>, answer: PermAnswer }` (`console.permission`, writes; `PermAnswer = Once \| Always \| No`, `once`/`always`/`no`; no id = the oldest): `Once` → `allow_once`; `Always` → `allow_always`; `No` → `reject_once`, else `reject_always`. It sends `AcpCmd::Permission { choice: Some(<that option's id>) }`, sets the row's state to that kind, and replies `allowed once` / `allowed always` / `refused`. Refusals: `nothing is waiting for an answer`, `that request isn't waiting any more`, `moa's agent didn't offer that answer`. A cancel (`console.cancel`), the turn's end, moa's agent exiting and a restart answer every open one `None` (when the agent is still there) and set their rows `cancelled`. `MoaRow.asking: Vec<i64>` (oldest first). Hidden console command `/permission ID once\|always\|no`. CLI `moa console answer once\|always\|no`. **Status line:** while `asking` isn't empty and no console is attached (`console_attached`), `counts_text` starts with `#[fg=#ebcb8b,bold]⚠ moa needs you#[nobold]`, then two spaces. | Spec §6.11: "held in a pending table and shown as a card…answered by option kind, never position…'moa needs you' in the status line". The run's requests can't outlive its connection, so they aren't saved. |
| K6 | **The watchdog** (`central::watchdog`): while `Thinking` and `asking` is empty, `TimerKey::Watchdog` is armed at the newest of (the prompt's send, the last message of this generation: update, permission or turn end, a permission's answer) + `watchdog_ms`. When it fires: if `now` is more than 60 s past the deadline, it re-arms at `now + watchdog_ms` (the machine slept); otherwise `AcpCmd::Cancel`, a `card` row (K7, `what: watchdog`), and the timer at `now + watchdog_kill_ms`. A turn end in between leaves it there. When that fires with the turn still open: the turn's running tools are cancelled, open permissions are `cancelled`, the waiter hears `moa's turn hung, so moa's agent was restarted`, the turn is dropped, `generation += 1`, `AcpCmd::Start` (the actor stops the old agent and its group first), and `Moa.restarting = true`. `started` then opens today's thread (`choose`: resume, else load) with no prompt; `opened` sets it open and the phase `Ready`, or, when it can't, leaves no thread (the next prompt opens one). The card's text is updated in place at each step (K11). | Spec: "no update for 5 min during a turn → `session/cancel`; unresolved 30 s later → kill and respawn the adapter and resume the thread; a card says so". |
| K7 | **Cards:** a feed row of kind `card`, body `{"what": "watchdog" \| "account", "text": <Markdown>}`. The console draws it as a command card is drawn: a head line `▸ moa's turn was stuck` / `▸ moa's account failed` / `▸ moa` (unknown `what`; Warn tone), then the text in the `┊ ` gutter, whose `/commands` are buttons (B13). | One shape for every card moa's core writes; 6d's and slice 7's join it. |
| K8 | **When moa's account fails** (`central::account`): `trouble(why) -> bool`, true when the lowercase text holds one of `ACCOUNT_TROUBLE = ["authentication required", "not logged in", "/login", "invalid api key", "oauth token", "credit balance", "billing", "usage limit", "limit reached"]`. It is checked on a turn that failed (`TurnEnded` `Err`), a new thread that couldn't open, and a start that failed. It writes a `card` (`what: account`, K11) once per account until a turn ends `end_turn` or the account changes (`Moa.account_card: Option<AccountName>`). Nothing else changes: the next prompt uses the same account. | C24: "a card offers switching to another account; nothing switches automatically". |
| K9 | **`/account [NAME]`** → `Op::ConsoleAccount { name: Option<String> }` (`console.account`, writes). From moa's caller (`Caller::Moa`): refused, `only you can switch moa's account`. While moa is starting or thinking: `moa is answering; cancel it first, then switch`. An account not in moa.toml, or with no `acp` or launcher: `there's no account NAME moa can run on`. Else `meta` `moa.account` = NAME (removed when NAME is moa.toml's `moa_account`); a running agent gets `AcpCmd::Stop`, phase `Off`, no thread; reply `moa switches to NAME with your next prompt, in a new thread`. No name: `moa is on A; others: B, C` (or `…; no other account is set up`). `launch::spec` takes `meta` `moa.account` first, when it is still a configured account. | C24's "switching" needs a way to switch that only you can use; moa.toml stays yours. |
| K10 | **`/auto [on\|off]`** → `Op::ConsoleAuto { on: Option<bool> }` (`console.auto`, writes; none flips it): `meta` `moa.auto` = `off`, or removed. Replies `moa reads session reports on its own` / `moa won't read session reports on its own; they ride on your next prompt (/auto on)`. `MoaRow.auto: bool`. **`/sleep [on\|off\|HOURS]`** → `Op::ConsoleSleepAfter { value: Option<String> }` (`console.sleep_after`, writes): `meta` `sleep.after_ms` = the hours as ms (`hours_ms`), `0` for `off`, removed for `on`; `sleep::threshold` reads it first. Replies `idle sessions go to sleep after N h · M asleep now` / `idle sessions stay up (/sleep on)`; a bad value: `/sleep takes on, off or a number of hours`. `LATER` keeps only `setup`. | Spec §6.11's command list and §6.13 ("`/sleep` changes it live"); 6b carried both here. |
| K11 | **Texts.** Wake: `(moa · a wake-up, not typed by the user) N session report(s) since your last look:` then each report, then `Update the big picture (see "Wake-ups" in your instructions).` A report: `### <title_of> · project <name or (none)> · session <first 8 of id> · finished a turn at HH:MM` (needs you: `· needs the user at HH:MM`, HH:MM local `last_at`), plus ` (K turns; earlier ones shortened)` when K > 1; then each earlier turn as `(earlier) ` + its first 500 characters (+ `…` when cut), then the last; blank lines between. Prefix: `(moa · N session report(s) since your last look, sent with the user's message)` then the reports. Watchdog card: `moa's turn had no news for <5 min>, so moa cancelled it.`; at the kill, ` It still hadn't stopped <30 s> later, so moa's agent was restarted and today's thread reopened.` (or `… was restarted. Today's thread couldn't be reopened; your next prompt starts a new one.`). Durations as `N min` when whole minutes, else `N s`. Account card: `moa's account <A> failed: <first line of why, 200 characters at most>.` then `Nothing was switched. To go on with another account:` and one `` `/account B` `` per other runnable account, or `Nothing was switched, and no other account is set up for moa in moa.toml.` | The spike's wording, with "the user" for a name (public repo). |
| K12 | **The console:** a `permission` row draws `⚠ moa asks: <title>` (Warn), then in the card gutter the buttons `[y once]` `[a always]` `[n no]` for the kinds offered (commands `/permission <id> once\|always\|no`), then, when `allow_always` is offered with a name, a dim `always: <name>`. Answered: `✓ allowed once: <title>`, `✓ allowed always: <title>` (Ok), `✗ refused: <title>` (Fail), `⊘ not answered: <title>` (Dim) for `cancelled` and for a `pending` row not in `MoaRow.asking`. With the input empty, y/a/n press the oldest open card's button of that kind; a kind not offered notes `moa's agent didn't offer that` in the footer. Head line: `⚠ answer y/a/n` (Warn, only the letters offered) while `asking` isn't empty. `◷ N for moa` (Dim) after the state while `queued > 0`. A `you` row with `reports: N`: wake → `· moa is reading N session reports…` under its header; yours → `· with N session reports` under `› text` (Dim). A session's last line under its event line only while `auto` is off (B3). | Spec §6.11's frame and card list. |

## Global Constraints

The roadmap's constraints apply, as do every earlier slice plan's and 6a's and 6b's. Also:

- **Reuse before writing.** Search the workspace before adding a helper. A near-copy is an
  **Important** finding, and each report lists what it reused.
- **Building and verifying:**
  - cargo only through `nix develop --command cargo …`, with `cargo nextest run`;
  - `scripts/verify quick` before every commit, and the full `scripts/verify` once per PR;
  - never run cargo while an e2e test runs.
- **The core stays pure:** time is `Now`; timers are `SetTimer`/`CancelTimer` deadlines. No
  timer is armed while nothing is queued and no turn runs (spec U5: no polling).
- **Exactly once, at every step:** a report is deleted only in the commit that saves the row
  carrying it. A refused commit sends nothing and deletes nothing.
- **Nothing switches moa's account** but `/account` from you (the design's "must never
  happen").
- **moa-view stays pure** (no IO, no ratatui, no crossterm). No line is wider than its width
  at 40–200; every string from data goes through `text::clean`.
- **Tests:**
  - only under `/tmp` roots;
  - never the real `claude`, the real adapter, a tmux server named `moa`/`moa-work`, or a real
    brain;
  - every wait goes through `wait_for` with a 10 s limit;
  - daemon tests move time with `FakeTime::advance`, never a sleep.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b` and
  `~/private`. Texts say "the user", never a name.
- **Log lines never hold record contents:** no report text, permission title or error text
  from moa's agent.
- **Plain words** in everything the user sees: "moa", "moa's agent", "thread", "session
  report", "wake-up". Never "ACP", "adapter" or "session/update".

## Review Focus

1. **Two permission requests at once, and typing while one waits:**
   - **The inputs:** two requests in one turn (parallel tool calls); `y` typed as the first
     letter of a prompt while a card waits; a request in a thread moa isn't answering.
   - **Expected:** y answers the oldest, and the second card stays. A `y` typed into a
     non-empty draft is text. A request outside the turn is cancelled, with no card.
   - **Pinned by:** `two_requests_queue_oldest_first` and
     `a_request_outside_the_turn_is_cancelled` (Task 4), and `y_in_a_draft_is_text` (Task 5).
2. **A card that can never be answered:**
   - **The inputs:** Esc while a card waits; moa's agent exits; the daemon restarts with a
     `pending` row in the feed; the watchdog restarts the agent.
   - **Expected:** every card shows `⊘ not answered`. `⚠ answer` and `⚠ moa needs you` clear.
   - **Pinned by:** `a_cancel_clears_the_cards` and `an_exit_clears_the_cards` (Task 4),
     `a_stale_pending_row_is_not_answered` (Task 5), `a_restart_clears_the_cards` (Task 6).
3. **Huge or odd report text:**
   - **The inputs:** a 100 KB final message; ANSI escapes and RTL text in it; a session whose
     title has a newline; a session archived before the wake; 8 sessions × 2 kinds × 6 turns,
     each 100 KB.
   - **Expected:** the wake prompt stays under `PROMPT_MAX`, and each section is one heading
     line.
   - **Pinned by:** `the_biggest_wake_is_bounded` and `odd_sessions_still_report` (Task 2).
4. **The Mac sleeps mid-turn:**
   - **The inputs:** the watchdog fires 2 h late.
   - **Expected:** it re-arms for a fresh 5 minutes and cancels nothing.
   - **Pinned by:** `a_late_timer_rearms` (Task 6).
5. **Settings typed wrong:**
   - **The inputs:** `wake_batch = 0`, `watchdog_seconds = -1`, `wake_settle_seconds = "45"`,
     `wake_setle_seconds = 45`, and `[moa]` written as `[mao]`.
   - **Expected:** one plain `moa.toml` line each. The misspelt table names `[moa]`.
   - **Pinned by:** `bad_moa_settings_say_so` (Task 1).

---

## Task 1: settings and the reports queue

**Files:** `crates/moa-core/src/config.rs` (`MoaConfig`, `[moa]`); `model.rs` (`Report`,
`ReportKind`); `change.rs` (`Report`, `DeleteReport`); `state.rs` (`reports`,
`NextIds.report`, `Table::Report`, `apply`, `Snapshot.reports`); new
`central/reports.rs` (the queue rules); `hooks.rs` (`applied_changes` queues);
`publish.rs` (`MoaRow.queued`); `crates/moa-store/src/sql/0001_v1.sql`, `rows.rs`,
`snapshot.rs`; tests in `crates/moa-core/tests/config.rs`, a new
`crates/moa-core/tests/central_reports.rs`, `crates/moa-store/tests/snapshot.rs`, `open.rs`
(its table list) and `round_trip.rs` (its change generator).

**Interfaces:**
- Produces: `MoaConfig` (K1) in `Config.moa`; `Report`, `ReportKind::{as_str, parse}`;
  `Change::{Report, DeleteReport}`; `State::reports() -> impl Iterator<Item = &Report>` in
  `(queued_at, id)` order; `central::reports::{TURNS_KEPT = 6, EARLIER_MAX = 500, LAST_MAX =
  2600, queued(state: &State, tx: &mut Tx, session: &Session, env: &HookEnvelope) ->
  Option<Change>}`; `MoaRow.queued: u32`.

- [ ] **Step 1: Failing tests:**
  - `moa_settings_default`: no `[moa]` gives 45 000, 180 000, 8, 300 000, 30 000 ms.
  - `moa_settings_parse`: each key read.
  - `bad_moa_settings_say_so`: Review Focus 5's inputs, each giving its exact line;
    `[mao]` gives `did you mean [moa]?`.
  - `a_finished_turn_queues_a_report`: hook `TurnEnded { last_message: "Merged PR #3" }` for
    Lantern. One `Persist` holds its `Change::Session` and a `Change::Report` with kind
    `TurnEnded`, turns `["Merged PR #3"]`, and `queued_at == last_at == env.at`.
  - `six_turns_are_kept`: 8 turn ends from one session give the last 6, and `queued_at` stays
    the first's.
  - `needs_you_keeps_the_latest`: `NeedsPermission { tool: "Bash" }` gives `permission: Bash`;
    a later `NeedsAnswer { what: "Which branch?" }` replaces it.
  - `a_turn_and_a_question_queue_apart`: two rows for one session.
  - `a_long_turn_is_cut_to_2600`.
  - `not_every_event_is_a_report`: an empty last message; a session in `Deps.agent_dir`; a
    duplicate hook; an older (history-only) event. None of them adds a `Report`.
  - `the_state_row_counts_them`: `queued` is 2.
  - `reports_load_and_change` (store): a write, an update, then a delete round-trip through
    the snapshot; an unknown kind is a `LoadNote`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K1 and K2.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: session reports queue in the database`.

## Task 2: the one channel

**Files:** new `crates/moa-core/src/central/channel.rs`; `central/reports.rs` (batch and
texts); `central/mod.rs` (`Pending.source`, `Turn.source`, `send` for `Source::Wake`,
`prompt` carries and deletes, `Moa.last_wake`, `Moa.wake_armed`); `effect.rs`
(`TimerKey::Wake`); `core.rs` (`channel::run` after `terminals::run`; `Input::Timer(Wake)`);
tests in `crates/moa-core/tests/central_reports.rs`.

**Interfaces:**
- Consumes: Task 1.
- Produces: `channel::{Source, run(state: &mut State, now: &Now, fired: bool, fx: &mut
  Vec<Effect>), auto_on(state: &State) -> bool, AUTO_KEY = "moa.auto"}`;
  `reports::{batch(state: &State, sessions: usize) -> Vec<&Report>, wake_text(state, now,
  &[&Report]) -> String, prefix_text(state, now, &[&Report]) -> String}`; the `you` row
  body's `reports` and `carried` keys.

- [ ] **Step 1: Failing tests** (settings at their defaults unless said):
  - `a_report_waits_for_it_to_settle`: a report at T arms `Wake` at T+45 s. Nothing at
    T+44 s. Firing at T+45 s starts moa's agent; once the thread opens, the prompt sent starts
    with K11's wake header.
  - `a_steady_stream_still_wakes_after_the_gap`: a report every 30 s from T wakes at T+180 s.
  - `wakes_are_180_s_apart`: after a wake at W, a report at W+10 s wakes at W+180 s.
  - `a_wake_takes_8_sessions`: 10 sessions report. The first wake's text has 8 `### `
    sections, the oldest queued; `queued` is then 2.
  - `a_newer_report_keeps_its_place`: Lantern at T, Kite at T+10, Lantern at T+20 → Lantern
    first.
  - `the_wake_text`: insta snapshot of one wake with a 3-turn report and a needs-you report.
    Earlier turns are `(earlier) ` + 500 characters + `…`.
  - `reports_ride_on_your_prompt`: a queued report, then `console.prompt "hi"`. One
    `Persist` holds the `you` row (body text `hi`, `reports: 1`, `carried`) and its
    `DeleteReport`, before the `AcpCmd::Prompt`, whose text ends `\n\n---\n\nhi`.
  - `the_wake_row_is_automatic`: `automatic` is true, its message id is the turn's, and no
    waiter is answered.
  - `auto_off_never_wakes_but_prompts_still_carry`: `meta` `moa.auto = off`. No `Wake` timer
    is armed, and a prompt still carries the report.
  - `no_wake_while_moa_answers`: a due wake waits for the turn's end, then goes.
  - `no_wake_after_a_failed_start`.
  - `a_refused_commit_delivers_nothing`: with writes refused, the prompt is refused, the
    report stays, and nothing is sent.
  - `the_biggest_wake_is_bounded`: 8 sessions × 2 kinds × 6 turns of 100 KB each. The wake
    text has at most 90 000 characters.
  - `odd_sessions_still_report`: a title with a newline, ANSI escapes and RTL text in the
    turn, and a session archived before the wake. Each section's heading is one line, and the
    report is delivered.
  - `no_timer_with_nothing_queued`: an idle brain with no reports arms no `Wake`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K3, K4 and K11's wake and prefix texts.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: session reports wake moa, or ride on your prompt`.

## Task 3: exactly once, proven

**Files:** `crates/moa-core/tests/central_reports.rs` (the proptest);
`tests/common/central.rs` (`saved` carries `reports` and `next_ids.report`; a `Disk`
helper).

**Interfaces:**
- Produces: `common::central::Disk`, a second `State` that applies only the changes of the
  `Persist` effects the test executes. `Disk::restart(&self, b) -> State` loads
  `saved(b, &disk)`.

- [ ] **Step 1: Write `each_report_reaches_moa_exactly_once`** (proptest, 256 cases):
  - **The world:** four live sessions, and settle, gap and batch small (`[moa]`
    `wake_settle_seconds = 5`, `wake_gap_seconds = 20`, `wake_batch = 2`).
  - **Generated steps (1–60):**
    - a turn end with a unique token as its text, or a needs-you with one, for a session;
    - your prompt;
    - advance 0–60 s, firing a due timer;
    - moa's agent answers what is out: `Started`, `Opened`, then `TurnEnded` `end_turn` for
      each prompt;
    - `/auto` flipped;
    - writes refused for the next input;
    - a restart, either before the last input's `Persist` was executed (it is lost) or after
      it but before its `AcpCmd::Prompt` reached moa ("died mid-send").
  - **Then drain:** auto on, then advance 60 s at a time, up to 50 times, until the queue is
    empty.
  - **The model:** the test keeps each (session, kind) queue by K2's rules. It knows which
    tokens were pushed out by a 7th turn or replaced by a newer needs-you before delivery.
  - **Asserted:**
    - no token is in two saved `you` rows;
    - every token the model keeps is in exactly one saved row;
    - pushed-out tokens are in none;
    - each prompt moa received holds exactly the tokens of its own row.
  - Copy `tests/sleep.rs`'s proptest config and its regressions file.
- [ ] **Step 2: Run it.** Expected: PASS. If it fails, fix Task 2's code, not the property.
  Keep the shrunk case as a named test.
- [ ] **Step 3: Make it fail once on purpose** (delete the reports one commit after the `you`
  row). Expected: FAIL with a duplicate or a lost token. Revert.
- [ ] **Step 4: Commit.** `v1: each session report reaches moa exactly once (proptest)`.

**Try it** (p1's body): the 6b sandbox with `[moa] wake_settle_seconds = 5`. Finish a turn
in a session, wait, and moa's console shows `◆ moa · on its own`.

## Task 4: permissions in the core

**Files:** `crates/moa-core/src/central/turn.rs` (`permission` holds; the refusal goes),
`central/mod.rs` (`Moa.asking`, `cancel`, `exited`, `answer_permission`), `wire.rs`
(`PermOption.name`), `feed.rs` (`kind::PERMISSION`), `op.rs` (`ConsolePermission`,
`PermAnswer`), `console.rs` (hidden `/permission`), `publish.rs` (`MoaRow.asking`),
`channel.rs` (the `asking` gate), `tmux/status.rs`;
`crates/moa-agents/src/acp/convert.rs` (the option's name); `crates/moa/src/console.rs`
(`answer`, and `feed` prints a permission row as `permission  <title> · <state>`);
`crates/moa-testkit/src/bin/stub-acp.rs` (an option's name is its id). Tests:
`crates/moa-core/tests/central_turns.rs`, `central_reports.rs`, `tmux_status.rs`;
`crates/moa-daemon/tests/moa.rs`, and **its snapshot
`crates/moa-daemon/tests/snapshots/moa__a_recorded_session_replays.snap`, which changes**;
`crates/moa/tests/moa_e2e.rs`.

**Interfaces:**
- Consumes: `console_attached`, `channel::run`.
- Produces: `Op::ConsolePermission { id: Option<i64>, answer: PermAnswer }`;
  `PermOption { id, kind, name }`; `Moa.asking`; `MoaRow.asking: Vec<i64>`;
  `kind::PERMISSION = "permission"`; the CLI `moa console answer once|always|no`.

- [ ] **Step 1: Failing tests:**
  - `a_permission_waits_for_your_answer` (replaces `a_permission_is_refused_by_kind`). The
    request saves a `permission` row (`pending`), sends nothing to the agent, and puts the row
    in `MoaRow.asking`.
  - `each_answer_sends_its_kind`: `once`, `always` and `no` send the id of the option with
    the matching kind. The row's state becomes that kind, and the reply is K5's.
  - `no_falls_back_to_reject_always` (replaces `no_reject_once_cancels_the_turn`).
  - `an_answer_not_offered_is_refused`: `always` with no `allow_always` →
    `moa's agent didn't offer that answer`.
  - `two_requests_queue_oldest_first`: `console.permission` with no id answers the first.
  - `a_request_outside_the_turn_is_cancelled`: `choice: None`, and no row.
  - `a_cancel_clears_the_cards`, `an_exit_clears_the_cards`, `a_turn_end_clears_the_cards`.
  - `no_wake_while_a_permission_waits`.
  - `moa_needs_you_without_a_console`: `@moa_status` starts with K5's segment. With a console
    panel registered it doesn't. It clears once the request is answered.
  - `the_option_name_reaches_the_core` (host test in `crates/moa-agents/tests/acp_host.rs`).
  - `a_permission_is_answered_from_the_command_line` (`moa_e2e.rs`, replaces
    `a_permission_is_refused_and_said`). `moa console prompt tidy up` runs in a thread, and
    `moa console answer no` is retried until it isn't refused. The stub logs
    `optionId: "no"`, and the feed's kinds are `you, permission, moa`.
  - `a_recorded_session_replays`, updated. The prompt is sent with `wait`. A second client
    waits (`wait_for`) for a `pending` `permission` row, then sends `console.permission
    {"answer": "no"}`. **The snapshot changes:** the refusal's `system` row becomes a
    `permission` row with state `reject_once`. Review the new snapshot by eye before
    accepting it.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K5** (the console's side is Task 5).
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa's permission requests wait for your answer`.

## Task 5: permission cards in the console

**Files:** `crates/moa-view/src/views/console.rs` (the card, the head line),
`panel/console_keys.rs` (y/a/n), `fixture.rs` (rows for each state); tests in
`views/console.rs`, `panel`'s tests and `moa-tui/tests/render.rs`.

**Interfaces:**
- Consumes: `MoaRow.asking`, `kind::PERMISSION`, the hidden `/permission` command.

- [ ] **Step 1: Failing tests:**
  - `debug_lines` snapshots at widths 40 and 80. They show a pending card with three buttons
    and the `always:` line, a card offering only once and no, and each answered state.
  - `y_a_and_n_press_the_oldest_card`: with the input empty, each sends
    `Op::ConsoleCommand { line: "/permission <id> once|always|no" }` for the oldest id.
  - `y_in_a_draft_is_text`.
  - `a_kind_not_offered_says_so`: `a` with no `allow_always` sends nothing and notes it.
  - `a_stale_pending_row_is_not_answered`: `pending`, but not in `asking`.
  - `the_head_says_answer`: `⚠ answer y/a/n`, and `⚠ answer y/n` when only those are offered.
  - `a_click_on_y_answers`.
  - `console_never_panics` (6b's proptest) generates `permission` bodies too.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K12's permission part.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: answer moa's permission requests with y, a or n`.

**Try it** (p2's body): `moa --sandbox` with the real adapter, against a copy of the brain.
Ask moa to create `note.txt` in its folder. A card asks; press `y`. Close the console and
ask again: the status line says `⚠ moa needs you`. Then answer with `moa console answer no`.

## Task 6: the watchdog, and cards

**Files:** new `crates/moa-core/src/central/watchdog.rs`; `central/mod.rs`
(`Moa.watchdog`, `Moa.restarting`, `started`/`open`/`opened` without a prompt), `turn.rs`
(every message of the generation feeds the watchdog), `feed.rs` (`kind::CARD`), `effect.rs`
(`TimerKey::Watchdog`), `core.rs`; `crates/moa-view/src/views/console.rs` (`card` rows, K7);
tests in a new `crates/moa-core/tests/central_watchdog.rs` and `views/console.rs`.

**Interfaces:**
- Consumes: `MoaConfig.{watchdog_ms, watchdog_kill_ms}`, `turn::cancelled_tools`.
- Produces: `kind::CARD = "card"`; `central::card(state, tx, what: &str, text: &str) ->
  FeedEntry`; `watchdog::{Watchdog, Stage::{Watching, Cancelled { card: i64 }}}`.

- [ ] **Step 1: Failing tests:**
  - `no_news_for_5_min_cancels`: a turn sent at T arms `Watchdog` at T+300 s. An update at
    T+100 s moves it to T+400 s. Firing then sends `AcpCmd::Cancel` and saves the card
    `moa's turn had no news for 5 min, so moa cancelled it.`
  - `a_turn_that_ends_after_the_cancel_is_left_alone`: an end within 30 s sends no `Start`,
    and the card stays.
  - `still_stuck_30_s_later_restarts`:
    - `AcpCmd::Start` comes with the next generation;
    - the waiter hears K6's line, and running tools are `cancelled`;
    - after `Started`, `AcpCmd::Open { Resume { thread } }` goes, with no prompt;
    - after `Opened`, the phase is `Ready` with the thread open, and the card has K11's
      restart sentence.
  - `a_restart_without_resume_loads`.
  - `a_restart_that_cant_reopen_says_so`: the thread is gone, the card says so, and the next
    prompt opens a new thread.
  - `a_restart_clears_the_cards`.
  - `the_watchdog_waits_while_you_decide`: a pending permission cancels the timer. An answer
    20 min later re-arms it 5 min from the answer.
  - `a_late_timer_rearms`: firing 2 h late re-arms at now+300 s and sends nothing.
  - `the_limits_are_settings`: `watchdog_seconds = 2` arms at T+2 s, and the card says `2 s`.
  - `no_watchdog_when_idle`.
  - `cards_draw_as_cards` (moa-view): `debug_lines` of both cards at widths 40 and 80. An
    unknown `what` draws `▸ moa`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K6 and K7.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: a watchdog for moa's turns`.

## Task 7: `/auto`, `/sleep`, `/account`

**Files:** `crates/moa-core/src/op.rs` (`ConsoleAuto`, `ConsoleSleepAfter`,
`ConsoleAccount`), `console.rs` (parsing, `COMMANDS` help lines, `LATER`), `central/mod.rs`
(the ops), `central/launch.rs` (`meta` `moa.account` first), `sleep.rs` (`threshold` reads
`meta` first), `publish.rs` (`MoaRow.auto`); tests in `crates/moa-core/tests/console.rs`,
`central_launch.rs`, `sleep.rs`, and `crates/moa-daemon/tests/console.rs` (6b's
`later_commands_say_so` now uses `/setup`).

**Interfaces:**
- Produces: the three ops of K9 and K10; `ACCOUNT_KEY = "moa.account"`, `SLEEP_KEY =
  "sleep.after_ms"`. `COMMANDS` help: `auto` `[on|off]: moa reads session reports on its
  own`; `sleep` `[on|off|HOURS]: idle sessions go to sleep after HOURS`; `account` `[NAME]:
  switch moa to another of your accounts`; hidden `permission`.

- [ ] **Step 1: Failing tests:**
  - `auto_flips_and_sets`: `/auto` flips it; `/auto off` and `/auto on` set it. The replies
    are K10's, and `MoaRow.auto` follows.
  - `sleep_sets_the_threshold`: `/sleep 2` makes an idle session due at +2 h; `/sleep off`
    never; `/sleep on` goes back to moa.toml's; bare `/sleep` says the current one; `/sleep x`
    is refused.
  - `account_switches_with_the_next_prompt`: `/account acct-b` writes `meta`. A running agent
    gets `Stop`. The next prompt's `Start` carries acct-b's spec, and its thread is new.
  - `account_back_to_moa_toml_clears_meta`.
  - `account_refused_from_moa`, `account_refused_while_thinking`, `account_unknown_refused`.
  - `bare_account_lists`.
  - `later_commands_say_so`: only `/setup` is still later.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K9 and K10.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: /auto, /sleep and /account`.

## Task 8: the account card

**Files:** new `crates/moa-core/src/central/account.rs`; `central/mod.rs` (`opened`'s and
`exited`'s failures), `turn.rs` (`ended`'s `Err`); tests in a new
`crates/moa-core/tests/central_account.rs`.

**Interfaces:**
- Consumes: `central::card` (Task 6), `ConsoleAccount` (Task 7).
- Produces: `account::{ACCOUNT_TROUBLE, trouble(why: &str) -> bool}`,
  `Moa.account_card`.

- [ ] **Step 1: Failing tests:**
  - `an_auth_failure_offers_other_accounts`: a turn fails with `Authentication required` on
    acct-a. A `card` (`what: account`) holds `` `/account acct-b` ``. No `Start` follows, and
    the next prompt starts acct-a.
  - `a_billing_failure_too` (`Credit balance is too low`), and `a_usage_limit_too`.
  - `an_ordinary_failure_is_no_card` (`connection reset`).
  - `once_per_account_until_a_turn_ends_well`.
  - `a_start_that_fails_on_auth_offers_too`, and the same for a new thread that couldn't
    open.
  - `no_other_account_says_so`.
  - `nothing_switches_on_its_own`: three failures in a row; every `Start` is acct-a's.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement K8 and K11's account text.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: a card when moa's account fails`.

## Task 9: the console's new lines

**Files:** `crates/moa-view/src/views/console.rs` (`◷ N for moa`, the reports lines, the
turn text by `auto`), `fixture.rs`; tests in `views/console.rs` and `moa-tui/tests/render.rs`.

**Interfaces:**
- Consumes: `MoaRow.{queued, auto}`, the `you` body's `reports`.

- [ ] **Step 1: Failing tests:**
  - `the_head_counts_reports`: `◷ 3 for moa` while `queued` is 3, and nothing at 0. Snapshots
    at widths 40 and 80.
  - `a_wake_says_it_reads_reports`: `· moa is reading 3 session reports…` under
    `◆ moa  HH:MM · on its own`.
  - `your_prompt_says_what_it_carried`: `· with 2 session reports` under `› hi`.
  - `turn_text_only_when_auto_is_off`: a `●` event line has its last line under it with auto
    off, and none with auto on.
  - `render.rs`'s pixel snapshots for the console at all `WIDTHS`, updated.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement the rest of K12.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: the console shows moa's reports and auto`.

**Try it** (p3's body): the sandbox with `[moa] watchdog_seconds = 20`. Ask moa something long
and watch for the card. Then `/auto off`, finish a turn in a session, and see `◷ 1 for moa`.

## Task 10: the stub scenarios

These are the roadmap's "done when", as daemon tests (`Rig`, `FakeTime`, `stub-acp`).

**Files:** `crates/moa-testkit/src/stub.rs` and `bin/stub-acp.rs` (`StubStep::Fail { message
}`: the prompt's answer is a JSON-RPC error, code `-32000`, with `message`);
`crates/moa-testkit/src/lib.rs` (`stub_moa_toml_two_accounts()`, adding acct-b);
`crates/moa-daemon/tests/common/rig.rs` (`Rig::with_toml`, `Rig::seed_sessions(n) ->
Vec<SessionId>`: live sessions with conversations, written through `rig.store()` before
`start`; `hook_line` moved here from `tests/hooks.rs`); a new
`crates/moa-daemon/tests/central.rs`.

- [ ] **Step 1: Write the tests:**
  - **Permission:** `a_permission_waits_and_y_goes_on`. `Ask`, then say, then end. The
    prompt's waiter hears nothing until `console.permission once`. Then the stub logs the
    allow-once option id, and the answer comes.
  - **Slow turn:** `a_slow_turn_is_left_alone`. The script is say, gate g1, say, gate g2, end.
    Advance 4 min, open g1, wait for the second row, advance 4 min, open g2. The turn ends
    `end_turn`, and there is no `session/cancel` and no card.
  - **Hang → watchdog:**
    - `a_hang_is_cancelled_by_the_watchdog`: `Hang`. Advance 5 min: the stub logs
      `session/cancel`, the turn ends cancelled, and the cancel card is saved.
    - `a_hard_hang_restarts_moas_agent`: `HangHard`. Advance 5 min, then 30 s. There are two
      `initialize`s, a `session/resume` of the thread, and one more launch. The card has the
      restart sentence. The next prompt is answered.
  - **Crash → resume:** 6a's `a_crash_mid_turn_says_so_and_the_next_prompt_resumes` stands as
    is. Add `a_crash_while_asking_clears_the_card`: `Ask` then `Die`; the row ends
    `cancelled`, and `asking` is empty.
  - **Wake batching:** `reports_wake_moa_in_batches`. Ten seeded sessions each send a `stop`
    hook. Advance 45 s: one `session/prompt` is logged with 8 `### ` sections. Advance 180 s:
    a second with 2. The `you` rows are automatic.
  - `a_report_rides_on_your_next_prompt`: `/auto off`, then a hook, then a prompt. The
    logged prompt holds the report and ends with the prompt's text.
  - `reports_survive_a_daemon_restart`: a hook, stop, start, prompt. It carries the report,
    once.
  - **The account card:** `an_auth_failure_shows_the_account_card` (`Fail { message:
    "Authentication required" }`, two accounts).
- [ ] **Step 2: Run them.** Expected: PASS. Make each fail once on purpose, then revert.
- [ ] **Step 3: Commit.** `v1: the stub scenarios for permissions, the watchdog and wakes`.

## Task 11: end to end in the cockpit

**Files:** `crates/moa/tests/console_e2e.rs`; fixes found by it.

- [ ] **Step 1: Write the tests** (the real binary, a tmux sandbox, `stub-acp` scripted):
  - `y_allows_a_permission`: the stub asks `tidy the Harbor`. The console shows `⚠ moa asks:
    tidy the Harbor`, `[y once]` and `⚠ answer y/a/n`. Key `y`. The stub logs the allow-once
    id, and the console shows `✓ allowed once: tidy the Harbor`.
  - `n_refuses_and_moa_goes_on`: as above with `n`. The console shows `✗ refused`, then the
    stub's words.
  - `moa_needs_you_without_a_console`: kill the console's pane, then `moa console prompt`
    in a thread. `tmux show -gv @moa_status` holds `moa needs you`. `moa console answer no`
    clears it.
  - `a_hard_hang_shows_the_watchdog_card`: `moa.toml` adds `[moa]` with `watchdog_seconds =
    2` and `watchdog_kill_seconds = 1`; the stub does `HangHard`. The console shows
    `▸ moa's turn was stuck`, then `moa's agent was restarted`, then `● ready`.

  Each runs 10 times under load, and each is made to fail once on purpose.
- [ ] **Step 2: Run them.** Expected: PASS.
- [ ] **Step 3: Run `scripts/verify`** (full). Expected: PASS.
- [ ] **Step 4: Commit.** `v1: permissions and the watchdog end to end`.

**Try it** (p4's body): p2's and p3's try-its, in one sandbox.

## How slice 7 joins the channel

Asks join without changing what 6c built:
- **A new source:** `channel::Source` gains `Ask(AskId)`. `channel::run` checks for the
  oldest pending ask to `moa` before the wake. An ask goes whatever `/auto` says, behind the
  same gates: idle, no permission waiting, not restarting.
- **One commit:** its delivery claims the ask (`pending → sent`) in the commit that saves its
  automatic `you` row, body `{"text", "ask": <id>}`. That is the exactly-once rule reports
  follow.
- **The turn's end:** `Turn.source` tells `turn::ended` that the turn answers an ask. Slice 7
  adds `channel::ended(state, tx, &turn, &result)`, which writes the reply: the turn's final
  text, or `moa's turn ended early: <reason>`.
- **Never a prefix:** an ask always gets its own turn.
- **Slice 7's own:** its card (`✉ NAME asks moa`) is a `card` row, `what: ask`.
- **Its proptest** reuses Task 3's `Disk` and restart steps.

## Carried, and where each goes

- **To 6d:**
  - the opening brief;
  - nags on the wake text (`⏰`, C20), after the reports;
  - suggestion cards;
  - the terminal items.
- **To slice 7:** asks as a channel source (above), and the ask card.
- **To slice 8:**
  - `/setup`;
  - adding an account from the account card;
  - accounts beyond those in moa.toml.
- **Later, if wanted:** keep `last_wake` across restarts; wake only while a console is
  attached (call 1).
- **To slice 10 (from 6b's p4):** sessions moa launches in a sandbox (`ops/launch.rs`) don't
  get the real home's login, as moa's own agent now does. On the real home it doesn't arise.
- **Open, no slice yet:** moa's agent runs on the account's Claude store, so the account's own
  hooks run for it too. In the recorded run, one made Claude ask about `moactl ls` despite the
  allow-list. 6c's cards answer such asks; whether moa's agent should run them at all is open.
- **Still carried from 6a/6b:** the last system line is lost on a failed commit; a failed
  commit that extends a row more than 100 back is duplicated; `)` isn't escaped in allow-list
  folders; `console.command` counts as a write; `/new`'s flag lives in memory only.
