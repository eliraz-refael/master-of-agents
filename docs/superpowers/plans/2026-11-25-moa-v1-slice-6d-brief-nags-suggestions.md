# moa v1 slice 6d: the opening brief, reminders, suggestions

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The first time you show up each day, moa opens the day with a brief that ends with
what's due. moa's suggestions are cards in the console with yes, later and no, and they read
the same wherever you answer them. A yes with nothing to run tells moa. moa sees your idle
terminals and offers to close them, and offers to close a session's terminals when you close
or archive it. Slice 6 is complete after 6d.

**Architecture:**
- **Everything new is core logic** in `moa_core::central`, plus one tmux hook. Time is an
  input, as everywhere. No new timer: the channel's `Wake` timer and the end-of-input check
  already cover "as soon as moa is idle".
- **The channel gets two sources.** `Source::Brief` opens the day, and `Source::Yes` tells
  moa about a yes with no actions. Each prompt carries what is due at its commit: session
  reports, yeses, and (the brief and wake-ups only) the due list and idle terminals. The one
  commit that saves the `you` row also marks each carried thing as delivered.
- **Delivered means saved.** A report is deleted (6c). A yes's suggestion row moves from
  `waiting for moa` to `told moa`. A due task gets its `nags` row. The day's brief sets
  `meta` `moa.briefed`. All of it happens in that one commit, so a restart neither repeats
  nor loses anything.
- **A suggestion card is a feed row** of kind `suggestion`, body `{"id", "text"}`. The
  console draws it from the live `suggestions` topic, so an answer anywhere redraws it
  everywhere.
- **Terminals:** `opened_by = moa` for a `term` moa's agent opens. `term close` joins the
  suggestion whitelist, and only as `term close`.

```
 tmux client-attached / client-active, moa keys, typing ─► opening::present ─► brief_due
 end of every input ─► channel::run │ idle + brief_due ─► Brief │ yes waiting ─► Yes │ due ─► Wake
 prompt(): commit {you row, DeleteReport…, Suggestion(told moa)…, Nag…, meta moa.briefed} ─► send
 suggest / close / archive ─► Suggestion + `suggestion` feed row ─► card [yes] [later] [no]
 yes without actions ─► Suggestion(result "waiting for moa") ─► channel
```

**Tech Stack:** Rust; the existing crates. No new dependencies. No schema change: `nags`,
`suggestions`, `meta` and `feed` already hold everything.

**Spec:** `docs/superpowers/specs/2026-11-22-moa-slice-6-central-agent-design.md` ("6d", "Must
never happen", and the "My calls" bullets on presence and the terminal items), which the user
approved. It builds on the main spec `docs/superpowers/specs/2026-11-15-moa-rust-design.md`
§6.11 (C19, C20, hidden prompts, the console's cards) and §6.12 (suggestions). The main spec
wins where they differ.

Roadmap: "Slice 6", item 6, plus the backlog's terminal items (`docs/backlog.md`,
"Terminals"). Builds on 6a–6c (#85–#95).

## Builds on (names to use)

- **The channel and prompts** (`crates/moa-core/src/central/`):
  - `channel.rs`: `Source`, `run`, `due`, `auto_on`;
  - `mod.rs`: `send`, `prompt` (the one commit, then `AcpCmd::Prompt`), `Moa` (`last_wake`,
    `wake_armed`, `restarting`, `asking`, `pending`, `turn`), `failed_wake`, `card`,
    `console_attached`;
  - `reports.rs`: `batch`, `wake_text`, `prefix_text`, `cut_marked`, `heading_field`;
  - `account.rs`: `thread` (the open thread, else the newest saved);
  - `prompt.rs`: `system_text` (its brief is `ops::read::brief`, which already ends with the
    newest `thread-summary` note, U19);
  - `feed.rs`: `kind::*`, `row`, `text_body`; `constitution.md`.
- **Suggestions:** `ops/suggestions.rs` (`suggest`, `answer`, `run_action`, `split`,
  `waiting`); `model::{Suggestion, SuggestionStatus, SuggestionAction, ACTION_VERBS}`;
  `moa_proto::cli::{parse_action, SUGGESTION}`.
- **Nags:** `model::Nag { task_id, due, day }`, `Change::Nag`, `State.nags` (keyed by
  `(task, due, day)`), already saved and loaded by the store.
- **Terminals:** `terminals::{open_records, is_idle, close, find}`, `State::term_row`
  (`active`), `config.ui.terminal_idle_ms`; `ops/term.rs` (`term`, `Tell`, `record_opened`,
  `close`, `close_answered`, `with_tilde`); `Asked::Term`.
- **Sessions:** `ops/sessions.rs` (`close`, `archive`, `kill_checked`, `kill_tab`);
  `ops::launch::title_of`.
- **Presence inputs:** `tmux/mailbox.rs` (`HookMsg`, `Mailbox::parse`), `moa-tmux/src/conf.rs`
  (`HOOK_TABLE`), the mailbox match in `core.rs` (`HookMsg::ClientAttached`).
- **State:** `State::{meta, caller, suggestions, tasks}`, `Caller::Moa`, `Now.local_day`.
- **moa-view:** `views/console.rs` (`Draw::entry`, `core_card`, `carded`, `button_span`,
  `hanging`, `carried`, the card gutter), `data.suggestions`.
- **Tests:**
  - core: `tests/common/central.rs`, `tests/central_reports.rs` (its proptest),
    `tests/central_turns.rs`, `tests/terminals.rs`, `tests/suggestions.rs` if present (else
    the suggestion tests' file);
  - daemon: `tests/central.rs`, `tests/common/{rig.rs, hooks.rs}`;
  - e2e: `crates/moa/tests/console_e2e.rs`;
  - testkit: `stub_moa_toml`, `write_stub_script`.

## Calls for you to check

The most important come first.

1. **The opening brief goes even with `/auto off` (D3).** `/auto` says it is about session
   reports, and the brief is the one turn a day starts with. The other choice: `/auto off`
   stops it too, and your first prompt opens the day instead.
2. **A yes with nothing to run goes to moa as soon as it is idle, even with `/auto off`
   (D5).** It's your answer, not a report. It waits while moa answers.
3. **What counts as you showing up (D2):**
   - a tmux client attaching, or becoming the active one (the new `client-active` hook);
   - one of moa's keys, or a tmux prompt you typed;
   - typing, a `/command` or paging in the console;
   - moving the selection in a panel.

   Something moa does itself (a wake-up's `moactl focus`) doesn't count.
4. **Your first prompt of the day takes the brief's place (D3).** It carries the opening
   lines and the due list before your text, and no separate brief turn goes.
5. **Offers to close a session's terminals come from the daemon, not a turn of moa's (D9).**
   - They cover only terminals recorded as that session's: splits of its tab.
   - A close kills only the agent's pane, so its splits outlive it: every close offers for
     them, as an archive does.
6. **Suggestion cards use buttons only (D7).** Tab, Enter and click press them; `y/a/n` stay
   permission keys.
7. **The due list (D4):**
   - one line per lateness, most overdue first, due tomorrow included;
   - at most 20 tasks per prompt (the rest come next time).
8. **Idle terminals (D6):**
   - the opening lists them all;
   - a wake-up lists only those that went idle since the last wake-up.
9. **Yesterday's summary (U19) stays as it is.** moa writes a `thread-summary` note when you
   wrap up, and the brief carries the newest one. No turn at day's end writes one.
10. **A yes waiting for moa is marked in the suggestion's `result` column** (`waiting for moa`,
    then `told moa`). No schema change.

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged. Each gets:
- `scripts/verify`;
- per-task reviews;
- one `/code-review medium --fix` over its branch;
- green CI.

Bodies end with the try-it, when there is one.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-6d-1` | 1–4 | `v1 slice 6d (1): suggestion cards, terminals moa opens and closes` |
| p2 | `v1/slice-6d-2` | 5–6 | `v1 slice 6d (2): the opening brief and what's due` |
| p3 | `v1/slice-6d-3` | 7–8 | `v1 slice 6d (3): the stub scenarios, end to end` |

p1 also carries this plan.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| D1 | **Delivered with the prompt.** `prompt()` builds what a prompt carries at its commit, in this order: `[system brief (thread's first)] [opening block (Brief, or your prompt while brief_due)] [yes block] [reports block] [due block (Brief, Wake, or your prompt that opens the day)] [idle block (same)] [your text]`, joined by `\n\n---\n\n`, empty parts left out. The reports block is 6c's prefix text on your prompt, and 6c's wake text on every automatic prompt (a wake-up's own text is that block, so it isn't repeated). The commit holds the `you` row and, for each carried thing: `DeleteReport` (6c); `Change::Suggestion` with `result = "told moa"`; `Change::Nag { task_id, due, day: today }`; `Change::Meta { moa.briefed = <today as YYYY-MM-DD> }` when the opening is carried. A refused commit marks nothing and sends nothing (6c's rule). Body keys, each only when nonzero/true: `reports: N`, `carried: <blocks before your text>` (your prompt), `yeses: [ids]`, `nags: K`, `opening: true`. `Moa.last_wake` moves whenever an automatic prompt carries reports. | Spec: "each report reaches moa exactly once"; C20: "the dedupe is persisted"; one rule for every source, so slice 7's asks join the same way. |
| D2 | **Presence:** `central::opening::present(state: &mut State, now: &Now)`. When `meta` `moa.briefed` isn't today's date, it sets `Moa.brief_due = true` (memory). Called from: `HookMsg::ClientAttached`, the new `HookMsg::ClientActive` (tmux hook `client-active[90]`, mailbox `moa:hook client-active <client>`, `hook_client` as for attached), every `Mailbox::Key`, `Mailbox::Prompt`, and the client ops `console.prompt`, `console.command`, `console.page`, `ui.here`, `ui.select_view`. Nothing else: ops from moa's own caller never call it. | C19: "a client attach or `client-active` mailbox message, not at midnight"; the spec's "My calls" add `client-active` beside `client-attached`. A single attached client never re-attaches, so keys and typing count too. |
| D3 | **The opening brief:** `Source::Brief`. `channel::run` sends it when `brief_due` and the gates are open: phase `Off` or `Ready`; no `pending`, no live `turn`, `asking` empty; not replaying, not restarting; writes not refused; `launch::spec` is `Ok` or the agent runs. `/auto` isn't a gate. It goes before a yes or a wake that is also due, and carries their reports and yeses. Its row is `automatic`. Its commit sets `moa.briefed` and clears `brief_due` (cleared at the commit, kept on a refusal). **Your prompt while `brief_due`** carries the opening block too and opens the day the same way (D1). A new day's thread is opened by `threads::choose` as today, so the system prompt's brief is today's; a thread a wake-up opened earlier today is used as it is. | C19; "moa pays for a turn nobody reads" must never happen, so only presence starts it. |
| D4 | **Due tasks (C20):** `central::due::due_now(state: &State, now: &Now) -> Vec<&Task>`: tasks with `status.is_open()`, `due <= now.local_day + 1`, and no `nags` row `(task, due, now.local_day)`; ordered by `(due, id)`; at most `DUE_MAX = 20`. `due::block(state, now, &[&Task]) -> String` (K-texts in D10). Carried only by the brief, a wake-up and your prompt that opens the day; never alone (no wake for a due task, U18). | U18: due dates ride on the brief and wake-ups only; C20: once per (task, due, day). |
| D5 | **A yes with nothing to run:** `suggestions::answer` with `Answer::Yes` on a suggestion with no actions sets `result = Some(YES_WAITING)` (`"waiting for moa"`) and replies `yes; moa acts on it when it's free`. `State::yeses()` iterates accepted suggestions with no actions and `result == YES_WAITING`, by id. `Source::Yes`: `channel::run` sends one when any wait and the D3 gates are open (`/auto` isn't a gate). Every prompt carries all waiting yeses (D1) and sets each to `YES_TOLD` (`"told moa"`). | §6.12: "without actions, yes is a signal delivered to moa through the one channel". |
| D6 | **Idle terminals:** `central::idle::idle_terms(state: &State, now: &Now, since: Option<UtcMs>) -> Vec<&Terminal>`: open records that are idle (`terminals::is_idle(touched_at or opened_at, row.active, terminal_idle_ms, now)`), and, with `since`, only those whose idle start (`touched_at or opened_at` + `terminal_idle_ms`) is after `since`. The brief passes `None`; a wake passes `Moa.last_wake`. Oldest first, at most `IDLE_MAX = 10`. None when `terminal_idle_ms` is 0. | Spec 6d: "idle terminals ride on the brief and on wake-ups. moa sees the idle list". Not re-telling the same list on every wake keeps wakes short. |
| D7 | **Suggestion cards:** every new suggestion (any caller; not a duplicate) also writes a feed row of kind `suggestion` (`feed::kind::SUGGESTION`), body `{"id": N, "text": <text>}`, in the same commit, in `central::card_thread(state)` (the open thread when it is saved, else the newest saved; `account::thread` moves here and the account card uses it). No thread: no row (the today view still shows it). The console draws it (D10's texts) from `data.suggestions[id]`, the body's text standing in when the row isn't there. Buttons: `[yes]` `[later]` `[no]` with commands `/suggestion yes N`, `/suggestion later N`, `/suggestion no N`, pressed by Tab/Enter/click; `yes` is left out when `actions_invalid` is set. | §6.11's "suggestion ✦ moa suggests" card; §6.12: "cards update everywhere when answered anywhere". |
| D8 | **`term close` in a suggestion:** `ACTION_VERBS` gains `term`; `parse_action` refuses a `term` action that isn't `Op::TermClose` with `a suggestion can run term only as term close`. `run_action` runs `Op::TermClose { target }` by finding the record (`terminals::find`) and starting the gentle close (`terminals::close`) with `Tell::Quiet`, a new variant: nobody is told; its activity line `closed terminal NAME` is written as for `Tell::Reply`; a failure logs `terminal #N wasn't closed for a suggestion: <why>`. The close's query goes out after the answer's commit; a refused commit starts none. | Spec 6d: "`term close` joins the suggestion action whitelist". `term PATH` would open terminals on a yes, which nobody asked for. |
| D9 | **Offers:** after `sessions::close` (whose kill takes only the agent's pane, so the session's splits outlive it) and `sessions::archive`, the open terminal records whose `session_id` is one of those sessions become one suggestion, through `suggest` (so it is deduplicated and carded), with one `--do "term close <id>"` per terminal. **`opened_by = moa`:** `Asked::Term` carries `opened_by: OpenedBy`; `term` from `Caller::Moa` sets `Moa`, any other caller `You`; `record_opened` writes it. | Spec 6d's terminal bullets; the design's call: "through suggestions, not as a timed sweep". |
| D10 | **Texts.** See the block below. | The spike's wording; "the user", never a name. |

**D10, the texts:**

```
opening:  (moa · the user is here for the first time today, not typed by the user)
          Open the day: brief the user as a thread's first message does (see "How to talk" in your instructions).
yes:      (moa · the user said yes to your suggestion(s), not typed by the user)
          - #12: Kite has 3 overdue tasks blocked on the domain; move them to Friday?
          Do what each one proposed.
due:      Due by tomorrow (end your reply with these lines as they are):
          ⏰ 2 days overdue: Reply to Kite; Renew the domain
          ⏰ 1 day overdue: Send the invoice
          ⏰ Due today: Merge PR #3
          ⏰ Due tomorrow: Book the venue
idle:     Idle terminals (untouched for 2 h or more; offer to close one that looks forgotten, as a suggestion with --do "term close ID"):
          - #4 kite · ~/private/kite · idle 5 h · at its prompt
          - #7 logs · ~/private/lantern · idle 3 h · running tail
```

- **Due lines:** titles one line, cut to 80 characters; tasks with the same lateness share a
  line, joined by `; `; when tasks were left out, a last line `(and N more: moactl task ls)`.
- **Idle lines:** `idle N h` in whole hours, rounded down; `at its prompt` when its command is
  its shell, else `running <command>`; the folder through `with_tilde`; name and command one
  line, cut to 40 characters.
- **Offers:**
  - one session: `<title> is closed. Close its terminal <name> too?` or `… Close its N
    terminals too (<a>, <b>, <c>…)?` (`is archived` for an archive);
  - several sessions: `You archived N sessions with open terminals. Close their N terminals
    too (<a>, <b>, <c>…)?`;
  - at most 3 names, then `…`.
- **Console** (moa-view):
  - **An open or later suggestion:**
    - the head `✦ moa suggests` (Accent), with ` · later` (Dim) for a later one;
    - the text as Markdown in the card gutter;
    - when it has actions, a dim `yes runs: <action>; <action>`; with invalid actions, a dim
      `yes can't run its actions`;
    - the buttons.
  - **An answered one:** `✓ yes: <text>` (Ok), with a dim `but: <result>` when the result
    isn't `done`, `waiting for moa` or `told moa`; `✗ no: <text>` (Dim).
  - **An unknown status:** `✦ suggestion #N: <text>` (Dim).
  - **Lines under an automatic row's header:**
    - `· moa opens the day` for `opening`;
    - `· moa acts on your yes to #12` (`to #12 and #14`, `to #12, #14 and #15`) for `yeses`;
    - 6c's `moa is reading N session reports…`.
  - **Lines under your own row:** `· opens the day` and `· with your yes to #12`, beside 6c's
    `· with N session reports`.

## Global Constraints

The roadmap's constraints apply, as do every earlier slice plan's and 6a's to 6c's. Also:

- **Reuse before writing.** Search the workspace before adding a helper. A near-copy is an
  **Important** finding, and each report lists what it reused. In particular: one thread
  chooser for cards (`card_thread`), one `suggest` path for offers, one gate check for every
  automatic source.
- **Building and verifying:**
  - cargo only through `nix develop --command cargo …`, with `cargo nextest run`;
  - `scripts/verify quick` before every commit, and the full `scripts/verify` once per PR;
  - never run cargo while an e2e test runs;
  - test counts are quoted from `target/verify/nextest.log`, never from memory.
- **The core stays pure:** time is `Now`. No new timer, and nothing armed while nothing waits
  (spec U5: no polling). No moa turn at a set time (U18).
- **Exactly once, at every step:** a report, a yes, a due task (per day) and the day's
  opening are marked delivered only in the commit that saves the row carrying them. A refused
  commit sends nothing and marks nothing.
- **moa acts only on a yes.** Nothing new runs an action, closes a terminal or switches an
  account without your answer.
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
- **Log lines never hold record contents:** no task title, suggestion text or terminal name.
- **Plain words** in everything the user sees: "moa", "moa's agent", "thread", "session
  report", "wake-up", "suggestion". Never "ACP", "adapter" or "nag".

## Review Focus

1. **The brief twice, or never:**
   - **The inputs:** a burst of 20 presence events; presence while moa answers a wake-up; the
     daemon restarted at 09:00 after a brief at 08:00; presence at 23:59 with moa busy until
     00:01; writes refused when the brief would go.
   - **Expected:** one brief per local day. It waits for moa to be idle. A restart the same
     day doesn't brief again. A refusal keeps it due.
   - **Pinned by:** `presence_bursts_brief_once`, `the_brief_waits_for_moa`,
     `a_restart_doesnt_brief_again`, `a_refused_brief_stays_due` (Task 6).
2. **A yes answered twice, or while moa can't hear it:**
   - **The inputs:** yes from the console and the today view on one suggestion; a yes while
     moa answers, while its agent restarts, after a failed start, then a daemon restart
     before it goes.
   - **Expected:** the second answer is refused (`suggestion #N is already accepted`). The yes
     reaches moa once, when it can.
   - **Pinned by:** `a_yes_reaches_moa_once` (proptest) and `a_yes_waits_for_moa` (Task 3).
3. **Odd due dates:**
   - **The inputs:** a task 300 days overdue; a done task with a past due; a due date moved
     during the day; 50 tasks due; a title with a newline and ANSI escapes.
   - **Expected:** a moved date is told again for its new date; done tasks never are; 20 per
     prompt, the rest next time; every due line is one line.
   - **Pinned by:** `the_due_list_is_bounded_and_clean`, `a_moved_date_is_told_again` and
     `nags_are_once_a_day` (proptest) (Task 5).
4. **A card whose suggestion isn't there, or can't run:**
   - **The inputs:** a `suggestion` row whose id isn't in the topic; actions flagged invalid
     by startup; a status a newer build wrote; a 5 000-character text at width 40.
   - **Expected:** drawn degraded, never breaking the frame; no `yes` button for invalid
     actions.
   - **Pinned by:** `odd_suggestion_cards_draw` (Task 2).
5. **`term close` from a yes when the terminal moved on:**
   - **The inputs:** the terminal already closed; already closing; now running a program; the
     link to tmux down.
   - **Expected:** the yes records the failure in its result (`but: …` on the card); a running
     program gets the gentle close (Ctrl-c, then the kill 3 s later).
   - **Pinned by:** `a_yes_closes_terminals_gently` (Task 1).

---

## Task 1: terminals moa opens, and `term close` on a yes

**Files:** `crates/moa-core/src/model.rs` (`ACTION_VERBS`); `crates/moa-proto/src/cli.rs`
(`parse_action`'s `term` check); `crates/moa-core/src/ops/term.rs` (`Tell::Quiet`,
`opened_by` through `Asked::Term` and `record_opened`); `crates/moa-core/src/tmux/` (where
`Asked::Term` is defined); `crates/moa-core/src/ops/suggestions.rs` (`run_action` runs
`TermClose`); `crates/moa-core/src/core.rs` (`TermOpen` passes the caller); tests in
`crates/moa-core/tests/terminals.rs`, the suggestion tests' file, and
`crates/moa-proto/tests/` (the grammar's tests).

**Interfaces:**
- Produces: `ACTION_VERBS` with `term`; `Tell::Quiet`; `term(state, tell, path, title,
  opened_by: OpenedBy)`; `TermClose` runnable from a suggestion's yes (D8).

- [ ] **Step 1: Failing tests:**
  - `moa_opens_terminals_as_moa`: `term` from `Caller::Moa` records `opened_by: moa`; from a
    session's caller and from an unknown caller, `you`.
  - `a_suggestion_may_close_terminals`: `suggest "…" --do "term close 4"` is accepted;
    `--do "term ~/private/kite"` is refused with `a suggestion can run term only as term
    close`.
  - `a_yes_closes_terminals_gently` (Review Focus 5): yes on `term close 4` with terminal 4 at
    its prompt sends the kill after the answer's commit; running `tail`, Ctrl-c, then the kill
    3 s later. Already closed: the result is `term close 4: that terminal is gone`. Already
    closing: `done`, and nothing more is sent. Link down: the result says why. The activity
    has `closed terminal <name>`.
  - `a_refused_yes_closes_nothing`: with writes refused, the yes is refused and no tmux query
    goes.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D8 and D9's `opened_by`.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa's terminals say so, and a yes can close one`.

## Task 2: suggestion cards

**Files:** `crates/moa-core/src/central/mod.rs` (`card_thread`, from `account::thread`);
`central/account.rs` (uses it); `central/feed.rs` (`kind::SUGGESTION`, `suggestion_body`);
`ops/suggestions.rs` (`suggest` writes the row); `crates/moa-view/src/views/console.rs`
(draws it); tests in the suggestion tests' file, `crates/moa-view/tests/` (the console's),
with insta snapshots.

**Interfaces:**
- Consumes: Task 1 (`term close` shows in `yes runs:`).
- Produces: `central::card_thread(state: &State) -> Option<String>`;
  `feed::{kind::SUGGESTION = "suggestion", suggestion_body(id: SuggestionId, text: &str) ->
  String}`.

- [ ] **Step 1: Failing tests:**
  - `a_suggestion_is_carded`: `suggest` from moa's caller during a turn writes a `suggestion`
    row `{"id", "text"}` in the turn's thread, in the suggestion's own commit; a duplicate
    writes none.
  - `a_card_goes_to_the_newest_thread`: no open thread → the newest saved; no thread at all
    → no row, and the suggestion is still saved.
  - `the_account_card_still_finds_its_thread`: 6c's account-card tests pass unchanged.
  - Console snapshots at widths 40 and 80: open with actions; later; open with no actions;
    yes `done`; yes with a failure (`but: term close 4: that terminal is gone`); no.
  - `answering_anywhere_redraws_the_card`: the same feed, with the topic row changed from
    open to accepted, draws `✓ yes: …` and no buttons.
  - `the_card_buttons_answer`: Tab to `[later]`, Enter → the command `/suggestion later 12`.
  - `odd_suggestion_cards_draw` (Review Focus 4): id not in the topic; `actions_invalid` set
    (no `[yes]`); an unknown status; 5 000 characters at width 40. No line is wider than its
    width.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D7 and D10's console card texts.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa's suggestions are cards in the console`.

## Task 3: a yes with nothing to run reaches moa

**Files:** `crates/moa-core/src/ops/suggestions.rs` (`YES_WAITING`, `YES_TOLD`, the answer);
`crates/moa-core/src/state.rs` (`yeses`); `central/channel.rs` (`Source::Yes`, the gates
shared by every automatic source); `central/mod.rs` (`prompt` carries yeses, D1);
`central/reports.rs` or a new `central/carry.rs` (the yes block, D10); `constitution.md`;
`crates/moa-view/src/views/console.rs` (the `yeses` lines); tests in
`crates/moa-core/tests/central_reports.rs` (with its proptest) and the console's.

**Interfaces:**
- Consumes: Task 2.
- Produces: `suggestions::{YES_WAITING = "waiting for moa", YES_TOLD = "told moa"}`;
  `State::yeses() -> impl Iterator<Item = &Suggestion>`; `Source::Yes`;
  `channel::may_send(state: &State) -> bool` (D3's gates, used by every automatic source;
  6c's wake check adds its `/auto` test to it).

- [ ] **Step 1: Failing tests:**
  - `a_yes_without_actions_tells_moa`: yes on #12 (no actions) replies `yes; moa acts on it
    when it's free`, saves `result: waiting for moa`, and, moa idle, sends an automatic
    prompt with D10's yes block. Its commit saves `told moa`; the row's body has `yeses:
    [12]`.
  - `a_yes_waits_for_moa` (Review Focus 2): a yes while moa answers goes at the turn's end; a
    yes while restarting goes once the thread reopens; after a failed start it rides on your
    next prompt.
  - `a_yes_goes_with_auto_off`.
  - `your_prompt_carries_yeses`: a waiting yes, then `console.prompt "hi"` → one commit,
    `yeses: [12]`, the text `<yes block>\n\n---\n\nhi`.
  - `a_wake_carries_yeses_too`.
  - `a_yes_with_actions_tells_moa_nothing`.
  - `a_yes_reaches_moa_once` (proptest): extend 6c's exactly-once proptest with "answer yes",
    so every yes appears in exactly one sent prompt across restarts and refused commits.
  - Console: an automatic row with `yeses: [12, 14]` draws `· moa acts on your yes to #12 and
    #14`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D5, D1's yes part, and the constitution's line:** under "Be
  proactive", `When the user says yes to a suggestion without --do, a prompt tells you so: do
  what it proposed.`
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: a plain yes reaches moa through the channel`.

## Task 4: close or archive offers to close the terminals

**Files:** `crates/moa-core/src/ops/sessions.rs` (`close`, `archive`); a helper in
`ops/suggestions.rs` or `terminals.rs` (the offer's text, D10); `constitution.md` (the action
list gains `term close`; idle terminals, below); `docs/backlog.md` (the terminals entry:
`opened_by = moa`, idle terminals and the close offers are built in 6d; pruning stays with
Q1); tests in `crates/moa-core/tests/terminals.rs`
or `sessions.rs`.

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: `terminals::of_sessions(state: &State, ids: &[SessionId]) -> Vec<&Terminal>` (open
  records whose `session_id` is one of `ids`, by id).

- [ ] **Step 1: Failing tests:**
  - `archiving_offers_to_close_its_terminals`: archive Lantern with splits `kite` (#4) and
    `logs` (#7) → one suggestion `Lantern · Rust scaffold is archived. Close its 2 terminals
    too (kite, logs)?` with actions `term close 4`, `term close 7`, and its card.
  - `archiving_several_offers_once`: two sessions, three terminals → D10's several-sessions
    text.
  - `a_close_that_kills_the_agent_offers_its_splits`.
  - `a_close_that_leaves_the_tab_offers`: the agent already left; its split is still open.
  - `no_terminals_no_offer`; `archiving_again_offers_nothing_new` (the duplicate check).
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D9's offers, and the constitution's lines:** in "Be proactive", the
  action list reads `(task, set, archive, close, group, note, project or term close)`; a new
  bullet: `Idle terminals come with the day's opening and wake-ups. Offer to close one only
  when it looks forgotten, as a suggestion with --do "term close ID"; never close one
  unasked.`
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: closing a session offers to close its terminals`.

## Task 5: what's due, and idle terminals, ride on wake-ups

**Files:** new `crates/moa-core/src/central/due.rs` (D4) and `central/idle.rs` (D6);
`central/mod.rs` (`prompt` carries them on wakes, D1); `constitution.md` (the nag paragraph);
tests in a new `crates/moa-core/tests/central_due.rs` and `central_reports.rs` (proptest).

**Interfaces:**
- Consumes: Task 3 (D1's carrying order).
- Produces: `due::{DUE_MAX = 20, due_now(state, now) -> Vec<&Task>, block(state, now,
  &[&Task]) -> String}`; `idle::{IDLE_MAX = 10, idle_terms(state, now, since:
  Option<UtcMs>) -> Vec<&Terminal>, block(state, now, &[&Terminal]) -> String}`.

- [ ] **Step 1: Failing tests:**
  - `the_due_block`: insta snapshot of D10's due block from tasks 2 days overdue (two), 1
    day overdue, today and tomorrow; a task due in 2 days and a done one are left out.
  - `a_wake_carries_whats_due`: a wake-up's prompt holds the due block after the wake text,
    and its commit saves a `Nag` per task for today.
  - `nags_are_once_a_day`: a second wake the same day carries no due block; the next day's
    first wake carries it again.
  - `a_moved_date_is_told_again` (Review Focus 3).
  - `due_tasks_dont_wake_moa`: due tasks and no reports arm no `Wake`.
  - `your_prompt_carries_nothing_due` (U18).
  - `the_due_list_is_bounded_and_clean` (Review Focus 3): 50 due tasks → 20 lines' worth and
    `(and 30 more: moactl task ls)`; a title with a newline and ANSI escapes is one clean line.
  - `the_idle_block`: snapshot of D10's idle block; `terminal_idle_ms = 0` gives none.
  - `a_wake_tells_only_newly_idle_terminals`: a terminal idle before the last wake isn't in
    the next one's block; one that went idle after it is.
  - The proptest gains "a task becomes due" and "the day turns": each (task, due, day) is in at
    most one sent prompt.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D4, D6, D1's due and idle parts for wakes, and the constitution's
  paragraph,** replacing "**Nag** about tasks due by tomorrow once a day each…": `**Due
  dates:** a wake-up or the day's opening may end with a "Due by tomorrow" list from moa.
  End your reply with its lines as they are. If a report shows one is done, close it instead
  and leave its line out.`
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: what's due and idle terminals ride on wake-ups`.

## Task 6: the opening brief

**Files:** `crates/moa-tmux/src/conf.rs` (`client-active` in `HOOK_TABLE`);
`crates/moa-core/src/tmux/mailbox.rs` (`HookMsg::ClientActive`); new
`crates/moa-core/src/central/opening.rs` (`present`, `BRIEFED_KEY = "moa.briefed"`, the
opening text); `central/channel.rs` (`Source::Brief`, first among the sources);
`central/mod.rs` (`Moa.brief_due`; `prompt` carries the opening, D1); `core.rs` (D2's call
sites); `constitution.md`; `crates/moa-view/src/views/console.rs` (the opening lines); tests
in a new `crates/moa-core/tests/central_opening.rs`, `crates/moa-core/tests/tmux_vocab.rs`,
`crates/moa-tmux/tests/conf.rs`, and the console's.

**Interfaces:**
- Consumes: Tasks 3 and 5.
- Produces: `opening::{present(state: &mut State, now: &Now), BRIEFED_KEY}`;
  `Source::Brief`; `Moa.brief_due: bool`.

- [ ] **Step 1: Failing tests:**
  - `the_first_attach_of_the_day_briefs`: briefed yesterday; `moa:hook client-attached` at
    08:00 → an automatic prompt that starts with D10's opening, carries the due and idle
    blocks, and opens a new thread whose system prompt has today's brief. Its commit sets
    `moa.briefed` to today. The row's body has `opening: true`.
  - `presence_bursts_brief_once` (Review Focus 1): 20 presence inputs of every D2 kind →
    one brief.
  - `the_brief_waits_for_moa`: presence while a wake-up's turn runs → the brief goes at the
    turn's end.
  - `a_restart_doesnt_brief_again`: `moa.briefed` is today → presence does nothing.
  - `a_refused_brief_stays_due`: writes refused → nothing sent, `brief_due` stays, and the
    brief goes once writes are back.
  - `midnight_itself_does_nothing`: crossing midnight with no presence sends nothing.
  - `your_first_prompt_opens_the_day`: presence, then your prompt before the brief goes (moa
    busy) → no separate brief; your prompt carries the opening and due blocks, and sets
    `moa.briefed`.
  - `the_brief_goes_with_auto_off`.
  - `moas_own_ops_arent_presence`: `focus` from `Caller::Moa` leaves `brief_due` false.
  - `the_brief_carries_reports_and_yeses`: one commit marks them all.
  - Mailbox: `moa:hook client-active /dev/ttys1` parses to `HookMsg::ClientActive`. Conf:
    `client-active[90]` is set and unset with the others.
  - Console: an automatic row with `opening: true` draws `· moa opens the day`; your row
    with it draws `· opens the day`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement D2, D3 and D10's opening text, and the constitution's lines:**
  in "What you're told": `The first time the user is here each day, moa sends a prompt that
  opens the day: answer it as a thread's first message.`
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: moa opens the day when you show up`.

## Task 7: the stub scenarios

**Files:** `crates/moa-daemon/tests/central.rs`; `tests/common/rig.rs` (a `mailbox` helper if
there is none, to send `moa:hook client-attached`).

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Failing tests** (real daemon, `stub-acp`, `FakeTime`):
  - `an_opening_brief_on_the_first_attach_after_midnight` (the roadmap's "done when"): a
    brain with a thread and a brief yesterday; time moves past midnight; nothing goes. Then a
    `client-attached` mailbox line: the stub logs a `session/new` whose system prompt holds
    today's brief, then a prompt starting with the opening line; `moa.briefed` is today.
  - `a_restart_the_same_morning_doesnt_brief_again`.
  - `whats_due_is_told_once`: a task due today; two wake-ups → the due block in the first
    only; a daemon restart, then a third → none.
  - `a_plain_yes_reaches_moa`: `suggest` from moa's caller, `/suggestion yes N` from the
    console's connection → the stub logs a prompt with the yes block.
  - `archiving_offers_to_close_terminals`: a session with a split terminal (as 5d's terminal
    tests make one), archived → a suggestion with `term close`; yes → the terminal closes.
- [ ] **Step 2: Run them.** Expected: FAIL where the wiring is missing, else PASS at once
  (say which in the report).
- [ ] **Step 3: Fix what fails.**
- [ ] **Step 4: Run them again.** Expected: PASS.
- [ ] **Step 5: Commit.** `v1: 6d's stub scenarios`.

## Task 8: end to end in the cockpit

**Files:** `crates/moa/tests/console_e2e.rs`.

- [ ] **Step 1: Failing tests** (the real binary in a tmux sandbox, `stub-acp`):
  - `a_suggestion_card_answered_with_a_button`: the stub's turn runs `moactl suggest
    "Archive Kite?" --do "archive <id>"`; the card shows `✦ moa suggests` and `[yes]
    [later] [no]`; Tab to `[yes]`, Enter → `✓ yes: Archive Kite?`, and the session is
    archived.
  - `a_card_answered_elsewhere_updates`: `moa suggestion no N` from the command line → the
    console's card shows `✗ no: …`.
  - `attaching_opens_the_day`: a sandbox whose `moa.briefed` is yesterday; attaching →
    `· moa opens the day` under `◆ moa`, and the stub's answer below.
- [ ] **Step 2: Run them.** Expected: FAIL where the wiring is missing.
- [ ] **Step 3: Fix what fails.** Each test waits on something visible and checks an effect
  that would vanish with the feature; make each fail once on purpose.
- [ ] **Step 4: Run them again,** 20 times under load. Expected: PASS every time.
- [ ] **Step 5: Commit.** `v1: 6d end to end in the cockpit`.

## Try it (p2's body)

With the real adapter (your own run), in a sandbox:
1. In a fresh sandbox, give a task `--due today`, then detach and attach: moa opens the
   day, ending with `⏰ Due today: …`.
2. Ask moa to suggest something with no action, and press `[yes]`: moa acts on it.
3. Open a split terminal in a session's tab, then `/archive` the session: a card offers to
   close it.
