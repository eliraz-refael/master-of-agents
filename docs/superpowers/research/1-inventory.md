# moa prototype: feature-parity inventory and lessons

Source: `spike/` at main `5c977d9` (PRs #1–#23), `spike/LEARNINGS.md`, commit messages,
code comments, and the prototype's own error logs (`console.err`, `picture.err`, which are
gitignored). File references are `file:function` under `spike/moa-proto/` unless another
folder is named. `master-chat/` = `spike/master-chat/`, `plugin/` = `spike/moa-plugin/moa/`.

Process map of the prototype, for orientation:

```
moa (bash launcher) ──► tmux -L moa[-BRAIN] -f tmux.conf  (its own server)
  window 0 "moa" = the cockpit, a layout of panes tagged @moa_panel:
     console.py  (curses; talks ACP to claude-agent-acp, the moa agent, cwd master-chat/master)
     picture.py --panel tree|plan|status|today|activity|projects   (curses, one process each)
     agent spot  (a live agent pane swapped in, or an "empty slot" sh)
  windows 1..N = agent sessions (claude-*/codex, started with env MOA_TRACK=1 …) and ⌨ terminals
  popups: picker.py (Ctrl-a /), less help.txt; menus: menu.tmux
every Claude session ──hook──► master-chat/hooks/track.sh ──sqlite3──► brain.db
moa agent ──Bash──► moactl (bash→python moactl.py) ──► brain.db + tmux
other agents ──plugin bin/moactl──► moactl (routing by folder) ──► brain.db + tmux
```

---

## 1. Data model (the brain)

One SQLite file per brain (`MOA_DB`), WAL mode. Base schema in `master-chat/schema.sql`;
more tables, columns, an index and triggers are added on **every** `connect()` in
`picture.py:connect` (ALTER TABLE in try/except + `CREATE … IF NOT EXISTS` script). There is
no schema version. All timestamps are **local-time text** (`datetime('now','localtime')`,
`YYYY-MM-DD HH:MM:SS`), dates are `YYYY-MM-DD` text; comparisons are string comparisons.

### projects
| column | meaning |
|---|---|
| id | PK |
| name | UNIQUE, human name ("lantern"); matched case-insensitively by moactl |
| path | project folder; sessions link to the project whose path is the longest prefix of their cwd (also `path-worktrees/…`) |
| area | default 'private'. **Unused** by any code |
| status | idea \| active \| paused \| done |
| summary | free text (seed: last commit subject) |
| last_activity | ISO date; set by seed from git, bumped to today by the hook on every event of a linked session |
| created_at / updated_at | local timestamps |

### tasks (the big picture; moa is the only writer besides the plan panel's d/x keys and suggestion actions)
| column | meaning |
|---|---|
| project_id | FK projects (nullable) |
| title | short actionable title (moa told ≤ 60 chars, no dates in titles) |
| status | idea \| todo \| active \| waiting \| blocked \| done \| dropped. `task add` defaults todo, or blocked when `--blocked-on` given |
| blocked_on | who/what it waits on (free text, "Dana"); setting it non-empty forces status blocked unless `--status` given |
| today | YYYY-MM-DD when it's on that day's list (★ on the plan) |
| session_id | linked session (full ID resolved from prefix/title) |
| note | free text |
| who | you \| agent \| other — whose move (default `you` on add) |
| due | YYYY-MM-DD (input accepts today, tomorrow, +N, weekday names = next one never today, ISO; "" clears) |
| priority | p0 \| p1 \| p2 \| NULL |
| created_at / updated_at | updated_at bumped by `task set` and plan d/x; "✓ N done today" counts `status='done' AND date(updated_at)=today` |

Derived "kind" on the plan (`picture.py:plan.kind`): wait = status waiting/blocked or who=other;
agent = who=agent or status active; else you.

### notes (moa's memory)
`project_id` (nullable), `kind` fact \| preference \| routine \| observation \| thread-summary,
`text`, `created_at`. The brief shows the last 10 fact/preference/routine notes and the last
thread-summary (600 chars).

### suggestions (moa proposes, user clicks)
`text` (a question saying what yes does), `status` open \| accepted \| rejected \| later,
`action` = JSON list of moactl argument strings that "yes" runs (verbs whitelisted:
task, set, archive, close, group, note, project), `created_at`, `resolved_at`. Duplicate
open text is refused ("already suggested #N").

### sessions (registry of agent conversations)
| column | meaning |
|---|---|
| session_id | PK; the agent's conversation ID (Claude session UUID, or codex ID) |
| account | account name: basename of CLAUDE_CONFIG_DIR minus leading dot (hook), or launch command name (adopt), or an absolute config dir for unnamed ones, or `codex` |
| cwd | last reported cwd |
| project_id | linked project (set on first insert by longest path prefix; `project --add` back-links unlinked sessions) |
| title | human title (from MOA_TITLE at launch, or moa/tree rename). First writer wins in the hook (`coalesce(title, excluded.title)`) |
| state | working \| idle \| waiting \| ended (from the hook) plus **archived** (hidden from tree, still resumable) and **elsewhere** (imported from herdr, running there; don't resume here). Schema comment lists only the first four |
| last_prompt | first 500 bytes of the last user prompt |
| first_seen / last_seen | local timestamps |
| pane | tmux pane ID (`%12`) where it runs; **not unique, reused across tmux server restarts** |
| tmux | tmux socket path the pane belongs to (from `$TMUX`) |
| grp | tree group path, slash-separated ("Private/Lantern") — first writer wins in the hook |
| asleep_at | set when moa closed it to free memory; cleared on resume / SessionStart. "Asleep" = asleep_at set AND state='ended' |

### events (append-only log from hooks and UI)
`id`, `ts`, `session_id`, `account`, `cwd`, `kind` start \| prompt \| stop \| waiting \| end \|
**ui** (something the user or moactl did: "accepted suggestion #3", "layout → focus",
"put 2 sessions to sleep…"), `text` (start: source; prompt: 500 bytes; stop: final message
trimmed to first 600 + "[…]" + last 1800 when > 2400 chars; waiting: notification message;
end: reason). Index `events_session(session_id, id)` (added in connect).

### activity (added in connect; fed only by triggers)
`id`, `ts`, `who` brain \| session \| you, `text`. Triggers: project insert / status change,
task insert / status change / added to today, note insert, suggestion insert, session
project link, and every non-start event (ui → who=you with its text; others → who=session,
"folder › prompt", "✓ turn done", "⚠ needs you", "○ ended"). Read by the activity panel and
the full picture's strip. Never pruned.

### groups (added in connect)
`path` PK (slash-separated), `created_at`. Exists so empty groups survive (tree editing).
Groups shown = union of `groups.path` and every `sessions.grp`.

### asks (added in connect; also in schema.sql)
`id`, `ts` (asked; **restamped when typed in**), `from_sid` (asker session ID, 'moa', or NULL
for an untracked agent), `from_name`, `to_sid` (session ID or 'moa'), `text`, `status`
pending → sent → received → answered, or failed / dropped (moa's own skip received), `reply`
(answer, or why it failed), `answered_at`.

### meta (key/value)
| key | value | writer / reader |
|---|---|---|
| `cursor` | max events.id moa has been told about | master hooks `briefing.sh` (SessionStart) and `since.sh` (UserPromptSubmit) |
| `console_thread` | JSON `{sid, date, account}`: today's moa ACP thread and the account dir it lives in | console `thread_meta` |
| `setup_skipped` | JSON list of account names the user said "not now" to | `moactl setup` |

### Files that are part of "state" but not in the DB
- `<MOA_DB>-focus` (`picture.WAKE`): an empty file a tmux hook touches on window/pane switch.
- `~/.config/moa/layouts.json` (`MOA_LAYOUTS` overrides): `{current, layouts:{name: node}}`.
- `~/.config/moa/<brain|default>.env`: `MOA_ACCOUNT`, `MOA_OPEN_ACCOUNT` per brain.
- `~/.config/moa/<brain>.md`: the scope ("who you are") for a named brain; default brain uses
  `master-chat/private-scope.md`. Template `master-chat/scope-template.md` (`{{BRAIN}}`).
- `~/.local/share/moa/<brain>/brain.db` for named brains; default brain in the repo folder.
- tmux pane/window/global user options (see §5) hold UI state: seen-times, per-window state,
  status counts, "here" folder, panel tags.
- `<account>/settings.json.bak-moa`: one-time backup before moa first edits an account.

---

## 2. Features (the parity checklist)

### 2.1 Launch and brains — `moa` (bash), `brain.sh`
- [ ] `moa` opens the default brain's cockpit; `moa NAME` opens brain NAME; `moa NAME --account A`
  also sets the account moa runs on and that new sessions default to (saved to
  `~/.config/moa/NAME.env`; `default.env` for the default). `-h` prints usage. Brain names:
  `[a-z0-9][a-z0-9-]*`, `default` reserved. One brain per invocation.
- [ ] Each brain = own tmux server (`-L moa` / `-L moa-NAME`), own `brain.db`, own scope file
  (`brain.sh:moa_brain` exports `MOA_BRAIN MOA_TMUX MOA_DB MOA_SCOPE`).
- [ ] Unsets inherited `MOA_ACCOUNT MOA_OPEN_ACCOUNT MOA_TRACK MOA_TITLE MOA_GROUP MOA_SELF
  MOA_ACCOUNT_BRAIN MOA_EXCEPT_UNDER MOA_ONLY_UNDER` (so running `moa work` from inside the
  default moa's terminal doesn't leak), then exports everything **before** the tmux server
  starts so panels/keys/sessions/hooks inherit the brain.
- [ ] First run: default brain → `seed.sh` (projects from `~/private/*` git repos: name, path,
  last commit date and subject; plus hardcoded seed notes); named brain → empty schema +
  scope from template, with messages. Warns if no `--account`.
- [ ] Creates session `moa`, window `moa`, sized to the terminal (`tput`), with a placeholder,
  then `moactl layout apply`; if that fails (broken layouts.json) falls back to the console
  alone, tagged and framed.
- [ ] Already running + `--account`: updates the server env (`set-environment -g`) so new
  sessions use it; moa itself moves on its next console start.
- [ ] Inside tmux (`$TMUX` set): refuses to nest; prints "open it from a new terminal tab".
- [ ] Attaches (`exec tmux attach -t moa`).
- Rust note: brains are independent instances; nothing is shared between them, not even peers.

### 2.2 Cockpit and layouts — `layout.py`, `moactl layout|goto|focus`
- [ ] A layout is a tree: `{"split":"h"|"v","children":[…≥2]}` or `{"panel":NAME}`, each with
  optional `"size"`: int cells or `"NN%"`. Validation: known panel names, ≤ 1 console and ≤ 1
  agent (`layout.validate`).
- [ ] Panels: `console`, `tree`, `plan` (big picture), `status`, `today`, `activity`,
  `projects`, `picture` (tabbed all-in-one), `agent` (a live agent pane moved in; empty
  "no agent here yet / press Enter on a session in the tree" slot otherwise).
- [ ] Presets (`layout.DEFAULTS`): cockpit (tree 26% · console · plan 30%), focus (tree over
  plan 45% at 24% · agent over console 30%), classic (console · plan over tree 40% at 42%),
  triage (status 30% · console · plan 28%). Presets added later appear in old configs
  (`layout.load` merges; a deleted preset comes back). Config auto-created.
- [ ] `layout apply [NAME]` builds in a fresh window (`moa-building`), **moves** the live
  console and agent in with swap-pane (they keep running), respawns other panels, resizes,
  breaks leftover agents (and a console with no spot) back into their own windows, kills
  the old window and strays, renames to `moa`, moves to index 0, selects the console, saves
  `current`. Logs a ui event.
- [ ] `layout list` (▸ marks current, leaves listed, config path), `layout show NAME` (JSON),
  `layout set NAME JSON` (validated, saved; moa uses this to edit layouts on request).
- [ ] `goto PANEL` selects that panel; if missing and the current layout has it (or no
  layout), re-applies the layout ("repair"); `agent` also finds the empty slot.
- [ ] `focus Q` = resume (if not running) and show it: swap into the agent spot if the layout
  has one (tag + label; the previous occupant goes back to its window, renamed by title; an
  empty slot is killed), else switch to its window.
- [ ] Panel tagging (`layout.tag`): `@moa_panel`, `@moa_label` (border title: `● title` for
  agents, LABELS for panels), and for framed panels `@moa_framed=1` plus pane-level
  `window-style fg=#4c566a` / `window-active-style fg=#88c0d0` (the frame colour follows
  focus with no events).
- [ ] Repair: Ctrl-a r / menu "Repair panels" = `layout apply` (respawns dead panels).
- [ ] Splitting the cockpit is refused ("the cockpit is a layout").

### 2.3 Panels — `picture.py` (one process per panel, `--panel NAME`; no flag = tabbed "picture")
Common:
- [ ] Rounded frame with title on the top edge (`chrome.frame`), one column padding.
- [ ] Rows: selectable or not; wrapped continuation lines (`Row.more`) scroll/click with their
  item; right-aligned chip (due date).
- [ ] Keys: j/k/↑/↓ move, g/Home, G/End, PgUp/PgDn (by selectable rows), Enter/Space act.
  Tabs (picture only): Tab/→/l, BTab/←/h, 1–6, q quits. Panels ignore tab keys.
- [ ] Mouse: wheel scrolls the view ±3 (the selection is dragged along to stay visible;
  "tracking" off until the next key); left click selects + Enter (single click acts, like
  herdr); clicks on headings ignored; `mouseinterval(0)`.
- [ ] Scroll model (`follow`): keep the selected item whole, including headings above it; the
  first item brings the top of the list, the last item brings the key hints.
- [ ] Footer line: key hints, or a message from the last action, or a y/N confirm.
- [ ] Inline one-line editor in the footer (`ask`): Enter, Esc cancels, Backspace, Ctrl-u,
  Ctrl-w.
- [ ] Publishes the selected row's folder as pane option `@moa_here` (for `moactl term`), one
  tmux call per change.
- [ ] Animation: spinner `⠋⠙⠹…` for working (10 fps), ⚠/! blink for waiting; redraw at
  100 ms while anything animates, else 300 ms.
- [ ] Every panel recomputes `live()` and calls `push_status` (status line feed; see 2.5).
- [ ] Self-reload when `picture.py` changes on disk (execv).

**tree** (Sessions view)
- [ ] Header: `N sessions · N live · N asleep · ⠋ N working · N waiting on you`.
- [ ] Nested groups from `grp` (or project name, or "Loose"), plus empty groups from
  `groups`; guides `├─ └─ │`; group row `▾/▸ name  N · N live ⠋`; collapse toggles.
- [ ] Session row: glyph (status-panel state if live, else state icon; 💤 if asleep), title
  (or last prompt, or ID), account short name (≥ 34 cols), short path (≥ 76 cols), age.
  Colour by state (waiting yellow bold, question orange, done green, live green, else dim).
- [ ] Hides archived.
- [ ] Enter/click: session → `moactl focus` (resume if needed; `elsewhere` → message); group
  → fold. Keys: `n` new session (prompts path prefilled from the group's most recent
  session or the selected one, title, account), `+` new group, `m` move to group, `r`
  rename session or group (group rename renames the subtree), `D` delete empty group,
  `a` archive, `x` close (y/N confirm; kills pane, state ended).
**plan** (big picture)
- [ ] Header `Fri 13 Nov · ✓ N done today`.
- [ ] Per project (sorted: has a session needing you first, then best priority, then has
  "you" tasks, then earliest due, then name): `name ──── N on you · N with agents · N waiting`.
- [ ] Rows: sessions blocked on a permission prompt ("the session needs you"), then tasks
  sorted you → agent → wait, then priority, due, id. Marks: ★ today / ▸ you, ◐ or spinner
  agent ("agent working" when its session is working), ◷ wait (with blocked_on / "someone
  else"). P0/P1 prefix the title; P0 red. Long titles wrap.
- [ ] Due chip on the right: "N d overdue" red pill, "today" yellow pill, "tomorrow"/weekday
  blue (< 7 days), "3 Dec" dim; when too narrow the date goes inline.
- [ ] Footer list: active-this-week projects with no open tasks.
- [ ] Enter/click → focus the task's session ("no session linked" otherwise); `d` done, `x`
  drop (direct SQL + ui event).
**status** (triage)
- [ ] Every live session one line, grouped: needs you · asks you · done · working · idle
  (`SHOW`), group header with count. Line: glyph, title, group leaf (≥ 44 cols), age since
  that state began, and "why" (≥ 72 cols: the prompt, the notification text, the question
  line, or the last line of the reply).
- [ ] Empty: "no live sessions: Ctrl-a / finds one to resume". Enter/click → focus.
**today** (legacy, replaced by plan in presets): today's tasks, sessions waiting/idle in the
last day, working sessions, open/later suggestions with `y`/`n` (y runs actions via moactl).
**projects**: projects with idle days, live count, open tasks; ▸/▾ expand to summary, open
tasks, last 6 sessions; `n`/`N` start a session there on two hardcoded accounts.
**activity**: last 300 activity rows, coloured by who.
**picture** (no `--panel`): the six views as tabs plus a 7-line activity strip at the bottom
whose rows < 4 s old flash with ✦; brand `moa ⠋` top right.

### 2.4 Live state model ("what each session needs from you") — `picture.py:live`
- [ ] Five display states for live sessions (state in working/idle/waiting), most urgent first:
  **waiting** (⚠ needs you: state waiting and the last event is a real Notification),
  **question** (? asks you: last event is stop and its text's last line, minus Markdown,
  ends in "?"), **done** (✓ turn ended after you last saw the pane), **working** (◐),
  **idle** (●).
- [ ] "Why" text per state; lines starting with `<` (system/command messages) blanked.
- [ ] Idle-prompt Notifications ("…waiting for your input") are ignored both at install time
  (matcher) and in the SQL (old rows).
- [ ] "Seen": pane option `@moa_seen` (epoch). A pane first seen gets now as baseline; a pane
  that is the active pane of an attached session's active window gets marked seen whenever
  its last event is newer. Panels re-evaluate when the brain or the `-focus` file changes.
- [ ] Sorted by state then most recent; cached per (db mtime, focus-file mtime).

### 2.5 Status line — `tmux.conf`, `picture.py:push_status`, `counts_text`
- [ ] Two status lines (blank spacer + bar). Left: `◆ moa` pill (blue; orange while the
  Ctrl-a key table or prefix is active; yellow in copy mode). In Ctrl-a mode a grey pill
  lists the keys. Otherwise `^a keys` then the current window as a pill: its state glyph
  (from `@moa_state`) and name ("▦ cockpit" for the moa window), `⤢` when zoomed.
- [ ] Window list: only windows whose agent is ⚠ waiting or ? question, by name (cut at 18);
  all others expand to nothing; the current window's own format is empty (it is on the
  left).
- [ ] Right: `@moa_status` counts per state, coloured (`⚠ 2  ? 2  ✓ 3  ◐ 4  ● 19`), then
  date and time. Bell flag colours a window red.
- [ ] Rule (comment in tmux.conf): no `#()` shell segments, nothing run by status-interval
  (15 s, clock only).
- [ ] `push_status` writes `@moa_status`, `@moa_wake` (the focus-file path), per-window
  `@moa_state` (and unsets for windows no longer live), per-pane `@moa_seen`, in one tmux
  call, only when something changed.

### 2.6 Keys and menu — `tmux.conf`, `menu.tmux`, `moamenu`, `help.txt`
- [ ] `Ctrl-a` (root table, no prefix) enters key table `moa`; the status line shows the keys.
- [ ] In the table: `c t p g` goto console / tree / plan / agent; `/` and `s` picker popup
  (80%×70%); `n` command-prompt "folder:,title:" (prefill `~/private/,`) → `moactl open`;
  `$` terminal here (`moactl term --pane #{pane_id}`); `|` / `-` split the current pane with
  a shell in its folder (labelled `⌨ shell`; refused in the cockpit); `m` back to the
  cockpit; `1 2 3 4` layouts cockpit/focus/classic/triage; `r` repair; `z` zoom; `[ ]`
  prev/next window; `a` literal Ctrl-a; `?` help popup (`less -R help.txt`); arrows move
  between panes and repeat (`-r`, repeat-time 700 ms); `h j k l` move without repeat;
  `Space` or a second `Ctrl-a` open the full menu.
- [ ] Menu (`display-menu`, centred, rounded): Console, Sessions tree, Big picture, Agent
  spot, Find a session…, New session…, Terminal here, Split beside/below, Back to moa, Zoom,
  four layouts, Repair, Previous/Next window, Keys and help, Send Ctrl-a.
- [ ] tmux prefix (Ctrl-b) `Space` opens the same menu (instead of next-layout, which would
  scramble the cockpit); prefix `m` back to moa; prefix `C` repair.
- [ ] `moamenu ARGS`: runs moactl and shows its last output line as a 4 s tmux message.
- [ ] Help text documents all of the above plus tree keys, panel mouse, console commands,
  layouts and the state glyphs.

### 2.7 Session picker — `picker.py` (tmux popup)
- [ ] Query line, `N/M · N live` count; fuzzy filter: every space-separated term is a
  subsequence of (title, group, state word, folder basename, ID prefix); contiguous matches
  rank first; hits highlighted in title and group.
- [ ] Order: live sessions by urgency (status-panel order) → open plain terminals (`⌨`) →
  ended/asleep/elsewhere sessions by recency (≤ 300), archived excluded.
- [ ] Columns: glyph (animated), title, group (≥ 70 cols), state word, age, why.
- [ ] Keys: Enter picks, Esc/Ctrl-c/Ctrl-g close, ↑/↓, Ctrl-p/Ctrl-n, Tab/BTab, PgUp/PgDn,
  Backspace, Ctrl-u, Ctrl-w. Refreshes when the brain changes; animates at 150 ms.
- [ ] Pick a session → `moamenu focus ID` (resume if needed, show in agent spot or switch);
  pick a terminal → select its window (message if it closed meanwhile).

### 2.8 Console — `console.py` (curses; the moa chat)
**Connection and threads**
- [ ] Spawns `claude-agent-acp` (`MOA_ACP` overrides) with `CLAUDE_CONFIG_DIR=<moa's
  account dir>` and `MOA_SELF=moa`, cwd `master-chat/master` (so moa's CLAUDE.md, project
  hooks and permissions load). stderr discarded.
- [ ] One thread per day per account: `initialize` → if `meta.console_thread` is today's and
  on this account and the agent supports `loadSession`, `session/load` (replays the thread;
  replayed entries are stamped "earlier"); else `session/new` and send the hidden OPENING
  prompt ("give me your opening brief… propose today's 1–3 as one suggestion").
- [ ] Account: `MOA_ACCOUNT` (name mapped through moactl's table, or a config dir); default
  acct-a.
- [ ] Hidden prompts (OPENING, WAKE "(Automatic wake-up from the console", ASK "(A message for
  you from another agent") are not shown and are recognised **by prefix** during replay to
  hide them; wake turns show "· on its own" under the `◆ moa` header.
- [ ] Agent process exit → error line "reopen the console from the menu"; fails moa's open ask.
- [ ] Esc while moa is busy → `session/cancel`.
**Rendering**
- [ ] Feed of entries (max 500): you, agent (streamed chunks appended), tool, sys, err, ctl
  card, setup card, help card, event, perm card, suggest card, ask card.
- [ ] moa's replies render Markdown: `#`/`##` headings with ▍ bar, `###+` bold, bullets
  (•◦▪ by depth, numbered), task checkboxes ☑/☐, status glyph as bullet, block quotes ▎
  italic dim, fenced code ▏, horizontal rules, tables (box-drawn if they fit, else one
  bullet per row), `**bold**`, `` `code` ``, links (text underlined, URL dropped), status
  glyphs coloured (●⚠▸◐✓✗○◌). Word wrap with hanging indents; buttons never split.
- [ ] Each moa turn grouped under one `◆ moa  HH:MM` header with a ▏ gutter; tool calls one dim
  line each (`⚙ title: command` with ✓/✗/spinner; full moactl path and `cd … &&` shortened,
  `~` for home, multi-line commands joined); runs of > 3 tool calls collapse to
  "⚙ N earlier steps  N ✓" + last two.
- [ ] "moa is thinking… Esc cancels" while busy. Frame title `◆ moa · BRAIN`; frame right:
  `thread XXXXXXXX`, `◷ N for moa` (queued reports), state (connecting / ⠋ thinking / ⚠
  answer y/a/n / ● ready).
- [ ] Scroll: PgUp/PgDn ±10, wheel ±3; "↑ N more · PgDn" rule when scrolled. Rows cached per
  entry keyed on width/text length/status/answered/n/ts/auto/name generation.
**Input and commands**
- [ ] One input line with cursor: ←/→ (Ctrl-b/f), Home/End (Ctrl-a/e — reachable only via
  Ctrl-a a), Backspace, Delete, Ctrl-u (kill to start), Ctrl-w, ↑/↓ history (when the
  palette is closed). Text → `session/prompt`. Lines starting with `/` are console commands
  (no LLM), parsed with shlex (falls back to split on unbalanced quotes).
- [ ] Commands (`COMMANDS`): `/attention`, `/brief`, `/ls` (card, one row per session with
  ↩ resume button and untracked panes), `/open PATH [title…]`, `/resume Q`, `/focus Q`,
  `/term [PATH]`, `/layout [NAME]` (no arg = list), `/close Q`, `/archive Q`, `/new` (fresh
  thread), `/auto [on|off]` (wake-ups), `/sleep [on|off|HOURS]`, `/suggestions` (cards for
  open/later), `/setup` (card incl. skipped) and `/setup ARGS` (passes through to moactl),
  `/help` (card with buttons), `/quit`. Hidden: `/suggestion yes|no|later N`. Unknown →
  message. A command needing an argument with none → fills `/cmd ` into the input.
- [ ] Command palette: typing `/` lists matching commands with args and descriptions; after
  `/resume|focus|close|archive ` it lists sessions (state glyph, title, group, age) filtered
  by the text; after `/layout ` the layouts. ↑↓ pick, Tab completes, Enter runs (completes
  first if it needs an argument), click works; floats over the feed.
- [ ] Button bar under the input: attention, brief, ls, open…, resume…, layout, new, help
  (`!fill` buttons put text in the input).
**Buttons**
- [ ] Any backticked `/command …` in moa's text, or `` `moactl resume|focus|open|close|archive
  X` `` (without `--` flags), becomes a button. moactl output `→ moactl resume ID` becomes a
  button too.
- [ ] Session buttons show the session's name, not its ID (`↩ Lantern · Rust scaffold`;
  group leaf prefixed when the title doesn't contain it; ≤ 40 chars); ambiguous prefixes
  keep the ID. Names refresh when the brain changes.
- [ ] Tab/BTab hop between buttons when the input is empty (newest feed row first, left to
  right, then the bar); focus follows the same button when the feed moves; Enter presses;
  Esc unfocuses; click presses.
**Cards**
- [ ] ctl card: `╭─ title ──╮`-style box with command output rendered as Markdown.
- [ ] setup card: shown at start when some account isn't tracked and wasn't skipped; per
  account `track` / `private only` / `not now` buttons (→ `/setup install|only|skip NAME`;
  re-offers after); "every account is tracked ✓"; unreadable settings shown in red.
- [ ] permission card: "⚠ permission — moa wants to: TITLE" with `y once` / `a always` / `n
  no`; keys y/a/n answer the oldest pending request when the input is empty; shows "→
  option name" after.
- [ ] suggestion card "✦ moa suggests": Markdown text, "yes runs: moactl …" (or "yes tells
  moa…"), buttons ✓ yes (✓ ok without actions) / ◷ later / ✗ no → shows "→ ✓ output" etc.
  Cards answered elsewhere (panel, moactl) update.
- [ ] ask card "✉ NAME asks moa": another agent's message to moa.
**Session reports, folding**
- [ ] Watches `events` past a cursor. start/end/stop/waiting become event lines: `HH:MM ▶ X
  started`, `○ X ended` (hidden for a session put to sleep), `⚠ X needs you: text ↩ answer`,
  `● X finished a turn ↩ open`. Consecutive stop lines from one session fold into one line
  that moves to the bottom: "X · N turns since 21:10". Text of finished turns is shown only
  when `/auto off` (nobody briefs). The cursor advances only after a row is shown.
- [ ] Reports from moa's own folder are ignored.
**Wake-ups (moa keeps the big picture)**
- [ ] stop/waiting reports with text queue per (session, kind), keeping up to 6 turns per
  session (earlier ones shortened to 500 chars in the prompt, the last ≤ 2600).
- [ ] `tick`: when `auto`, connected, not busy, no pending permission, not replaying: wait
  until reports settle (45 s since the last one, `MOA_WAKE_SETTLE`) unless the oldest
  waited ≥ GAP; at most one wake per 180 s (`MOA_WAKE_GAP`); ≤ 8 sessions per wake (rest
  next time). The hidden prompt lists each report with project/session/time and appends
  "Due by tomorrow" tasks not yet nagged today (in-memory set of (task, due, day)), then the
  instructions (update tasks, brief per session, ▸ lines, ≤ 2 suggestions).
  Shows "moa is reading N session reports…".
**Asks to moa**
- [ ] `take_ask`: between turns, the oldest pending ask to 'moa' → status sent, ask card,
  hidden prompt with ASK prefix; at turn end the turn's agent text becomes the reply
  (`answered`), with "(moa's turn ended early: reason)" when not end_turn. A console start
  fails any 'sent' ask to moa ("moa's console restarted before moa answered").
**Sleep / doze**
- [ ] Every 600 s (`MOA_SLEEP_EVERY`), sessions idle > 6 h (`MOA_SLEEP_AFTER`, 0 = never;
  `/sleep` changes it live) go to sleep via `moactl.drowsy` + `put_to_sleep`; one dim
  "💤 put N idle sessions to sleep: names · resume from the tree" line and a ui event.
**Robustness**
- [ ] Every loop stage in try/except: traceback to stderr (`console.err`), an error line in
  the feed at most once per 30 s per message.
- [ ] Self-reload on `console.py` change, only when moa isn't mid-turn (terminates the agent,
  execv; the thread reloads by ID).

### 2.9 Suggestions (moa proposes, user clicks) — `moactl.py:suggest/suggestion`
- [ ] `moactl suggest "TEXT" [--do "MOACTL-ARGS"]…`: actions parsed with shlex and verb
  whitelisted at creation; duplicate open text refused; prints the new ID.
- [ ] `moactl suggestion yes|no|later ID` (and `ls`: open + later with their actions): yes runs
  each action as a separate `moactl` process, collecting failures; sets status/resolved_at;
  ui event "accepted suggestion #N: text (failed: …)". Already-resolved → error.
- [ ] Without actions, "yes" is a signal for moa (it reads the ui event and acts).
- [ ] moa's rules (CLAUDE.md): ≤ 2 per wake-up, never repeat open/rejected, written as a
  question saying what happens and why; morning brief proposes today's 1–3 as one
  suggestion of `task set N --today` actions; overdue → new date/drop/split; stale/too many
  → drop/postpone; new folder → `project NAME --add PATH`.

### 2.10 Nagging and due dates
- [ ] Due dates are data (`--due`), never in titles; moa derives them ("72h" → +3).
- [ ] Wake-ups carry open tasks due by tomorrow, each at most once per day per (task, due)
  per console process (resets on console restart); moa ends its brief with
  `⏰ Due today: …` / `⏰ 2 days overdue: …`, once a day, overdue first.
- [ ] No timer: nagging only happens on wake-ups (i.e. when sessions report).

### 2.11 Sleep — `moactl.py:sleep_cmd/drowsy/put_to_sleep/runs_in_pane`
- [ ] `moactl sleep Q…` / `moactl sleep --idle HOURS [--dry-run]`.
- [ ] Eligible (`drowsy`): live, display state idle (not working/waiting/question/done),
  not a tagged panel or `@moa_term`, not the window on screen (`window_active &&
  session_attached`), not in an open ask (either side), the most recent session reporting
  from that pane, last event and last_seen older than the cutoff, reachable, and the pane
  really runs that conversation (`--resume` ID matches or none, cwd related).
- [ ] Sleeping: in the cockpit's agent spot → respawn the empty slot; else kill the pane.
  Then state='ended', asleep_at=now. Resume clears it (moactl and the hook's SessionStart).
- [ ] Display: 💤 in tree, picker, `/ls`; "N asleep" in tree header; "Asleep" line in brief;
  `ls` keeps asleep rows past the 2-day window.

### 2.12 Partners: peers and asks — `moactl.py:peers/ask/deliver/wait_reply/asks_ls`, hook
- [ ] `moactl peers [--json]`: moa first ("moa itself: knows the big picture"), then every
  live, reachable session except the caller: ID prefix, label, state, folder, "last asked".
  JSON adds agent (claude/codex), project, cwd, doing.
- [ ] `moactl ask TO "TEXT" [--timeout S]` (default 540): TO = `moa` or session prefix/title/
  project (live sessions preferred over ended ones of the same title). Refuses asking
  yourself, an unreachable peer (suggests `moactl resume`), or a peer that is itself waiting
  on your answer (deadlock guard). Inserts the ask, then `wait_reply`.
- [ ] Caller identity (`caller`): `MOA_SELF=moa` → moa; else the tracked session in
  `$TMUX_PANE` on the same tmux socket; else cwd == master folder → moa; else (None, folder
  name).
- [ ] Delivery (`deliver`, run by the waiting asker, 1 s poll): when the peer is idle, fail
  stale sent/received asks to it older than 2×ASK_WAIT, then atomically claim (pending →
  sent, restamp ts) only if no other ask to that peer is in flight; type the framed message
  with `load-buffer` + `paste-buffer -p` (bracketed paste), sleep 0.5 s, `send-keys Enter`.
  Frame: `[moa · message from NAME · ask #N]\nTEXT\n\n(Your final message this turn goes
  back to NAME as the answer. They're waiting on your answer, so answer rather than asking
  them back.)`.
- [ ] Hook side: a UserPromptSubmit whose full prompt contains `[moa · message from` → parse
  `ask #N` (fallback: oldest sent ask to this session) → received. Stop → the oldest
  received ask to this session gets `answered` with the **full** final message.
- [ ] Failure modes: sent but not received within 20 s (a dialog or copy mode had the pane) →
  failed; peer stopped running → dropped (pending) or failed; timeout while pending →
  dropped ("stayed busy"); timeout while sent/received → prints "no reply yet… `moactl reply
  ID` waits again" and exits 0.
- [ ] Asks to moa: delivered by the console (2.8), not by the asker.
- [ ] `moactl reply ID [--timeout]` waits again; `moactl asks` lists the last 20.
- [ ] Every session moa starts is told about peers via SessionStart output (full plugin
  moactl path, 600000 ms Bash timeout advice, the message marker).
- [ ] The plugin's skill tells every session on an installed account (2.15).

### 2.13 moactl — every verb and flag (`moactl.py:main`)
Session names everywhere: unique ID prefix, else case-insensitive substring of title /
project / group (non-archived, exact title wins); ambiguous → lists up to 8 and exits 1.
| verb | flags / args | behaviour |
|---|---|---|
| `ls` | `--all`, `--json` | sessions live/idle/waiting/elsewhere, or seen in 2 days, or asleep (all with --all). `alive` = asleep \| live \| state. Then UNTRACKED agent panes on this server in private folders with the adopt command. JSON: id, alive, state, account, group, title, cwd, last; `{untracked, account, cwd}` |
| `open PATH` | `--account` (default `MOA_OPEN_ACCOUNT` or acct-a), `--title`, `--group`, `--prompt`, `--bg` | creates the folder if missing; new tmux window (name = title or basename, 24 chars) running `env MOA_TRACK=1 MOA_TITLE=… MOA_GROUP=… <account cmd> [prompt]`; ui event; focuses unless `--bg` |
| `resume Q` | `--bg` | refuses if it runs on another moa's server, or `elsewhere`, or its folder is gone; if alive in an untagged/agent pane, just focus; else new window `env MOA_TRACK=1 <account> --resume ID` (codex: `codex resume ID`), store pane, clear asleep_at, ui event, focus |
| `focus Q` | | = resume without `--bg` |
| `close Q` | | kill pane if alive; state ended, asleep_at NULL; ui event |
| `archive Q…` | | state archived; ui event |
| `sleep [Q…]` | `--idle HOURS`, `--dry-run` | see 2.11 |
| `term [PATH]` | `--title`, `--pane` | plain shell window `⌨ name` in PATH (relative to "here"); "here" = focused pane's folder if agent/untagged, else the panel's `@moa_here`, else the agent spot's folder, else ~; pane marked `@moa_term`, `@moa_label` |
| `set Q` | `--title`, `--group` | rename / regroup (creates the group row) |
| `group add PATH` / `rename OLD NEW` / `rm PATH` | | rename rewrites the subtree in sessions and groups; rm refuses while non-archived sessions remain |
| `adopt PANE` | `--title`, `--group`, `--id` | identify the agent in the pane (start command, or the shell's child process argv + env); must be in a private folder; conversation ID from `--resume`, `--id`, or the newest transcript in `<config>/projects/<cwd with non-alnum→->/*.jsonl` not held by another live pane; refuses a session tracked in another live pane; frees stale rows holding this pane ID; upserts the row (state idle) |
| `goto PANEL` | | 2.2 |
| `layout list\|show\|apply\|set` | args | 2.2 |
| `brief` | | compact brief (~2 KB): today's tasks, needs attention (8), active this week, live sessions (12), asleep, latest notes, last thread summary |
| `attention` | | ranked: waiting sessions (with time), idle sessions in the last day ("your move"), today's open tasks, blocked tasks, stale projects with open tasks (> 7 d), open suggestions; each with a `→ moactl resume ID` |
| `find Q` | | up to 15 matching sessions with state, age, cwd |
| `project NAME` | `--add PATH`, `--summary` | show (status, path, last activity, summary, open tasks, last 10 sessions with last prompt, last 10 notes); with `--add`: new project for a private folder (refuses duplicates), links unlinked sessions under it |
| `note TEXT` | `--project`, `--kind` (default fact) | insert note |
| `suggest TEXT` | `--do ARGS` (repeatable) | 2.9 |
| `suggestion yes\|no\|later\|ls [ID]` | | 2.9 |
| `task ls [PROJECT]` | `--all` | open tasks grouped by project with `#id [status] title (P0, today, who=, due=, on=, session=)` |
| `task add PROJECT TITLE…` | `--today --status --blocked-on --who --due --session --note --priority` | project must exist (exact name, case-insensitive) |
| `task set ID` | `--status --title --who --due --priority ("" clears) --session --note --blocked-on ("" clears) --today --not-today` | per-field update; empty title refused |
| `peers` | `--json` | 2.12 |
| `ask TO TEXT` | `--timeout` | 2.12 |
| `reply ID` | `--timeout` | 2.12 |
| `asks` | | last 20 |
| `setup [list\|install\|only\|remove\|skip] [NAME] [FOLDERS…]` | `--json`, `--brain B`, `--except F…` | 2.14 |
| `import-herdr` | | import herdr-sessions JSON (skips Work/* groups and rows without a conversation ID); live ones become `elsewhere`; ISO times converted to local |
Errors exit non-zero with a one-line human message (used by moamenu/the panels as the
footer text).

### 2.14 Setup, accounts and routing — `setup_hooks.py`, `moactl setup`, `track.sh`, plugin moactl
- [ ] Account discovery: every `~/.claude` and `~/.claude-*` dir with `projects/` or
  `settings.json`; named via the command→folder map, else folder name; state installed /
  not installed / unreadable, limits (only / brain / except parsed back out of the hook
  command), last used (projects/ mtime), plugin enabled.
- [ ] `moactl setup` (tty, no op): interactive, per untracked account `y` install / `f` only
  under the private folders / `n` not now (remembered in `meta.setup_skipped`); lists
  installed accounts missing the plugin with the exact reinstall command.
- [ ] `setup list [--json]`: one line per account with state, limits (`→ work moa, except
  ~/x`, `(only ~/private)`), plugin status, `[skipped]`.
- [ ] `setup install NAME [--brain B [--except F…]]`, `setup only NAME [F…]` (default the
  private roots), `setup remove NAME`, `setup skip NAME`. Validation of flag combinations.
- [ ] Install writes hooks for SessionStart, UserPromptSubmit, Stop, SessionEnd, Notification
  (matcher `permission_prompt|elicitation_dialog`): one command `[ENV=…] bash track.sh #
  moa-spike`, timeout 5; the env prefix carries `MOA_ONLY_UNDER` / `MOA_EXCEPT_UNDER`
  (paths written as `$HOME/…`, double-quote-escaped) and `MOA_ACCOUNT_BRAIN`; the same
  values go in settings `env` for the plugin's moactl. Reinstall strips previous moa hooks
  first (no duplicates); other tools' hooks untouched; old hand-installed track.sh hooks
  recognised.
- [ ] Settings writes: backup `settings.json.bak-moa` once, temp file + rename, keeps file
  mode, writes through a symlink to its real path, `ensure_ascii=False`, trailing newline.
- [ ] Plugin install/remove via `claude plugin marketplace add <dir>` + `claude plugin install
  moa@moa-local` (and `uninstall` + `marketplace remove`) with `CLAUDE_CONFIG_DIR`, 60 s
  timeout, stdin closed; errors returned as "but: …". Removal also deletes the empty
  `enabledPlugins` / `extraKnownMarketplaces` keys Claude leaves behind.
- [ ] Routing (hook and plugin moactl, same `brain.sh:moa_under`, whole path components,
  empty entries ignored):
  - `MOA_ONLY_UNDER`: report only sessions under those folders (already-tracked sessions
    stay); plugin moactl refuses elsewhere ("moa isn't available here…").
  - `MOA_ACCOUNT_BRAIN=B` + `MOA_EXCEPT_UNDER`: outside the exceptions the session goes to
    brain B (unless already tracked in the starting brain); inside, it stays with the moa
    that started it (unless already tracked in B). Plugin moactl switches `MOA_DB`/etc. to
    B, and refuses if B has no brain yet.
  - A tracked session never moves brains when its cwd moves.
- [ ] Tracking is opt-in per launch: the hook exits unless `MOA_TRACK=1` (only sessions moa
  started are tracked). Untracked agents can be adopted (2.13).

### 2.15 The plugin — `spike/moa-plugin/`
- [ ] Local directory marketplace `moa-local` (read in place: edits apply at next session
  start) with one plugin `moa`.
- [ ] Skill `moa-peers`: description triggers on "ask/tell/consult/hand to/coordinate with
  another session, agent or moa; second opinion; a peer knows something"; body documents
  `moactl peers [--json]`, `moactl ask` (600000 ms Bash timeout, `moactl reply ID`,
  `moactl asks`), how to name peers, how to write a message, the incoming marker, don't ask
  back, and "if moactl says moa isn't available here, tell the user; don't work around it".
  `allowed-tools: Bash(moactl peers) Bash(moactl peers *) Bash(moactl ask *) Bash(moactl
  reply *) Bash(moactl asks)` (no approval prompts).
- [ ] `bin/moactl` on the agent's PATH: routing (2.14), then execs the repo moactl.

### 2.16 moa's instructions and context — `master-chat/master/CLAUDE.md`, master hooks, settings
- [ ] Identity ("moa", never "master"), chief of staff; brain-scoped ("Who you are" from the
  briefing); memory is the brain, write things down immediately; inferred structure is a
  suggestion; plain facts need no confirmation; one-line acknowledgement after writing.
- [ ] Call moactl by **full path**, never via a variable/alias.
- [ ] Tree upkeep (titles, groups, archive proposals), "what needs attention" ranking,
  layout editing, open/resume without confirmation when asked.
- [ ] Wake-up protocol (task ls → close/add/update → brief, don't relay; Q&A turns are focus
  signals, not tasks), due dates, nagging, partners (answer asks; ask peers with short
  timeouts, never on wake-ups; propose pairings), proactivity (priorities, suggestions).
- [ ] Wrap-up → `thread-summary` note.
- [ ] SessionStart hook `briefing.sh`: prints "Who you are (brain, db path)" + scope file +
  `moactl brief`; sets `meta.cursor`.
- [ ] UserPromptSubmit hook `since.sh`: prints events since `meta.cursor` (ts | session |
  project | kind | 200 chars), advances the cursor. (Runs on every moa prompt, including
  wake-ups, so moa sees reports twice.)
- [ ] `master/.claude/settings.json`: those two hooks and an allow-list (sqlite3, the moactl
  path, `git -C`, `ls`, Read of the private tree and two accounts' transcripts).

### 2.17 Hot reload and "tell me when to restart"
- [ ] Panels and picker reload by execv when their own file's mtime changes; the console too,
  but only when moa isn't mid-turn. tmux.conf changes need a manual `source-file` (PR
  descriptions spell this out).
- [ ] User expectation: always say explicitly whether a restart is needed.

### 2.18 Other behaviours worth keeping
- [ ] herdr import and the `elsewhere` state (sessions running in another tool are listed
  but not resumed here).
- [ ] Untracked-pane detection in `ls` (agent typed into a plain shell, claude under
  node/bun, codex).
- [ ] `ui` events: everything the user does through moactl/panels is logged so moa learns of
  it (accepted suggestions, closes, layout changes, sleeps).
- [ ] Seeding from git state on first run ("it knows all my projects" was a highlight).

---

## 3. Timers, polls and process launches

The product should replace these with events (a daemon owning the DB and the processes, a
pub/sub of changes, tmux control mode or its own terminal multiplexing). Costs matter: the
prototype's tmux.conf notes every process launch costs 5–10 ms on this machine because of
endpoint security, and a Python start costs much more.

### Loops and polls
| where | what | frequency |
|---|---|---|
| `console.py:main` | `get_wch` timeout → whole loop: drain ACP inbox, `watch_events` (stat DB + WAL), `watch_suggestions` (stat again), `tick`, `doze`, full redraw, stat of `console.py` | **10 Hz forever** |
| `console.py:watch_events/watch_suggestions` | change detection by DB/WAL **mtime**, then SQL past a cursor | each loop |
| `console.py:tick` | wake-up scheduler: SETTLE 45 s, GAP 180 s, batch 8 | evaluated each loop |
| `console.py:doze` | sleep check (`drowsy`: 2× `list-panes`, per candidate `display-message`, maybe `pgrep` + 2× `ps`) | every 600 s |
| `picture.py:main` (each panel process: typically 2–3 at once) | stat `picture.py`, DB, WAL, focus file; rebuild every frame **while anything animates** (full SQL per view) | **10 Hz while any session works**, else 3.3 Hz |
| `picture.py:live` | `tmux list-panes -a` on every DB/focus change, per panel process | per change × panels |
| `picture.py:push_status` | `tmux set …` (one process) when counts/states change, per panel process (duplicated) | per change × panels |
| `picture.py:main` | `tmux set -p @moa_here` | per selection-folder change |
| `picker.py:main` | stat DB; animate | 6.7 Hz while open |
| `moactl.py:wait_reply` | SQL + `reachable` (`tmux display-message` per second) + delivery attempt | **1 Hz for up to 540 s per waiting asker** |
| `moactl.py:deliver` | `time.sleep(0.5)` between paste and Enter | per ask |
| `tmux.conf` status-interval | clock redraw only | 15 s |
| tmux `repeat-time` | arrow key repeat window | 700 ms |
| `claude` Notification | idle-prompt ping after ~60 s at the prompt (filtered) | per idle session |

### Process launches per action
| action | launches |
|---|---|
| **every hook event** of every tracked session (`track.sh`) | bash, `cat`, `jq` ×3–5 (fields; Stop adds `jq -Rrs` truncation, maybe `tail`+`jq` transcript fallback), `head`/`sed`/`basename`, sourcing `brain.sh` + subshell, `sqlite3` ×1–2 for routing checks, `sqlite3` main write, `sqlite3` for asks (prompt/stop), `sqlite3` for asleep (start), `cat` heredoc (start). ~8–14 processes, timeout 5 s, busy_timeout 2 s |
| every moa prompt (incl. wake-ups) | `since.sh`: bash + `sqlite3` ×3 |
| moa thread start | `briefing.sh`: bash, cat, `moactl brief` (bash → python), sqlite3 |
| every moactl call (moa's Bash tool, panels, menu, console commands) | bash `moactl` → `python3 moactl.py` (imports picture, layout; `connect()` runs ALTERs + trigger script **every time**) + N tmux processes |
| from another agent | plugin bash `moactl` (+ `lsof` for Claude's cwd when routing) → bash → python |
| `moactl focus/resume` | ~10–15 tmux calls (`display-message` for socket path / alive / panel, `new-window`, `has-session`, `list-windows`, `list-panes`, `swap-pane`, 3× `set` for tag/label/frame, `rename-window`, `select-window`, `select-pane`) |
| `layout apply` | dozens of tmux calls (per panel: split/respawn/3× set/resize; then break/kill/rename/move/select) |
| `ls` / `adopt` untracked detection | `list-panes`, per pane: `display-message`, `pgrep`, `ps` ×2 |
| `suggestion yes` | python moactl, then one more python moactl **per action** |
| console start | ACP agent (Node), `moactl attention` (python), `moactl setup list --json` (python, reads every account's settings) |
| console `/command` | one python moactl per command (`/ls` → `ls --json`) |
| tmux key in the moa table (c t p g 1–4 r $ n) | `run-shell -b` → `moamenu` (bash) → moactl (bash → python) → tmux `display-message` |
| window/pane switch, client attach | `run-shell -b touch <db>-focus` (one process per switch) |
| setup install/remove | `claude plugin marketplace add` + `claude plugin install` (Node, 60 s timeout) |
| launcher | `tput` ×2, sqlite3, seed (git per repo) |
| every agent session | one Node process each (the reason for sleep) |
| moa itself | one long-lived `claude-agent-acp` (Node) per console |

### Event-driven replacements to design for
- Hook → a tiny client that writes to the daemon's socket (or the daemon tails something),
  instead of bash+jq+sqlite3 per event.
- DB change → in-process notifications to all views; no mtime polling, no per-panel duplicate
  work, one status computation feeding every view and the status line.
- Focus/seen → tmux control-mode notifications (`%window-pane-changed`, `%session-window-changed`)
  or own focus tracking; no touch-a-file hook.
- Ask delivery → the daemon knows when a peer goes idle (Stop event) and delivers then; no
  1 Hz asker loop; the asker blocks on a socket.
- Wake-up scheduling and sleep checks → timers in the daemon keyed on events.
- tmux mutations batched in one control-mode connection, not a process per command.

---

## 4. Lessons and mistakes

Each: **symptom** → cause → **rule for the design**.

### Process identity and tmux
1. **Pane IDs are reused after a tmux server restart.** A stale `sessions.pane` pointed at a
   cockpit panel or someone else's agent: `resume` said "already running", sleep could kill
   the wrong pane, adopt thought a pane was tracked. → Pane IDs restart from %0 per server.
   **Rule:** never treat a pane ID as identity. Identity = (multiplexer instance + pane) and
   must be verified against what runs in it (conversation ID, cwd), or better, the daemon
   spawns and owns agent processes and knows their PIDs. Clear pane bindings when a server
   starts. (`moactl.py:resume/untracked_panes/runs_in_pane/drowsy`, `adopt` frees stale rows.)
2. **tmux 3.8 `display-message -p -t %gone` exits 0** and prints nothing, so dead panes looked
   alive. → **Rule:** never rely on exit codes of tmux queries; compare output (`pane_alive`).
   In Rust, prefer control mode / structured replies and a version test matrix.
3. **A session that died without SessionEnd stays "idle" forever**, holding a pane ID. →
   **Rule:** liveness must come from the process/pane, not from the last hook event;
   reconcile on startup and on pane-exit notifications.
4. **Sessions from another brain's server**: an account split between brains can deliver a
   session another moa opened; its pane ID names a different pane here. → store the server
   (socket path) with the pane and compare (`on_this_server`); `TMUX_PANE` from another
   server names someone else's pane (`here_dir`). **Rule:** pane references are scoped to a
   server instance.
5. **Environment inheritance across tmux servers**: panes inherit the *server's* environment,
   not the shell's. A scratch test cockpit reached the real moa account; a `moa work` run
   from inside the default moa would carry its MOA_ vars and an agent's routing vars. →
   **Rule:** construct every child's environment explicitly (allow-list), never rely on
   inheritance; the launcher unsets all MOA_* and sets them before the server starts.
6. **A pane's login shell resets PATH**, so a stub `claude-agent-acp` on PATH wasn't used and
   test consoles reached the real agent. → `MOA_ACP` explicit command. **Rule:** configured
   absolute commands for external tools; tests inject stubs via config, not PATH.
7. **tmux inside tmux, both on Ctrl-a, is a trap.** → launcher refuses to attach inside
   `$TMUX`. **Rule:** detect nesting; offer to switch client instead.
8. **`-f` replaces `~/.tmux.conf`**, losing the user's extended-keys (Shift+Enter in Claude
   Code) and mouse. → `source-file -q ~/.tmux.conf` first. **Rule:** whatever terminal layer
   the product uses must pass extended keys (CSI-u), focus events, mouse and clipboard to
   agents.
9. **`escape-time 0` can leak DA replies on attach**; 10 ms is used. Curses needs
   `ESCDELAY=25`.
10. **prefix Space = next-layout scrambles the cockpit**; Ctrl-Space is taken by macOS. →
    rebind; use Ctrl-a. **Rule:** own every key that can break the layout.
11. **Focus events don't work for moved panes**: tmux only sends focus-out to panes it saw gain
    focus; swap-pane and startup races break it. → frame colour via per-pane
    `window-style`/`window-active-style`. **Rule:** don't build focus UI on terminal focus
    reports.
12. **tmux can't round borders** → borders became gutters (`pane-border-lines spaces`) and
    panels draw their own frames; panes without frames get a title pill in the gutter.
13. **`monitor-activity` is useless with agents** (they spin constantly); use the bell.
14. **Splitting the cockpit or using tmux layouts breaks the layout model** → refused.
15. **Arrow keys repeat (`-r`), hjkl don't**, so typing right after a move isn't eaten.

### Terminal rendering
16. **TERM/terminfo leak**: panels start curses on `tmux-direct` for exact colours, but the
    system terminfo lacks that entry, so child processes (agent shells) would break if they
    inherited it. → set TERM back right after `setupterm`. **Rule:** never change TERM for
    children; the renderer should emit truecolor directly (crossterm/ratatui) and let tmux
    downsample.
17. **Curses mapped hex to the 256-colour cube**, turning Nord's blue-greys neutral and green
    olive ("murk"). → direct colour; tmux needs `terminal-features …:RGB` per terminal
    (Ghostty marked; others need a line). **Rule:** truecolor by default, capability
    detection with a sane fallback.
18. **Emoji widths**: 💤 is one char, two cells; code places text with `len()`, so 💤 stands for
    glyph + space; emoji in card titles misaligned the box. → **Rule:** all layout by
    display width (unicode-width, matching tmux's wcwidth); prefer single-width glyphs
    (✦ ✉) in chrome; test alignment with wide glyphs at many widths.
19. **Writing the bottom-right cell raises in curses** (`_putter` swallows it). Rust TUI
    libraries handle this; note for any raw renderer.
20. **Scroll-to-top bug**: the view only followed the selected row, and the first selectable
    row sits under headings, so the top could never be shown again. → **Rule:** scrolling
    keeps the selected item *and its context* visible; the wheel moves the view and drags
    the selection along.
21. **Mouse**: with tmux `mouse on`, clicks/wheel reach a pane only if it requested mouse
    events; otherwise tmux enters copy mode. `mouseinterval(0)` for instant clicks. Users
    expect herdr-style single-click-acts. "Hold Shift to select text" must be documented.

### Hooks and Claude Code
22. **Hook stdin**: every field comes from JSON on stdin; `Stop.last_assistant_message` can be
    empty, so the hook falls back to the transcript tail (last assistant text block). → The
    product must handle both and not depend on transcript format more than necessary.
23. **Argument length**: a long reply passed on the sqlite3 command line can exceed the
    per-argument limit (128 KB on Linux) → pass via stdin. **Rule:** no data in argv.
24. **What's next is at the end of a reply**: keeping the first 500 chars lost it. → keep start
    600 + end 1800. Also `head -c 500` on prompts cuts by **bytes** (can split UTF-8).
    **Rule:** truncate by characters, keep head and tail.
25. **Notification noise**: Claude sends "Claude is waiting for your input" after ~1 min idle,
    so every finished session became ⚠. → matcher `permission_prompt|elicitation_dialog`
    plus filtering old rows by text. **Rule:** only permission/elicitation means "needs
    you"; idle pings are not state changes.
26. **Permission rules match the command as written**: moa calling `M=moactl; $M …` made every
    call ask for approval. → call by full path; plugin skill uses `allowed-tools`. **Rule:**
    give agents a stable, simple, allow-listable command name; document it in their
    instructions.
27. **Prompt markers can arrive wrapped**: Claude wraps a paste in `<pasted_content>` tags, so
    the ask header is matched anywhere in the full prompt (not the 500-byte excerpt), and
    the header carries the ask ID so a late paste can't claim the next ask's slot.
28. **Settings entries alone don't register a local plugin marketplace**; the CLI does it per
    `CLAUDE_CONFIG_DIR`. Uninstall leaves empty keys behind. A directory marketplace is read
    in place (edits apply on next session start).
29. **`CLAUDE_PROJECT_DIR` isn't in the agent's shell, and `$PWD` follows `cd`** → the plugin
    finds the session's start folder via `lsof` on `CLAUDE_PID`. **Rule:** routing decisions
    need the session's start cwd from a reliable source (the daemon's registry by session
    ID).
30. **The startup note only reaches sessions moa starts** → the plugin (skill + bin) reaches
    every session on an account. Skill description is always in context; body loads on use.
31. **Hooks run for moa's own thread too** (it runs on an account with the hook); gated by
    `MOA_TRACK` and moa's folder filter. **Rule:** moa's own agent must be explicitly
    excluded from tracking.
32. **Edits to users' settings.json**: tag every entry with a marker, never touch others' hooks
    (herdr, aoe), back up once, atomic write, keep symlinks and mode. Unreadable settings are
    reported, not overwritten.
33. **Running sessions only pick up hooks/plugins on restart** → say so.

### Brain, data and isolation
34. **Brain isolation**: work data must never reach the private brain; "one moa that filters"
    was rejected for separate moas. Folder decides, not account (private projects run on the
    work account). Routing must be identical in the hook and the plugin, and a session never
    switches brain after it's tracked. Anything not listed as an exception belongs to the
    routed brain, so a forgotten folder can't leak into the default brain. **Rule:**
    isolation is structural (separate processes/DBs), routing is one shared function, and
    the plugin guard is "a guard against accidents, not a wall".
35. **Naming confusion**: `setup only acct-work ~/private --else work` read as the opposite
    of what it did. → `install ACCOUNT --brain B --except FOLDERS`. **Rule:** say it the way
    people think about it; test CLI wording with the user.
36. **Hardcoded private roots leak across brains**: `PRIVATE_ROOTS` (~/private, ~/.config) is
    used by `adopt`, untracked-pane listing and `project --add` even in the work brain, so a
    work moa can't add projects or adopt panes in work folders. Account maps (`LAUNCH`,
    `CONFIG_DIRS`, picture's `account_cmd`) are hardcoded and duplicated. **Rule:** scope
    (which folders/accounts a brain owns) is per-brain config, used everywhere.
37. **`moa_under` with an empty entry** (`a::b`, trailing `:`) matched everything; prefix match
    must be on whole path components (`~/privateer`). → skip empties, compare `dir + "/"`.
38. **sqlite3 CLI creates a missing file even to read it** → `[ -f ]` checks. **Rule:** open
    read-only/no-create where only reading.
39. **Migrations on every connect**: columns/tables/triggers are added in `picture.connect()`,
    so hooks (raw sqlite3) may hit a brain without them; hook statements are split so a
    missing table/column only skips that part. **Rule:** versioned migrations, run once by
    the owner process; writers don't race schema.
40. **Asleep is a column, not a state**: killing the pane fires SessionEnd → `state='ended'`,
    which raced any state moa wrote. **Rule:** model orthogonal facts separately (lifecycle
    vs. why it ended); make hook writes and UI writes commute.
41. **Groups need their own table** so empty groups exist for tree editing.
42. **Due dates as data**: moa wrote "due around Nov 15" into a title. → column, relative
    inputs, weekday names (moa talks that way).
43. **Localtime text timestamps** everywhere; string comparisons; replayed ACP history has no
    timestamps ("earlier"). **Rule:** store UTC instants, render local.
44. **Duplicated status logic**: the 5-state model, account commands and resume logic exist in
    several files (picture/moactl/picker/console) and each panel process computes `live()`
    and pushes status independently. **Rule:** one model in the daemon, all views
    subscribe.
45. **The activity table and events grow forever**; `since.sh` prints every event since the
    cursor (could be huge after a long gap). **Rule:** retention and caps on what is fed to
    the LLM.

### Asks protocol (from the /code-review high fixes, #16)
46. One ask in flight per peer, **claimed atomically** before typing, so two askers can't both
    type and answers map to the right ask.
47. A paste that never becomes a prompt (a dialog or copy mode had the pane) must fail after
    20 s instead of taking the next ask's answer.
48. A wait that times out must **drop** an undelivered ask (else it's pending forever and
    blocks the peer); sent/received asks older than 2×wait are failed when the peer is idle.
49. A console restart fails moa's half-answered ask.
50. Deadlock guard: you can't ask someone who's waiting on your answer.
51. Agents' shell commands time out at 10 min → wait 9 min, tell agents to use a 600000 ms
    timeout, `reply ID` to re-wait.
52. moa is identified by `MOA_SELF`, not by folder or pane. Asking by name prefers live
    sessions over ended ones with the same title. Peers on another server aren't reachable.
53. Bracketed paste (`paste-buffer -p`) keeps a multi-line message one prompt; Enter after a
    0.5 s delay. **Rule for Rust:** typing into another agent's TUI is inherently racy;
    prefer a structured channel where available (ACP for agents moa runs itself, or the
    agent's own message API), and keep the typed path as a fallback with these guards.

### Console / moa behaviour
54. **The console died silently** (no logging). → every pane logs, survives a bad message, and
    there's an obvious "bring it back" (repair). But the error loop then logged the same
    traceback every frame (50× `NameError` in `console.err`) because a draw error recurs at
    10 Hz. **Rule:** dedupe logs too, and degrade the failing entry instead of the frame.
55. **Hot reload by execv on mtime** caused crashes when files changed in an inconsistent
    order (`ImportError: cannot import name 'ASLEEP' from picture`, `KeyError: 'soon'`,
    `NameError: chrome`): a half-updated set of modules killed the pane. **Rule:** the
    product upgrades as a unit; panes are supervised and restart; say when a restart is
    needed.
56. **A plain Claude session is the wrong master** (passive). The console must speak first,
    know what's running (from the registry), and act on events.
57. **"What's running?" took 9 shell commands** → a live registry owned by the daemon, exposed
    through the brain API; agents never do discovery or raw SQL (trust ladder and isolation
    enforced there).
58. **Briefing size**: 11.6 KB was cut to a 2 KB preview by Claude. → compact brief + on-demand
    verbs (`attention`, `project`, `find`).
59. **moa relayed instead of briefing** ("a replicant of the session he follows"). → the console
    shows only that turns happened (folded), moa writes the one-line brief; Q&A turns are
    focus signals, not tasks.
60. **Each wake-up is an LLM turn** — the cost to watch. Settle/gap/batch numbers (45 s, 180 s,
    8) and "≤ 2 suggestions per wake" exist to bound it. Reports reach moa twice (wake-up
    prompt + `since.sh` context). **Rule:** one channel; budget wake-ups.
61. **Hidden prompts are recognised by text prefix on replay**; fragile (wording changes break
    hiding). **Rule:** keep the console's own record of which turns were automatic, keyed by
    turn/message ID, not by text.
62. **A thread lives in one account's config folder**: when moa's account subscription stopped
    working, moa went down; switching accounts must start a new thread rather than load one
    the other account doesn't have. **Rule:** moa's account is a setting, the thread
    records it, and failover is designed.
63. **Suggestions must say what yes does**: labels like "New project 'x'…" left the user
    guessing. Actions are validated when moa creates them (so failures are moa's, not the
    user's), whitelisted to safe verbs, and yes runs them without an LLM turn.
64. **Session IDs on buttons mean nothing** to the user → names; moa keeps writing IDs (stable,
    unambiguous) and must not repeat the name next to the button.
65. **Nagging state is in memory** (`self.nagged`), so a console restart re-nags. And with no
    timer, nothing nags when no session reports. Known trade-off ("no timer: an active moa
    wakes often enough").
66. **ACP agent stderr is discarded** (`stderr=DEVNULL`) — failures of moa's agent are
    invisible. **Rule:** capture and surface agent stderr.
67. **Permission card keys y/a/n** only act when the input is empty, otherwise they type.

### Scale and UX
68. **At 30 agents a tab per window overflows at ~6.** → the status line names only urgent
    windows and counts the rest; a picker; a status panel. "Done since you looked" beats
    "idle" as a state at scale (workmux: waiting > done > working > stale).
69. **Tree upkeep is the chore**, creating sessions is fun. moa maintains titles/groups;
    archive proposals in batches.
70. **The tree must be visibly nested and the screen alive** (spinners, blink, flash of new
    activity). "Fun" is a requirement.
71. **tmux must be invisible plumbing** for a non-tmux user: one key (Ctrl-a) that shows its
    keys, a menu, a real Ctrl-a via `Ctrl-a a`; no prefix chords to learn.
72. **Conversation IDs surviving restarts is essential**; sleep relies on it (a Node process
    per idle agent vs. a free ID).
73. **Plain terminals belong next to agents** (dev servers) but aren't sessions; the picker
    lists them so they don't get lost.
74. **Codex has Claude-style hooks** (`~/.codex/hooks.json`, each hook trusted once) — same
    tracker could follow it; not installed yet.

### Testing practice that worked
75. Sandboxed tmux servers with a fake HOME, copied brains, a stub ACP agent (`MOA_ACP`), fake
    agent commands, `MOA_LAYOUTS` for a separate layouts file, a pty client rendered with
    pyte at several sizes, SGR mouse events written to the client, `capture-pane` checks.
    The Rust version needs the same harness: headless render tests at many widths, a fake
    agent, an isolated multiplexer.

---

## 5. Implicit contracts with outside tools

### Claude Code hook input (JSON on stdin) — `master-chat/hooks/track.sh`
| field | event(s) | use |
|---|---|---|
| `session_id` | all | session key |
| `cwd` | all | project link, routing |
| `hook_event_name` | all | dispatch |
| `source` | SessionStart | event text (startup/resume/clear/compact) |
| `prompt` | UserPromptSubmit | last_prompt (500 B), ask marker match on the full text |
| `last_assistant_message` | Stop | report text, ask answer |
| `transcript_path` | Stop | fallback: JSONL lines with `type=="assistant"`, `.message.content[]` with `type=="text"`, `.text` |
| `message` | Notification | "needs you" text |
| `reason` | SessionEnd | event text |
Hook stdout on SessionStart becomes model context (used for the peers note and moa's brief);
UserPromptSubmit stdout is also added as context (`since.sh`). Hooks run with the session's
environment: `CLAUDE_CONFIG_DIR` (account), `TMUX`, `TMUX_PANE`, and whatever the launcher
put in (`MOA_TRACK`, `MOA_TITLE`, `MOA_GROUP`, `MOA_DB`) plus settings `env`.
Notification matchers relied on: `permission_prompt`, `elicitation_dialog` (excluded:
`idle_prompt`, the "waiting for your input" ping).
Events **not** used: PreToolUse, PostToolUse, SubagentStop, PreCompact.

### Claude Code settings.json keys written
- `hooks.<Event>[] = {matcher?, hooks:[{type:"command", command:"… # moa-spike", timeout:5}]}`
  for SessionStart, UserPromptSubmit, Stop, SessionEnd, Notification.
- `env.MOA_ONLY_UNDER`, `env.MOA_ACCOUNT_BRAIN`, `env.MOA_EXCEPT_UNDER` (settings env reaches
  the agent's Bash tool, i.e. the plugin's moactl).
- Via `claude plugin …` CLI: `enabledPlugins["moa@moa-local"]`, `extraKnownMarketplaces["moa-local"]`.
- Backup file `settings.json.bak-moa`.
- moa's own project settings (`master-chat/master/.claude/settings.json`): `hooks.SessionStart`,
  `hooks.UserPromptSubmit`, `permissions.allow` (`Bash(sqlite3:*)`, `Bash(<moactl path>:*)`,
  `Bash(git -C:*)`, `Bash(ls:*)`, `Read(<folders>/**)`).
- Agent env vars read: `CLAUDE_CONFIG_DIR`, `CLAUDE_PROJECT_DIR` (absent in Bash), `CLAUDE_PID`.
- Transcript location convention: `<config>/projects/<cwd with every non-alphanumeric char
  replaced by '-'>/<session-id>.jsonl` (adopt).
- CLI flags: `claude --resume ID [prompt]`, account wrapper commands (`acct-a` etc.)
  that set `CLAUDE_CONFIG_DIR`, `CLAUDE_CONFIG_DIR=… claude` for unnamed accounts;
  `codex`, `codex resume ID`; `claude plugin marketplace add|remove`, `claude plugin
  install|uninstall`.
- Pasted text in a prompt arrives wrapped in `<pasted_content>` tags.

### Plugin layout
```
spike/moa-plugin/                      ← directory marketplace "moa-local" (read in place)
  .claude-plugin/marketplace.json      {name, owner:{name}, plugins:[{name:"moa", source:"./moa", description}]}
  moa/.claude-plugin/plugin.json       {name:"moa", version:"0.1.0", description}
  moa/bin/moactl                       on the agent's PATH while the plugin is enabled
  moa/skills/moa-peers/SKILL.md        frontmatter: name, description, allowed-tools
```

### ACP (claude-agent-acp 0.71, JSON-RPC 2.0 over stdio, newline-delimited) — `console.py`
Client → agent:
- `initialize` `{protocolVersion: 1, clientCapabilities: {fs: {readTextFile: false,
  writeTextFile: false}, terminal: false}}`; reads `result.agentCapabilities.loadSession`.
- `session/new` `{cwd, mcpServers: []}` → `result.sessionId`.
- `session/load` `{sessionId, cwd, mcpServers: []}` → replays history as `session/update`s.
- `session/prompt` `{sessionId, prompt: [{type: "text", text}]}` → `result.stopReason`
  (`end_turn` or other; others shown).
- `session/cancel` notification `{sessionId}`.
- Responses to agent requests: `session/request_permission` → `{outcome: {outcome:
  "selected", optionId}}`; anything else → error `-32601`.
Agent → client:
- `session/update` with `update.sessionUpdate` = `agent_message_chunk` / `user_message_chunk`
  (`content.text`), `tool_call` (`toolCallId`, `title`, `status`, `rawInput.command`),
  `tool_call_update` (same fields, partial). Other update kinds (thoughts, plans, available
  commands, mode changes) are ignored.
- `session/request_permission` `{toolCall: {title}, options: [{optionId, name, kind:
  allow_once | allow_always | reject_once …}]}`.
Behavioural contracts: the agent loads the cwd's `CLAUDE.md`, project `.claude/settings.json`
hooks and permissions, honours `CLAUDE_CONFIG_DIR`; threads live per account; replay has no
timestamps; tool calls run with the env the client gave the agent (`MOA_SELF`).

### tmux (3.8) features relied on
- Separate servers `-L NAME`, `-f` config, `%hidden` variables, `source-file -q`, `if -F`.
- User options at every scope: global `@moa_status`, `@moa_wake`; window `@moa_state`; pane
  `@moa_panel`, `@moa_label`, `@moa_framed`, `@moa_here`, `@moa_term`, `@moa_seen`; pane-level
  `window-style` / `window-active-style`.
- Formats: conditionals, comparisons, `#{E:…}`, `#{T:…}`, `#{=/18/…:…}` truncation; fields
  `pane_id`, `pane_current_path`, `pane_start_command` (quoted), `pane_pid`, `window_id`,
  `window_name`, `window_index`, `window_active`, `window_zoomed_flag`, `window_bell_flag`,
  `session_attached`, `socket_path`, `client_key_table`, `client_prefix`, `pane_in_mode`,
  `pane_active`.
- Custom `status-format[]` (2 lines) with a window list whose formats expand to empty to hide
  windows; `status-left` / `status-right` pills (Nerd Font half circles).
- `pane-border-status top`, `pane-border-lines spaces`, `pane-border-format`,
  `pane-border-indicators off`, `menu-border-lines rounded`, `popup-border-lines rounded`.
- Key tables (`bind -n C-a switch-client -T moa`, `bind -T moa …`, `bind -r` repeat,
  `repeat-time`), `display-menu`, `display-popup -E -w -h -T`, `command-prompt -p -I`,
  `run-shell -b`, `display-message -d`.
- `set-hook -g` `session-window-changed`, `window-pane-changed`, `client-attached`.
- Pane plumbing: `new-session -d -x -y`, `new-window -d -P -F -n -c`, `split-window -d -h|-v
  -P -F -c`, `respawn-pane -k`, `swap-pane -d`, `break-pane -d -n`, `kill-pane`,
  `kill-window`, `move-window`, `rename-window`, `resize-pane -x|-y` (cells or %),
  `resize-pane -Z`, `select-window`, `select-pane`, `list-panes -a -F`, `list-windows -F`,
  `has-session`, `set-environment -g`, `load-buffer -b NAME -` + `paste-buffer -p -d -b -t`
  (bracketed paste), `send-keys Enter / C-a`.
- Server options: `extended-keys on`, `focus-events on`, `escape-time 10`, `terminal-features`
  RGB for the outer terminal, `default-terminal tmux-256color`, `mouse on` (from the user's
  config), `base-index 1`, `renumber-windows on`, `history-limit 50000`, `set-titles`,
  `monitor-bell` / `bell-action any`.
- tmux's default mouse bindings forwarding clicks/wheel to panes that requested mouse
  reporting.
- Terminfo `tmux-direct` (ncurses) for 24-bit colour; Ghostty as the outer terminal.

### Environment variables
| var | meaning | set by → read by |
|---|---|---|
| `MOA_BRAIN` | brain name ("" = default) | launcher → console title, briefing |
| `MOA_TMUX` | tmux socket name (`moa`, `moa-NAME`) | launcher → all tmux calls |
| `MOA_DB` | brain.db path | launcher (and plugin routing) → everything, hooks |
| `MOA_SCOPE` | scope file | launcher → briefing.sh |
| `MOA_ACCOUNT` | moa's own account (name or config dir) | `<brain>.env` → console |
| `MOA_OPEN_ACCOUNT` | default account for `moactl open` | `<brain>.env` → moactl |
| `MOA_TRACK` | `1` = report this session | moactl open/resume, picture → hook gate |
| `MOA_TITLE`, `MOA_GROUP` | initial title/group | moactl open → hook; adopt reads them from ps env |
| `MOA_SELF` | `moa` = the caller is moa | console's ACP env → moactl `caller` |
| `MOA_ONLY_UNDER` | colon list: track/allow only under these | account hook command + settings env → hook, plugin moactl |
| `MOA_ACCOUNT_BRAIN` | send this account's sessions to brain B | same |
| `MOA_EXCEPT_UNDER` | colon list: folders that stay with the opening moa | same |
| `MOA_ACP` | agent command override (tests) | env → console |
| `MOA_LAYOUTS` | layouts.json override (tests) | env → layout.py |
| `MOA_WAKE_SETTLE`, `MOA_WAKE_GAP` | wake-up timing (45 / 180 s) | env → console |
| `MOA_SLEEP_AFTER`, `MOA_SLEEP_EVERY` | sleep threshold h (6, 0 = off) / check s (600) | env → console |
| `CLAUDE_CONFIG_DIR` | account | env → hook (account name), console (moa's account), setup (plugin CLI) |
| `TMUX`, `TMUX_PANE` | tmux socket/pane of the session | tmux → hook, moactl caller/here |
| `CLAUDE_PID`, `CLAUDE_PROJECT_DIR` | session process / start dir | Claude → plugin moactl |
| `TERM` | temporarily `tmux-direct` inside panels | chrome.wrapper |
| `ESCDELAY` | 25 ms | panels/console/picker |

### Other tools
- `sqlite3` CLI (hooks, launcher, seed), `jq` (hooks), `git` (seed), `lsof` (plugin
  routing), `pgrep`/`ps` (adopt/untracked), `less` (help), `tput` (launcher), Python 3 with
  curses (Nix), herdr-sessions JSON (`~/.local/state/herdr-sessions/sessions.json`:
  `sessions[].{group, conversation_id, path, profile, title, live, created_at,
  last_active_at}`) for import.
