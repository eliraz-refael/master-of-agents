# moa v1 slice 7: partners (peers and asks)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sessions and moa can ask each other and wait for the answer. `moactl ask kite
"…"` types the question into Kite's pane when Kite is free, and Kite's next reply comes back
as the answer. moa takes asks between its turns, and you see each one as a card. The daemon
runs the whole protocol: nobody loops, nothing new is launched, and every failure ends in a
clear answer.

**Architecture:**
- **One new core module, `moa_core::asks`.** It owns the state machine (pending → sent →
  received → answered | failed | dropped), the parked requests, and one timer
  (`TimerKey::Asks`). `asks::run` runs at the end of every input, as `channel::run` does:
  - it delivers to a session that is free;
  - it fails a `sent` ask with no receipt;
  - it answers parked requests whose wait ran out.
- **Delivery to a session is typing into its pane** through the link's control client:
  `set-buffer`, then `paste-buffer -p -d`, then `send-keys Enter` 0.5 s later. That's zero
  new processes. The receipt is the session's next `turn.started` holding the marker. The
  answer is its next Stop's full final message.
- **Delivery to moa is a channel source,** `Source::Ask`. The hidden prompt carries the ask
  alone. Its turn's text is the answer.
- **Rows are the truth.** Memory holds only the parked requests and the pending Enter keys,
  so a restart loses no ask. A `sent` ask whose Enter was lost fails at its receipt deadline.

```
 moactl ask kite "…" ─► Op::Ask ─► Ask row (pending) + parked request
 end of every input ─► asks::run │ kite free ─► claim (sent) + set-buffer, paste-buffer -p -d ─► 0.5 s ─► Enter
 kite turn.started with "[moa · message from … · ask #N]" ─► received
 kite Stop (full last message) ─► answered ─► parked request replies
 ask to moa ─► channel Source::Ask ─► card + hidden prompt ─► moa's turn text ─► answered
```

**Tech Stack:** Rust; the existing crates. No new dependencies. No schema change: the
`asks` table, `model::Ask`, `Change::Ask`, `State.asks` and `Topic::Asks` exist since slice
2.

**Spec:** `docs/superpowers/specs/2026-11-15-moa-rust-design.md` §6.12 ("Peers and asks"),
§6.6 ("Typing into a pane"), lessons 27 and 46–53, and §6.11 for the channel. The slice 6
design's channel rule applies: `docs/superpowers/specs/2026-11-22-moa-slice-6-central-agent-design.md`
("asks join 6c's channel"). Roadmap: "Slice 7: partners". Builds on slices 0–6d (#33–#98).

## Builds on (names to use)

- **Data:**
  - `model::{Ask, AskId, AskStatus, AskTarget}` (`AskTarget::{Moa, Session(SessionId)}`);
  - `Change::Ask`, `State.asks`, `Tx::next_id(Table::Ask)`, `Topic::Asks`;
  - `sleep::in_an_ask` already keeps a session in an open ask awake.
- **Grammar:** `moa_proto::cli::Verb::{Peers, Ask {to, text, timeout}, Reply {id, timeout},
  Asks}`. Today `Verb::to_op` refuses them with `VerbError::NotYet`. `moactl` has a global
  `--json`.
- **Ops:**
  - **Wiring:** `Op`, `Op::writes`, `Op::method` (`crates/moa-core/src/op.rs`), and
    `ops::run`.
  - **Parked replies:** the special `decide` arms in `core.rs` for ops that reply later
    (`TermLs` and `ops::term::ls_answers`; `central::Waiter`), `Effect::Reply`, and
    `Input::ClientGone`.
- **Callers:** `State::caller` → `Caller::{Moa, Session(id, handle), Unknown}`,
  `State::handle_of`, `State::candidates` / `resolve` (`names.rs`), `live::spike_state`,
  `State::binding` (`PaneBinding.pane_id`).
- **Hooks:**
  - **Paths:** `hooks::run` / `apply` / `applied_changes`, and `HookKind::{TurnStarted
    {prompt}, TurnEnded {last_message}, SessionEnded}`.
  - **Normalizing:** the Claude normalizer `moa-agents/src/claude/normalize.rs`, and
    `live::cut_middle`.
  - **Reports:** `reports::queued`.
- **tmux:**
  - **Commands:** `tmux/cmd.rs` (`Cmd`, `ALLOWED`, `check`, `quote`, `Cmd::interrupt`), and
    `Tx::tmux`.
  - **Timers:** `effect::rearm`, `TimerKey`, `Effect::{SetTimer, CancelTimer}`.
- **The channel:**
  - **`central/channel.rs`:** `Source`, `due`, `run`, `may_send`, `free`.
  - **`central/mod.rs`:** `send`, `prompt`, `Turn`, `card`, `card_thread`, `failed_wake`.
  - **The rest:** `central/turn.rs` `ended` (its `moa's turn ended early: <stop>`),
    `central/watchdog.rs` `due`, and `central/constitution.md` ("Partners").
- **SessionStart:** `peers::peers_note` (returns `None` today), called by `hooks::run`; `moa
  hook` prints it as `additionalContext`. `claude::plugin::session_plugin` writes the
  injected plugin.
- **Display:** `ops::term::with_tilde`, `time::span`, `text::clean`.
- **Tests:**
  - **Core:** `crates/moa-core/tests/common/{mod,world,central}.rs`, `handle_checked`, and the
    proptests in `tests/sleep.rs` and `tests/central_reports.rs`.
  - **Daemon:** `crates/moa-daemon/tests/{central.rs,tmux.rs}`, `common/{rig.rs,hooks.rs,mod.rs}`
    (`create_server`, `Up::mailbox`, `Rig::meta`, `Rig::write`), and `FakeTime`.
  - **e2e:** `crates/moa/tests/{hooks_e2e.rs,console_e2e.rs}`, including
    `open_and_hooks_launch_nothing_in_the_daemon` (the launches check).
  - **testkit:** `fake-claude` (`FakeStep`, `write_script`), `stub-acp` (`StubStep`).

## Calls for you to check

The most important come first.

1. **When moa asks a session, the watchdog waits (A10).** moa's turn is blocked in `moactl
   ask` for up to the ask's wait. The 5-minute watchdog would cancel it, so while an ask from
   moa is open the watchdog holds. It restarts its count when that ask ends.
2. **An ask to moa carries only the ask (A9).** Session reports, yeses and the opening brief
   wait for moa's next prompt, so moa's reply to the asker isn't mixed with them. Asks go
   first among the automatic prompts. `/auto off` doesn't stop them, since a session is
   waiting.
3. **The Stop that answers an ask doesn't queue a session report (A7).** That turn answered
   someone; it isn't new work. The answer is in `moactl asks`.
4. **Only sessions and moa can ask (A2).** From a plain shell, `moactl ask` is refused with
   `ask works from a session or moa; type into the session yourself`.
5. **A late receipt revives the ask (A6).** If the 20 s receipt failed but the marker turns
   up later, the ask becomes `received` again, and `moactl reply ID` gets its answer.
6. **Limits (A2):**
   - an ask's text is at most 8000 characters;
   - its wait is 1 to 3600 s, 540 s by default;
   - control characters other than newline and tab are removed from the text, so the paste
     can't be ended early.
7. **The `moa-peers` skill ships in moa's injected plugin now (A12).** Slice 8's account
   plugin reuses the same text.
8. **"Last asked" in `peers` is the newest ask to that peer, from anyone (A11).**
9. **Stale asks fail at 2 × 540 s after the claim (A5),** because the table keeps no
   per-ask wait. No schema change.

## PRs (roadmap P4)

Each PR branches from `main` after the previous one has merged. Each gets:
- per-task reviews;
- one `/code-review medium --fix` over its branch;
- `scripts/verify`;
- green CI.

| PR | Branch | Tasks | Title |
|---|---|---|---|
| p1 | `v1/slice-7-1` | 1–3 | `v1 slice 7 (1): asks between sessions` |
| p2 | `v1/slice-7-2` | 4–5 | `v1 slice 7 (2): asks to moa, the peers note` |
| p3 | `v1/slice-7-3` | 6–7 | `v1 slice 7 (3): the scenarios, end to end` |

p1 also carries this plan. The branch `v1/slice-7` holds the plan until p1 starts.

## Calls this plan makes

| # | Call | Why |
|---|---|---|
| A1 | **Statuses:** `pending` (saved, not yet typed), `sent` (claimed and typed; `claimed_at` set), `received` (the marker came), `answered` (`reply` holds the full answer, `answered_at` set), `failed`, `dropped`. For `failed` and `dropped`, `reply` holds the reason (one of A13's reason texts). There's no reason column, and the asker reads the reason the same way. | Spec 6.12; no schema change. |
| A2 | **`ask TO TEXT [--timeout S]`** (`Op::Ask { to, text, timeout_s }`, writes): **(a) Caller.** A session or moa. `Caller::Unknown` is refused. **(b) Text.** `TEXT` is cleaned: control characters other than `\n` and `\t` are removed, then it's trimmed. An empty text is refused; so is one over `ASK_TEXT_MAX = 8000` characters. **(c) Wait.** `timeout_s` must be in `1..=3600`. **(d) Target.** `TO` resolves through `State::peer(q) -> Result<AskTarget, OpError>`: `moa` means moa. Otherwise it takes `candidates(q)` and keeps the live ones (A3). Exactly one live candidate is the target. With no live candidate and exactly one candidate, that one is the target, and it's refused as unreachable. More than one live candidate is ambiguous, with `resolve`'s listing text. **Amended (code review):** with no live candidate and more than one candidate, `no live session matches '<q>'`. **(e) Refusals, in order:** yourself; unreachable (for moa: `moa isn't running` while its agent can't take a turn without the user, A9 (f)); the deadlock guard (the target has an open ask, `pending`/`sent`/`received`, to the caller). **(f) Saving.** One commit: the `Ask` row (`at` now, `from_session` the caller, `from_name` its label (A11), `to_session`, `text`, `pending`). The request is parked (A4). | Spec 6.12's refusals and its "names prefer live sessions over ended ones". |
| A3 | **Live and free.** A session is **live** when it isn't archived, isn't `Location::Elsewhere`, isn't `Lifecycle::Ended`, isn't asleep, and `State::binding(id)` is `Some`. A live session is **free** when its `activity` is `Idle` or `Failed` and it has no ask in `sent`/`received`. **Amended (code review):** and its last ask didn't fail `no receipt` since moa last heard any hook event from it (held in memory, `AsksMem.unheard`; a restart forgets it), so a pane that never answers doesn't take the asks queued behind. `State::is_free` is the one definition; a claim layers its stale asks on it (`is_free_once`). | "Delivery when the peer is idle"; one ask in flight per peer. |
| A4 | **Parked requests:** `asks::Parked { conn, req, ask: AskId, until: UtcMs, kind: Waits }`, with `Waits::{Ask, Reply}`, in `State.asks_mem.parked: Vec<Parked>`. **(a) When the ask ends** (answered, failed or dropped), every request parked on it replies (A13). **(b) At `until`:** a `pending` ask is dropped (`stayed busy`), and its requests reply with an error. A `sent` or `received` ask stays open, and its requests reply `Ok` with A13's "no reply yet" text, which exits 0. **(c) `Input::ClientGone(conn)`** removes that connection's entries and nothing else. **(d) `reply ID [--timeout S]`** (`Op::Reply`, no write): an ended ask replies at once; an open one parks. `no ask #N` when there's none. **Amended (code review, R3 amended):** a `pending` ask keeps its own wait in memory (`AsksMem.waits`; after a restart, `at + ASK_WAIT_DEFAULT_MS`) and is dropped at it even when its asker's request went away (`ClientGone`), so a question whose Bash call was killed isn't typed hours later. A `reply`'s wait never drops one; `Waits` is gone. A drop or receipt failure that falls due while writes are refused is armed for when they come back, never lost. | Spec: "Timeout while pending → dropped; while sent/received → no reply yet, exit 0". |
| A5 | **Delivery** (`asks::run`, at the end of every input, for asks to sessions). **(a) What goes.** For each target, the oldest `pending` ask is delivered when the target is free. **(b) Stale asks first.** `sent`/`received` asks to that target with `claimed_at + 2 × ASK_WAIT_DEFAULT_MS` (`ASK_WAIT_DEFAULT_MS = 540_000`) in the past are failed (`stale`) in the same commit. **(c) The claim.** One commit sets `status = sent` and `claimed_at = now`. After it: `Cmd::set_buffer(&name, &framed)`, then `Cmd::paste_buffer(&name, pane)`, where `name` is `moa-ask-<id>`. The Enter is due at `now + ASK_ENTER_MS` (500), held in `asks_mem.enter: BTreeMap<AskId, (PaneId, UtcMs)>`. **(d) The Enter.** When it fires it sends `Cmd::enter(pane)`. **(e) A refused commit** types nothing. **Amended (Task 3 review):** delivery waits for the link to be up: a claim with no link would save `sent` for text that reached no pane. Endings that a hook or a tab's exit brings (A6, A8) use `ending`, the row builder `asks::end` uses, in that event's own commit, and `run` replies to their waits at the end of the same input. | Spec 6.12, 6.6. |
| A6 | **Receipt and answer** (in `hooks.rs`, from `applied_changes`, in the hook's own commit). **(a) The marker.** `asks::marker_id(prompt: &str) -> Option<AskId>` finds `[moa · message from ` … `· ask #<N>]` anywhere in the prompt. **(b) The receipt.** A `TurnStarted` whose marker names an ask to *this* session that is `sent` (or `failed` with reason `no receipt`) makes it `received`. **(c) The answer.** That session's next `TurnEnded` makes a `received` ask `answered`, with `reply` set to the full `last_message` (an empty one: `(no text)`). **(d) No receipt.** In `asks::run`, a `sent` ask with `now ≥ claimed_at + ASK_RECEIPT_MS` (20 000) fails `no receipt`. **(e) Full texts.** The Claude normalizer stops cutting `prompt` and `last_message`. The core still cuts where it stores (history rows, `activity_why`), as today. **Amended (Task 3 review):** a late receipt revives a `no receipt` failure only while the receiver is live, has nothing else in flight, has no open ask to the asker (A2 (e) holds for a revived ask), and before the ask would be stale (`claimed_at + 2 × ASK_WAIT_DEFAULT_MS`). A received ask ends with its turn: a `TurnFailed` fails it (`<label>'s turn failed`), and a `TurnStarted` whose prompt doesn't hold its marker, or a `SessionStart` that begins a new conversation in the tab (`/clear`, resume, startup, fork), fails it (`<label> moved on before answering`). A compaction leaves it: one can come mid-turn, and the turn carries on to its Stop. So a Stop answers only the ask its own turn took, and A7 skips only that Stop. | Spec: "matched anywhere in the full prompt", "the full final message"; lesson 27. |
| A7 | **A Stop that answers an ask** queues no session report. `reports::queued` skips it. | Call 3. |
| A8 | **A peer that dies.** Its `SessionEnded` or its pane gone ends its open asks (sleep can't: `sleep::in_an_ask` keeps a session in an open ask awake): `pending` ones are dropped and `sent`/`received` ones fail, both with `<label> ended`. Its own asks to others stay. An asker's death removes nothing; the answer is still saved. **Amended (Task 3 review):** a `pending` ask is dropped `<label> isn't running` (in flight: `<label> ended`). A catch-all in `asks::run`, while the link is up, ends every open ask to a session that isn't live (A3) the same way, in a commit of its own: close, sleep by name, agent-left, adopt, archive, reconcile, and anything added later. Before the link is up (a daemon start before reconcile) it waits. | Spec: "Peer died → dropped or failed". |
| A9 | **Asks to moa:** `Source::Ask(AskId)`. **(a) When it goes.** `channel::due` puts it first: the oldest `pending` ask to moa, due at once, under the same gates. `/auto` isn't a gate. **(b) The prompt.** It carries only the ask block (A13), and its `you` row is `automatic` with body key `ask: {"id", "from"}`. **(c) Its commit** saves the row, sets the ask `received` with `claimed_at`, and writes an ask card in `card_thread` with `what = "ask"`. **(d) Its turn's end:** `EndTurn` makes the ask `answered` with the turn's joined text (what `turn::ended` replies). Any other stop fails it with `moa's turn ended early: <stop>`; an error fails it with `moa's turn failed: <error>`. **(e) An orphan.** An ask to moa in `sent`/`received` with no live `Turn` whose source is `Ask(that id)` fails `moa restarted before answering`. **Amended (Task 4):** the card goes in the prompt's own thread, before its row: `card_thread` before that commit would miss a thread the commit itself saves (the first prompt of a new one). An empty `EndTurn` answers `(no text)`, as a session's does. A turn whose agent went (`exited`) isn't live: its end can't answer any more. The orphan check runs in `asks::run` while writes are allowed. **Amended (Task 4 review, R13):** **(f) moa can't answer.** While moa's agent can't take a turn without the user (`central::can_answer` is false: `Phase::Failed`, or no account it can run on; busy or starting can), an ask to moa is refused (A2) and a `pending` one is dropped, both `moa isn't running` (A13's form, no resume hint), in `asks::run` with the orphans, rather than waiting out its wait. `asks.rs` reads moa's turn only through `central::answering` and `central::can_answer` (R14). | Roadmap: "between turns, a card plus a hidden ask prompt". |
| A10 | **The watchdog holds while moa waits.** `watchdog::due` is `None` while an ask from moa is open. When that ask ends, the watchdog counts its end as news. **Amended (Task 4):** "moa waits" is a request (`moactl ask`, or a `reply` that waits again) parked on an open ask from moa (`asks::moa_waits`), so an ask whose `moactl` was killed, or that ran out to "no reply yet", doesn't hold a later turn; the count starts again when that wait ends. It holds as a permission request does, before the watchdog's cancel only: after the cancel the turn should stop. **Amended (Task 4 review, R12):** only a wait moa itself made counts: `Parked.by` records the waiter (`ask`'s and `reply`'s caller), and anyone else's `reply` on moa's ask holds nothing. | Call 1. |
| A11 | **Labels:** a session's label is its title, else its handle; moa's is `moa`. A label is one line (`text::clean`), without `]`, and at most 40 characters. **`peers`** (`Op::Peers`, no write): moa first, then every live session except the caller. It has one row per peer, with columns `handle label state folder last-asked`. `state` is `spike_state` (moa's is `ready`, `busy` or `off`); `folder` is the cwd through `with_tilde`; `last-asked` is `asked <span> ago` for the newest ask to that peer, else `-`. JSON is an array of `{id, handle, label, state, folder, last_asked, agent_kind, project, cwd, doing}`. **`asks`** (`Op::Asks`, no write): the caller's asks, from or to it (every ask for `Unknown`). They're listed newest first, at most 20, each as `#N  from → to  status  <span> ago  <text, one line, 60 chars>`. JSON is the `Ask` rows. | Spec 6.12's peers columns. |
| A12 | **The peers note and skill:** `peers_note` returns A13's note for every session moa launched. The skill text is `crates/moa-agents/src/claude/skills/moa-peers.md`, written by `session_plugin` as `skills/moa-peers/SKILL.md`. | Spec 6.12: "Every moa-launched session's SessionStart context tells it about peers". |
| A13 | **Texts.** See below. | The spike's wording; "the user", never a name. |

**A13, the texts:**

```
framed (typed into the pane):
  [moa · message from Kite · ask #12] Is the domain renewed?

  (Your reply this turn goes back to Kite as the answer.)
to moa (hidden prompt):
  (moa · Kite asks you, ask #12, not typed by the user)
  Is the domain renewed?

  Your reply this turn goes back to Kite as the answer.
replies to the asker:
  answered        <the full answer>                       (Ok; data {id, from, to, status})
  no reply yet    no reply yet from Kite; `moactl reply 12` waits again   (Ok)
  dropped         Kite stayed busy for 9m; ask #12 was dropped             (error)
  failed          ask #12 failed: <reason>                                 (error)
reasons (in `reply` for failed/dropped):
  stayed busy · no receipt (a dialog or copy mode had the pane) · stale · Kite ended ·
  Kite isn't running · Kite's turn failed · Kite moved on before answering ·
  moa's turn ended early: <stop> · moa's turn failed: <error> · moa restarted before answering
refusals:
  you can't ask yourself
  ask works from a session or moa; type into the session yourself
  Kite isn't running; `moactl resume 3f2a91c4` starts it, then ask again   (its handle)
  moa isn't running                       (moa's agent can't take a turn, A9 (f); amended)
  Kite is waiting on your answer to ask #9; answer that first
  an ask is at most 8000 characters; point to a file instead
  an ask can't be empty
  a timeout is 1 to 3600 seconds
card (console):  head `an ask for moa`, text `Kite · #12: Is the domain renewed?`
line under the automatic row: `· moa answers Kite's ask #12`
peers note (SessionStart additionalContext):
  moa: you can message the other agents in this cockpit, and moa itself.
  - `moactl peers` lists them. `moactl ask NAME "TEXT"` asks one and waits for the reply,
    up to 9 minutes: run it with a Bash timeout of 600000 ms.
  - "no reply yet" means it's still open: `moactl reply ID` waits again. `moactl asks`
    lists recent asks.
  - A message starting `[moa · message from NAME · ask #N]` is an ask to you: your reply
    that turn goes back to NAME as the answer.
```

- **The `<span>` texts** use `time::span`.
- **The skill** repeats the note and adds when to ask (only for what only that agent
  knows), to keep a question short and self-contained, and to answer an ask in one reply.

## Global Constraints

The roadmap's constraints apply, as do every earlier slice plan's. Also:

- **Reuse before writing.** Search the workspace before adding a helper. A near-copy is an
  **Important** finding, and each report lists what it reused. In particular:
  - one name resolver (`candidates`);
  - one parked-reply pattern (`Effect::Reply` from memory, dropped on `ClientGone`);
  - one gate check for every automatic source (`channel::may_send`);
  - one card writer (`central::card`).
- **Building and verifying:**
  - cargo only through `nix develop --command cargo …`, with `cargo nextest run`;
  - `scripts/verify quick` before every commit, and the full `scripts/verify` once per PR;
  - never run cargo while an e2e test runs;
  - test counts are quoted from `target/verify/nextest.log`, never from memory.
- **The core stays pure:** time is `Now`. One new timer (`TimerKey::Asks`), armed only while
  something waits (spec U5: no polling).
- **Zero launches:** an ask, its delivery and its answer start no process (`Spawner`).
  Typing goes through the link's control client.
- **Rows are the truth:** every status change is in the commit that causes it. A refused
  commit types nothing and replies nothing new.
- **Nothing reaches a pane unsafely:** only `set-buffer`, `paste-buffer -p -d` and
  `send-keys … Enter` are added to `ALLOWED`, each with its own `check` arm. The text goes
  through `cmd::quote`.
- **Tests:**
  - only under `/tmp` roots;
  - never the real `claude`, the real adapter, a tmux server named `moa`/`moa-work`, or a
    real brain;
  - every wait goes through `wait_for` with a 10 s limit;
  - daemon tests move time with `FakeTime::advance`, never a sleep.
- **Public repo:** fixtures use Lantern, Kite, Harbor, Beacon, `acct-a`, `acct-b` and
  `~/private`. Texts say "the user", never a name.
- **Log lines never hold an ask's text or reply,** only ids and statuses.
- **Plain words** in everything the user and agents see: "ask", "answer", "peer", "moa".
  Never "ACP", "adapter" or "parked".

## Review Focus

1. **The pasted text breaks out:**
   - **The inputs:** an ask holding `ESC[201~`, `\r`, a NUL, `;`, `#{pane_id}`, `"`, `$(…)`,
     a trailing backslash, or 8000 characters of emoji.
   - **Expected:** the pane gets exactly the cleaned text inside one bracketed paste. tmux
     runs none of it as a command or format.
   - **Pinned by:** `typed_text_round_trips` (Task 1, real tmux) and
     `ask_text_is_cleaned` (Task 2).
2. **Two asks race for one peer, or ask each other:**
   - **The inputs:**
     - A and B ask C in one input;
     - A asks B while B's ask to A is pending;
     - C stops twice before the receipt;
     - the asker's connection drops mid-ask;
     - the daemon restarts between the claim and the Enter.
   - **Expected:** one ask at a time per peer, the oldest first. The deadlock is refused.
     Each request is replied to at most once. A lost Enter fails at 20 s.
   - **Pinned by:** `asks_interleave_safely` (proptest, Task 3).
3. **A receipt that isn't one:**
   - **The inputs:**
     - a prompt quoting another ask's marker;
     - a marker for an ask to a different session;
     - the user's own prompt arriving while the ask is `sent`;
     - a wrapped paste where the marker isn't first.
   - **Expected:** only the marker naming an ask to this session counts. A prompt without it
     changes nothing.
   - **Pinned by:** `only_its_own_marker_is_a_receipt` (Task 3).
4. **moa asks while it's watched:**
   - **The inputs:** moa asks Kite with a 540 s wait. Kite answers at 520 s, or never.
   - **Expected:** no watchdog cancel while the ask is open. The watchdog counts again from
     the ask's end.
   - **Pinned by:** `the_watchdog_waits_for_moas_ask` (Task 4).
5. **The answer is huge, or empty:**
   - **The inputs:** a final message of 200 KB; an empty one with a transcript; an empty one
     without.
   - **Expected:** the asker gets all 200 KB. The history row stays cut as today. An empty
     message gives `(no text)`.
   - **Pinned by:** `the_answer_is_the_full_message` (Task 3).

---

## Task 1: typing into a pane, and full hook texts

**Files:** `crates/moa-core/src/tmux/cmd.rs`; `crates/moa-agents/src/claude/normalize.rs`;
tests in `crates/moa-core/src/tmux/cmd.rs` (unit), `crates/moa-tmux/tests/` (real tmux),
and `crates/moa-agents/tests/` (normalize).

**Interfaces:**
- Produces: `Cmd::set_buffer(name: &str, text: &str) -> Cmd`, `Cmd::paste_buffer(name: &str,
  pane: PaneId) -> Cmd` (`paste-buffer -p -d -b NAME -t %N`), `Cmd::enter(pane: PaneId) ->
  Cmd` (`send-keys -t %N Enter`). `check` accepts exactly these shapes: a buffer name
  matching `moa-ask-<digits>`, and `send-keys` with `C-c` or `Enter` only.

- [ ] **Step 1: Failing tests:**
  - `typing_commands_check`: the three builders pass `check`. Refused shapes include
    `send-keys -t %1 ls Enter`, `set-buffer -b other x`, and `paste-buffer` without `-p`.
  - `typed_text_round_trips` (real tmux, sandbox server): `set_buffer` on each Review Focus
    1 input (after A2's cleaning), then `show-buffer -b`, gives the text back byte for byte.
    It includes newlines (spec 13: `\n` escapes on 3.4).
  - `full_texts_reach_the_core`: a Stop payload with a 10 000-character
    `last_assistant_message`, and a UserPromptSubmit with a 10 000-character prompt, normalize
    uncut.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement.** Correct `Cmd::interrupt`'s doc ("nothing else is ever typed")
  to name the ask path.
- [ ] **Step 4: Run them again, then `scripts/verify quick`.** They pass. The tmux 3.4 run
  is the PR's full verify.
- [ ] **Step 5: Commit.** `v1: typing an ask into a pane; hooks carry full texts`.

## Task 2: asks, peers, and waiting for the answer

**Files:**
- **Create:** `crates/moa-core/src/asks.rs`.
- **Modify:**
  - `op.rs` and `ops/mod.rs`;
  - `core.rs` (the parked arms, `ClientGone`, `asks::run` in `handle`);
  - `effect.rs` (`TimerKey::Asks`);
  - `state.rs` (`asks_mem`);
  - `names.rs` (`State::peer`);
  - `moa-proto/src/cli.rs` (`to_op`);
  - the protocol schema snapshot.
- **Tests:** `crates/moa-core/tests/asks.rs`; update the not-yet tests
  (`moa-proto/tests/cli.rs`, `moa/tests/ctl.rs`).

**Interfaces:**
- Produces:
  - `Op::{Peers, Ask {to: String, text: String, timeout_s: i64}, Reply {id: i64, timeout_s:
    i64}, Asks}`, with methods `peers`, `ask`, `reply`, `asks`.
  - The constants `ASK_TEXT_MAX`, `ASK_WAIT_DEFAULT_MS`, `ASK_ENTER_MS`, `ASK_RECEIPT_MS`.
  - `asks::{Parked, Waits, AsksMem, run(state, now, fired, fx), end(state, tx, id, status,
    reply) -> Vec<Effect>}`. `end` is the one place an ask ends and its requests reply.
  - `State::peer`, `State::label_of(AskTarget) -> String`, `State::is_live(SessionId)`,
    `State::is_free(SessionId)`.
- Consumes: Task 1 (nothing yet; delivery is Task 3).

- [ ] **Step 1: Failing tests** (core, `handle_checked`, fixed `Now`):
  - `an_ask_waits_pending`: Kite asks Harbor (busy). The row is `pending`, with
    `from_name` `Kite`. No reply yet.
  - `an_ask_refuses` (one case per A13 refusal): yourself; a shell caller; Harbor ended and
    unbound (`moactl resume <its handle>` in the text); the deadlock guard; 8001 characters; empty
    after cleaning; timeout 0 and 3601.
  - `names_prefer_live_sessions`: two sessions titled Kite, one ended, resolve to the live
    one. Two live ones list both.
  - `ask_text_is_cleaned`: Review Focus 1's control characters are gone from the saved text.
    `\n` and `\t` stay.
  - `a_pending_ask_is_dropped_at_its_wait`: at `until` the row is `dropped`/`stayed busy`,
    and the request gets A13's dropped error.
  - `a_sent_ask_says_no_reply_yet`: with the row set to `sent`, `until` replies `Ok` with
    the no-reply-yet text, and the row stays `sent`. `reply 12` parks again, and `end(…,
    answered, "yes")` replies `yes` to it.
  - `a_gone_client_leaves_the_ask`: `ClientGone`, then nothing is replied. The row is
    unchanged, and the timer is cancelled when nothing else waits.
  - `peers_lists_live_sessions` and `asks_lists_mine`: A11's rows, in snapshots (`insta`, as
    the other text tests).
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** A1–A4 and A11. `asks::run` arms `TimerKey::Asks` at the
  earliest `until` (Task 3 adds its own deadlines).
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: ask, reply, asks and peers`.

## Task 3: delivery to a session, receipt and answer

**Files:**
- **Modify:**
  - `asks.rs`;
  - `hooks.rs` (`applied_changes`: receipt and answer; A8 on `SessionEnded`);
  - `hooks::pane_gone` and `lost_agent` (A8);
  - `central/reports.rs` (`queued`, A7).
- **Tests:** `crates/moa-core/tests/asks.rs` and `asks_interleave.rs` (proptest).

**Interfaces:**
- Consumes: Task 1's builders, and Task 2's `asks_mem`, `end` and `is_free`.
- Produces: `asks::marker_id(prompt: &str) -> Option<AskId>`, `asks::framed(state, &Ask) ->
  String`.

- [ ] **Step 1: Failing tests:**
  - `a_free_peer_gets_the_ask_typed`: Harbor idle, Kite asks. One commit (`sent`,
    `claimed_at`), then `set-buffer -b moa-ask-1 …` and `paste-buffer -p -d -b moa-ask-1 -t
    %5`. At +500 ms, `send-keys -t %5 Enter`. No process is launched.
  - `a_busy_peer_gets_it_on_its_stop`: Harbor working, the ask stays `pending`. Harbor's
    Stop, then it's typed in the same input.
  - `one_ask_at_a_time`: two asks to Harbor. The second waits until the first is answered,
    then goes.
  - `only_its_own_marker_is_a_receipt`: Review Focus 3's prompts.
  - `the_answer_is_the_full_message`: Review Focus 5. The asker's request gets the full
    text, and `answered_at` is set.
  - `no_receipt_fails_at_20_s`, and `a_late_receipt_revives_it` (A6).
  - `stale_asks_fail_before_a_new_claim` (A5).
  - `a_dead_peer_ends_its_asks` (A8): `SessionEnded`, and the pane gone.
  - `an_answering_stop_queues_no_report` (A7).
  - `asks_interleave_safely` (proptest, Review Focus 2). Random sequences over three
    sessions of: ask, Stop, marker prompt, timer, `ClientGone`, refused write, reload from
    rows. Invariants:
    - at most one `sent`/`received` ask per target;
    - every terminal status stays terminal, except A6's revival;
    - every request replied at most once;
    - no two sessions with open asks to each other;
    - every `sent` ask either gets a receipt or fails by `claimed_at + 20 s`.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** A5–A8.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: an ask is typed into a free peer and answered on its stop`.

## Task 4: asks to moa

**Files:**
- **Modify:**
  - `central/channel.rs` (`Source::Ask`, `due`);
  - `central/mod.rs` (`prompt`, `send`, `failed_wake`);
  - `central/turn.rs` (`ended`);
  - `central/watchdog.rs` (`due`, A10);
  - `central/feed.rs`;
  - `asks.rs` (A9(e));
  - moa-view `views/console.rs` (`core_card` head for `ask`, and the row line).
- **Tests:** `crates/moa-core/tests/central_asks.rs`; console snapshots.

**Interfaces:**
- Consumes: Task 2's `end` and `label_of`.
- Produces: `Source::Ask(AskId)`.

- [ ] **Step 1: Failing tests:**
  - `an_ask_to_moa_waits_for_its_turn`: moa busy with your prompt, Kite asks moa, so it's
    `pending`. The turn ends, and the next prompt is the hidden ask alone (no reports block,
    even with reports queued). It's `automatic` with body `ask`, the card row is written, and
    the ask is `received`.
  - `moas_answer_goes_back`: the stub turn says "Renewed in May." with `EndTurn`, so the ask
    is `answered` with that text and Kite's request gets it.
  - `an_early_stop_fails_the_ask`: `Cancelled` gives `moa's turn ended early: cancelled`.
    An error gives `moa's turn failed: …`.
  - `an_orphan_ask_to_moa_fails` (A9(e)).
  - `asks_go_before_the_brief`: an ask and the brief both due, so the ask goes first. The
    brief goes after, with the reports.
  - `the_watchdog_waits_for_moas_ask`: Review Focus 4.
  - `moa_can_ask_a_session`: `Caller::Moa` asks Kite, and `from_name` is `moa`.
  - Console snapshots: the ask card at widths 40 and 100, and `· moa answers Kite's ask
    #12`.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** A9 and A10.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: asks to moa`.

## Task 5: the peers note and skill

**Files:**
- **Modify:**
  - `crates/moa-core/src/peers.rs`;
  - `crates/moa-agents/src/claude/plugin.rs`;
  - `crates/moa-core/src/central/constitution.md` ("Partners": say an ask comes as a hidden
    prompt naming the asker, and the watchdog waits during moa's own ask);
  - the tests pinning the empty note (`moa-core/tests/hooks.rs`, `moa/tests/hooks_e2e.rs`,
    `moa-testkit/tests/fake_claude.rs`).
- **Create:** `crates/moa-agents/src/claude/skills/moa-peers.md`.

- [ ] **Step 1: Failing tests:**
  - `session_start_gets_the_peers_note`: A13's note, exactly, for a moa-launched session.
    `None` for an adopted one, which has no injected plugin.
  - `the_plugin_ships_the_skill`: `session_plugin` writes `skills/moa-peers/SKILL.md` with
    frontmatter `name: moa-peers` and a one-line `description`.
- [ ] **Step 2: Run them.** They fail.
- [ ] **Step 3: Implement** A12, and update the pinned tests.
- [ ] **Step 4: Run them, then `scripts/verify quick`.** They pass.
- [ ] **Step 5: Commit.** `v1: the peers note and the moa-peers skill`.

## Task 6: the scenarios

**Files:** `crates/moa-testkit/src/{lib.rs, bin/fake-claude.rs}` (a step that reads the
pane); `crates/moa-daemon/tests/asks.rs`; `common/rig.rs` helpers.

**Interfaces:**
- Produces: `FakeStep::Answer { reply: String }`. It reads the pane's input until a
  bracketed paste ends and an Enter follows. It fires `UserPromptSubmit` with the pasted
  text (paste markers stripped), then `Stop` with `last_assistant_message = reply`.
  `FakeStep::Hang` already covers a pane that never answers.

- [ ] **Step 1: Failing tests** (real daemon, sandbox tmux, `FakeTime`, `stub-acp` for moa;
  the sessions' panes run `fake-claude`, bound as Task 7 of 6d did):
  - `kite_asks_harbor_and_gets_the_answer`;
  - `an_ask_waits_for_a_busy_peer`;
  - `no_receipt_fails_the_ask` (the pane is in copy mode);
  - `a_pending_ask_is_dropped_and_a_sent_one_waits_again` (then `reply`);
  - `the_deadlock_is_refused`;
  - `a_peer_that_dies_fails_the_ask`;
  - `kite_asks_moa_between_its_turns` (the card and the answer);
  - `moa_asks_kite` (the stub's turn runs `moactl ask`, through `StubStep::Run`);
  - `asks_survive_a_restart` (pending is delivered after it; sent with its Enter lost fails
    at 20 s);
  - `an_ask_launches_nothing` (the daemon's `launches`, before and after).
- [ ] **Step 2: Run them.** They fail where the wiring is missing. Say which in the report.
- [ ] **Step 3: Fix what fails.**
- [ ] **Step 4: Run them again.** They pass.
- [ ] **Step 5: Commit.** `v1: slice 7's scenarios`.

## Task 7: end to end in the cockpit

**Files:** `crates/moa/tests/asks_e2e.rs`.

- [ ] **Step 1: Failing tests** (the real binary in a tmux sandbox: two `fake-claude`
  sessions opened with `moa open`, moa on `stub-acp`, the real clock, short `--timeout`s):
  - `two_sessions_and_moa_exchange_asks` (the roadmap's "done when"):
    - Kite asks Harbor, and `moactl ask` prints Harbor's answer;
    - Harbor asks moa, and the console shows the card, then moa's answer;
    - the daemon's `launches` is unchanged.
  - `peers_and_asks_read_well`: `moactl peers` and `moactl asks` from Kite's pane, with
    A11's columns.
- [ ] **Step 2: Run them.** They fail where the wiring is missing.
- [ ] **Step 3: Fix what fails.** Each test waits on something visible. Make each fail once
  on purpose.
- [ ] **Step 4: Run them again,** 20 times under load. They pass every time.
- [ ] **Step 5: Commit.** `v1: asks end to end in the cockpit`.

## Try it (p3's body)

With the real adapter (your own run), in a sandbox:
1. Open two sessions. In one, ask Claude to run `moactl ask <the other> "what are you
   working on?"`. The other gets the framed message and answers, and the first prints the
   answer.
2. From a session, ask moa something. The console shows `an ask for moa`, and moa's reply
   goes back.
