# 3. Agents: ACP, Claude Code hooks/plugins/sessions, Codex

Research date: 2026-11-15. Local versions checked: Claude Code 2.1.311, codex-cli 0.167.2,
gemini 0.47.0, claude-agent-acp 0.71.0 (Nix; upstream is 0.99.1). Raw doc copies used for
this note are in `design-research/src/` next to this file.

**What changed since the prototype was built (read this first)**

1. **The ACP adapter moved and is far ahead of the Nix build.** It's now
   `@agentclientprotocol/claude-agent-acp` 0.99.1 (repo `agentclientprotocol/claude-agent-acp`).
   The old `@zed-industries/claude-agent-acp` stopped at 0.23.1. Our Nix build is 0.71.
2. **ACP now has a real `systemPrompt` field and `session/resume` (resume without replay).**
   The Rust crate is `agent-client-protocol` 5.3.0, with three major versions in about 6 weeks.
3. **Claude Code hooks can be `type: "http"`.** That's a POST of the same JSON with no
   process spawn. It's localhost TCP only, and when moa is down it shows a visible
   "hook error" notice.
4. **`CLAUDE_CODE_PLUGIN_DIRS` / `--plugin-dir` load a plugin for one session without
   installing it.** moa can inject its plugin (hooks + skill + `bin/moactl`) only into the
   sessions it launches, on any account, with no `claude plugin install` and no env-var
   gating.
5. **`CLAUDE_CODE_SESSION_ID` is set in every Bash, hook and stdio MCP subprocess.** So
   `moactl` knows which session is calling without moa injecting anything.
6. **`PermissionRequest` fires the instant a permission dialog opens.** `Notification:permission_prompt`
   only fires after ~6 s with no typing, and every keystroke resets that timer.
7. **Codex has an app-server.** The TUI can attach to it (`codex --remote unix://…`), and it
   emits `thread/status/changed` with `waitingOnApproval`. That's a hook-free state source.
   Codex hooks need one-time trust per hook hash.
8. **Channels (research preview) let an MCP server push messages into a running Claude
   session** and relay permission prompts. It's the "right" way to message a session
   instead of typing into tmux, but it isn't usable yet without a dev flag and a
   confirmation prompt.

---

## 1. ACP from Rust

### 1.1 Spec status
Sources: https://agentclientprotocol.com/protocol/v1/overview, `/protocol/v1/session-setup`,
`/protocol/v1/prompt-turn`, `/protocol/v2/migration`, `/rfds/updates`.

- JSON-RPC 2.0 over stdio (newline-delimited). There are no batches. Both sides send requests.
  HTTP/WebSocket transport is an RFD (`rfds/streamable-http-websocket-transport`).
- **v1 is stable.** v2 is labeled draft: "gate v2 support behind explicit version
  negotiation and feature flags until it stabilizes" (v2 migration guide, "Scope").
  v2 drops the v1 client `fs`/`terminal` capabilities entirely and adds `state_update`
  (idle/working), `session/status`, background work and replay-complete signals.
- The v1 methods moa needs:
  - `initialize` (protocolVersion, clientCapabilities)
  - `session/new {cwd, mcpServers, additionalDirectories?, systemPrompt?, _meta?}`
  - `session/load {sessionId, cwd, mcpServers}`: requires agent capability `loadSession`.
    The agent replays the whole conversation as `session/update` and only then responds.
  - `session/resume`: `sessionCapabilities.resume`. Reconnects with no replay.
  - `session/close`: `sessionCapabilities.close`.
  - `session/list`.
  - `session/prompt {sessionId, prompt: ContentBlock[]}`. Responds with `stopReason`, one of
    `end_turn | max_tokens | max_turn_requests | refusal | cancelled`.
  - `session/cancel` (notification). After it, the client MUST answer every pending
    `session/request_permission` with outcome `cancelled`, and the agent MUST end the turn
    with `stopReason: cancelled`.
  - `session/request_permission` (agent→client). Options have `kind` `allow_once`,
    `allow_always`, `reject_once` or `reject_always`. Choose by `kind`, never by position.
  - `session/set_mode`, `session/set_config_option`. Config options are where model,
    mode and effort now live (`/protocol/v1/session-config-options`).
  - `session/update` kinds: `user_message_chunk`, `agent_message_chunk`,
    `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`,
    `available_commands_update` (slash commands), `current_mode_update`,
    `config_option_update`, `session_info_update` (title), `usage_update` (context
    window + cost), `notice`, `compaction_update` / `compaction_summary_chunk`.
  - Slash commands are advertised through `available_commands_update`. The client invokes
    one by sending `/name args` as prompt text (`/protocol/v1/slash-commands`).
- **System prompt is now first-class** (the RFD was completed on 2026-11-06). The agent
  advertises `sessionCapabilities.systemPrompt` and the client sends
  `systemPrompt: {mode: "append"|"override", content: [{type:"text",text}]}` on
  new/load/resume. On load/resume, send the same prompt again. A changed prompt MAY only
  take effect after the next compaction (`/protocol/v1/session-setup#system-prompt`).
- `_meta` is the extension point on every type (`/protocol/v1/extensibility`). Names
  starting with `_` are reserved for custom methods.

### 1.2 The Rust crate
Sources: https://crates.io/crates/agent-client-protocol, https://github.com/agentclientprotocol/rust-sdk

- `agent-client-protocol` **5.3.0** (2026-11-14), MSRV 1.88, Apache-2.0, ~6.3 M downloads.
  The schema crate is `agent-client-protocol-schema` 1.18.0.
- **Churn is high:** 3.0.0 came out 2026-10-06 and 5.0.0 on 2026-11-02, with minors
  every few days. The API is builder style:
  `Client.v1().on_receive_notification(..).on_receive_request(..).connect_with(AcpAgent::from_str(cmd)?, |conn| ..)`.
  Features: `process` (spawn the agent subprocess), `stdio`, and `unstable_*` flags
  (session_fork, session_status, session_set_title, end_turn_token_usage, subagents, …).
  It also ships `-http` (HTTP/SSE/WS transports), `-rmcp` (MCP integration),
  a conductor/proxy framework and a `yopo` example client.
- Maturity: it's official and used by Zed, but the surface is still being reshaped
  (v1/v2 split, proxy chains). **Pin an exact version.** Keep all ACP code behind one
  module (`agents/acp.rs`) that exposes moa's own types. If a bump gets painful, use
  `agent-client-protocol-schema` (types only) plus a hand-rolled JSON-RPC loop. That's
  ~300 lines over tokio and is a legitimate fallback.

### 1.3 What moa needs as a *headless* ACP client
- **Capabilities:** advertise `fs: {readTextFile:false, writeTextFile:false}` and
  `terminal:false`. The adapter then runs Claude Code's own Read/Write/Bash tools, which is
  what we want. moa isn't an editor, and there's no buffer to read unsaved edits from.
  Advertise `session.compaction` so compactions replay cleanly, and form `elicitation` if
  the console will answer AskUserQuestion / MCP elicitations.
- **Permissions:** keep a pending-request table in the daemon keyed by JSON-RPC id. Answer
  from the console. If the console isn't attached, hold the request and raise a "moa needs
  you" item. Optionally auto-answer from moa's own policy (e.g. allow `Bash(moactl *)`).
  Better still, put allow rules into the account's settings so most calls never ask.
- **Always drain the stream.** Adapter issue #1269: a PostToolUse hook awaits delivery of
  `session/update`, so a client that stops reading stalls the session (600 s per tool, then
  a hard stall). The daemon must own the stdio pipe and persist every update to SQLite
  whether or not a console is attached. The console then reads from moa's store, never from
  the pipe.
- **Load vs resume:** after a daemon restart, use `session/resume` when advertised, since
  moa already has the history. Use `session/load` only to rebuild a missing local
  transcript, and de-duplicate by `messageId`. Known issue #1621 (v1): after load/resume
  nothing reads the CLI stream until the first prompt, so that prompt can pick up a
  previous cycle's result.
- **Cancel:** send `session/cancel`, then mark open tool calls cancelled locally, answer
  pending permissions with `cancelled`, and wait for `stopReason: cancelled`.
- **Model/mode:** read `configOptions` from the new/load response and from
  `config_option_update`. Set them with `session/set_config_option` (the model is a
  config option with category `model`). The modes are Claude's permission modes.

### 1.4 `claude-agent-acp`, the Claude adapter
Sources: https://github.com/agentclientprotocol/claude-agent-acp (README, `src/v1/acp-agent.ts`,
`src/new-session-meta.ts`, `src/client-system-prompt.ts`, `src/paths.ts`),
https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp

- **Package:** `@agentclientprotocol/claude-agent-acp` **0.99.1** (2026-11-14). It depends on
  `@anthropic-ai/claude-agent-sdk` 0.3.312 and `@agentclientprotocol/sdk` 2.1.0. The repo
  has 2.8k stars and 158 open issues and publishes nearly daily. Standalone executables
  (bun-compiled, bundling a `claude` CLI) are attached to GitHub releases. Set
  `CLAUDE_CODE_EXECUTABLE` to use your own `claude`; do that, so the agent runs the same
  CLI version as your sessions (issue #1319: the SDK-bundled fallback binary silently drops
  the `user` setting source).
- **Architecture:** it drives the real `claude` CLI through the Agent SDK (stream-json),
  with `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` set. One adapter process serves many ACP
  sessions.
- **Session ID = Claude session UUID.** On resume the adapter reuses the id. The thread is
  therefore an ordinary transcript at `$CLAUDE_CONFIG_DIR/projects/<cwd-slug>/<id>.jsonl`,
  and `claude --resume <id>` in a terminal opens moa's thread too.
- **Env / account:** `CLAUDE_CONFIG_DIR` is read from the process env (`src/paths.ts`).
  That covers transcripts, `session/list`, settings and credentials. **Run one adapter
  process per account.** `_meta.claudeCode.options.env` can override env per session,
  but the adapter's own transcript reading still uses the process env, so mixing accounts
  in one process is fragile. Other env vars it reads: `ANTHROPIC_MODEL`,
  `MAX_THINKING_TOKENS`, `CLAUDE_MODEL_CONFIG`, `CLAUDE_AGENT_ACP_PROTOCOL`
  (`v1|v2|auto`), `CLAUDE_AGENT_LOGS`.
- **Settings sources:** hard-coded `settingSources: ["user","project","local"]`. So
  CLAUDE.md, project `.claude/settings.json`, **user and project hooks**, skills, plugins
  and MCP servers all load, exactly as in a terminal session. The prototype confirmed this
  on 0.71. Consequences:
  - moa's own tracking hooks fire for moa's central session too. Tag it, or treat it as
    the "moa" agent.
  - Issue #1343: CLAUDE.md `@-imports` are not expanded under the adapter.
  - Issue #1388: a plugin-provided **HTTP** MCP server connects but its tools never reach
    the model under ACP. stdio MCP is fine.
- **`_meta` extensions on `session/new` / `session/load`:**
  - `_meta.systemPrompt`: a string replaces the preset. An object is merged into
    `{type:"preset", preset:"claude_code", ...obj}`, so `{append:"…"}` appends. This is
    the legacy path. Prefer the protocol `systemPrompt` field, which takes precedence
    when present.
  - `_meta.claudeCode.options`: an Agent SDK `Options` object. ACP owns and ignores `cwd`,
    `includePartialMessages`, `permissionMode`, `canUseTool` and `executable`. ACP merges
    `hooks`, `mcpServers` and `disallowedTools`. `tools` passes through;
    `allowDangerouslySkipPermissions:false` removes bypass. That makes `model`,
    `plugins` (the equivalent of `--plugin-dir`), `env`, `settings`, `agents` and
    `resume` all reachable.
  - `_meta.claudeCode.emitRawSDKMessages: true | filter[]`: raw SDK messages arrive as the
    ext notification `_claude/sdkMessage`. Useful for debugging or for usage/cost.
  - `_meta.disableBuiltInTools: true`, `_meta.additionalRoots`.
  - `_meta.jetbrains.air.*`: JetBrains AIR extensions (goal, permission presentation,
    session-failure, …). Ignore them.
- **Resume semantics:** `session/load` replays the transcript. `session/resume` restores
  without replay. Both call the SDK with `resume: <id>`. A different `systemPrompt` on
  resume only takes effect at the next compaction, because the CLI records the system
  prompt on the first request.
- **Other known limitations** (open issues, Nov 2026):
  - #1489: no way to pick the permission mode a new session *starts* in.
  - #1196: a permission request was sent while in bypass mode.
  - #1193 / #1445: occasional turns that never resolve (`Query closed before response
    received`, task-notification cycles). moa needs a per-turn watchdog and "restart
    adapter + resume" recovery.
  - #1307: replayed messages carry no timestamps.
  - Steering (mid-turn input) is v1-only through `_session/steering`.

### 1.5 Keeping the central agent pluggable
Sources: https://agentclientprotocol.com/get-started/agents, https://github.com/agentclientprotocol/codex-acp

- **Codex:** `@agentclientprotocol/codex-acp` 2.5.0 (TypeScript). It starts
  `codex app-server` and translates. The old Rust `zed-industries/codex-acp` is
  **archived**. Configure it with `CODEX_PATH`, `CODEX_CONFIG` (JSON merged into the
  session config), `INITIAL_AGENT_MODE`, `CODEX_API_KEY`/ChatGPT login, plus `CODEX_HOME`
  for the account. It supports slash commands, approvals and subagents.
- **Native ACP** (no adapter): Gemini CLI (`gemini --acp`; confirmed locally, 0.47.0),
  GitHub Copilot CLI (preview), Cursor CLI, OpenCode, Goose, Kiro, Qwen Code, Mistral Vibe,
  Kimi, Augment, Factory Droid, Cline, Junie, and others.
- **So:** define moa's central agent as "an ACP agent command plus env". Claude,
  codex-acp and Gemini all fit. Feature-detect `loadSession`, `sessionCapabilities.resume`
  and `sessionCapabilities.systemPrompt`. When `systemPrompt` is absent, fall back to
  putting moa's brief in the first user turn.

---

## 2. Claude Code hooks
Source: https://code.claude.com/docs/en/hooks (`.md` copy in src/hooks.md), Claude Code 2.1.311.

### 2.1 Events (33)
SessionStart, Setup, UserPromptSubmit, UserPromptExpansion, PreToolUse, PermissionRequest,
PermissionDenied, PostToolUse, PostToolUseFailure, PostToolBatch, Notification,
MessageDisplay, SubagentStart, SubagentStop, TaskCreated, TaskCompleted, Stop, StopFailure,
TeammateIdle, InstructionsLoaded, ConfigChange, CwdChanged, DirectoryAdded, FileChanged,
WorktreeCreate, WorktreeRemove, PreCompact, PostCompact, PreModelSwitch, PostModelSwitch,
Elicitation, ElicitationResult, SessionEnd.

### 2.2 Input fields that matter to moa
- **Common:** `session_id`, `prompt_id` (absent before the first prompt), `transcript_path`,
  `cwd`, `scratchpad_dir`, `permission_mode` (`default|plan|acceptEdits|auto|dontAsk|bypassPermissions`;
  not on every event), `effort.level`, `hook_event_name`. Subagent context adds `agent_id`
  and `agent_type`. There's no model env var: `SessionStart.model` is optional, and
  Pre/PostModelSwitch carry `from_model`/`to_model`.
- **SessionStart:** `source` = `startup|resume|clear|compact|fork`, `model?`, `agent_type?`,
  `session_title?`. On resume/fork it also has `seconds_since_last_response`,
  `context_tokens`, `prompt_cache_likely_expired` and `estimated_cache_write_usd`.
- **UserPromptSubmit:** `prompt`, `session_title?`.
- **Stop:**
  - `stop_hook_active` and `last_assistant_message` (use this field; the transcript can lag).
  - `background_tasks[]` (`id`, `type`, `status`, `description`, …).
  - `session_crons[]`. These let moa tell "done" apart from "paused waiting on background
    work".
- **StopFailure** (an API error ends the turn): `error` is one of `rate_limit`,
  `overloaded`, `authentication_failed`, `billing_error`, `server_error`,
  `max_output_tokens`, … Also `error_details` and `last_assistant_message`. **This
  replaces Stop on errors.** Track it, or errored sessions look "working" forever.
- **Notification:** `message`, `title?`, `notification_type` ∈ `permission_prompt`,
  `idle_prompt`, `auth_success`, `auth_storage_failure`, `elicitation_dialog`,
  `elicitation_url_dialog`, `elicitation_complete`, `elicitation_response`,
  `agent_needs_input`, `agent_completed`, `quota_auto_resume_fired|_stale|_disabled`,
  `plugin_notification`.
- **PermissionRequest:** `tool_name`, `tool_input`, `permission_suggestions?`,
  `mcp_server?`. There's no `tool_use_id`.
- **PermissionDenied:** `tool_name`, `tool_input`, `tool_use_id`, `reason`.
- **SessionEnd:** `reason` = `clear|resume|logout|prompt_input_exit|other`.
- **CwdChanged:** `old_cwd`, `new_cwd`.
- **SubagentStart/Stop:** `agent_id`, `agent_type`; Stop adds `last_assistant_message`.
- **PostToolUse:** `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, `duration_ms?`.

### 2.3 Output contract
- **Exit codes:**
  - `0` = success. One JSON object on stdout is parsed as JSON; anything else is plain text.
  - `2` = blocking error, where the event supports it (stderr is the reason).
  - Anything else = non-blocking error, which shows a visible "`<hook> hook error`"
    notice. `onFailure: "block"` (2.1.295+) turns failures into blocks.
- **Plain stdout on exit 0** becomes context on SessionStart, UserPromptSubmit,
  UserPromptExpansion and PostModelSwitch.
- **JSON output:**
  - `continue`, `stopReason`, `systemMessage`, `terminalSequence` (OSC 0/1/2/9/99/777 only).
  - `hookSpecificOutput.{hookEventName, additionalContext, …}`.
  - Top-level `decision:"block"` + `reason`.
- **SessionStart extras:** `additionalContext`, `sessionTitle`, `watchPaths`,
  `reloadSkills`, and `initialUserMessage` (`-p` only).
- **Size cap:** 10,000 chars for `additionalContext` and stdout. Overflow goes to a file
  with a 2,000-char preview.
- **Timeouts** (seconds):
  - command/http/mcp_tool default to 600.
  - UserPromptSubmit and Pre/PostModelSwitch default to 30. MessageDisplay defaults to 10.
  - **SessionEnd shares a 1.5 s budget.** It can be raised to 60 by a per-hook `timeout`
    (not for plugin hooks!) or by `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`.
- **Async:** `async: true` (command only) runs in the background without blocking.
  `asyncRewake` exits 2 to wake Claude. In `-p`, async hooks are cancelled at teardown.

### 2.4 Handler types: is there anything cheaper than a process?
There are five types: `command` (shell form, or exec form with `args`), **`http`**,
`mcp_tool`, `prompt`, `agent`.

- **`http`:**
  - It POSTs the same JSON with `Content-Type: application/json`. The response body uses
    the same JSON output schema; a 2xx with an empty body means success.
  - Fields: `url`, `headers` (`$VAR` interpolation only for names listed in
    `allowedEnvVars`), `onFailure`, `timeout`.
  - **Non-2xx, a connection failure or a timeout is a non-blocking error**, which puts a
    visible notice in the transcript.
  - No `async`. No Unix socket is documented (URL only). Policy can restrict URLs with
    `allowedHttpHookUrls`.
- **`mcp_tool`** calls a tool on a configured MCP server (plugin servers use
  `plugin:<plugin>:<server>`).
- **Matching hooks run in parallel.** An identical handler defined in two settings files
  runs once, but a plugin's copy stays separate.
- **Env:** hooks inherit the Claude process env (minus `OTEL_*`), plus
  `CLAUDE_PROJECT_DIR`, `CLAUDE_PLUGIN_ROOT`/`_DATA` and **`CLAUDE_CODE_SESSION_ID`**
  (env-vars page). `CLAUDE_ENV_FILE` is available to SessionStart, Setup, CwdChanged and
  FileChanged, so they can persist env into later Bash calls.
- **Trust:** interactive sessions hold back *all* settings-file hooks, user ones included,
  until the workspace trust dialog is accepted. `-p`/SDK sessions treat the folder as
  trusted.

**Cheapest hook: verdict.**
- HTTP hooks avoid fork/exec entirely. But they're TCP-only, block the event, and turn
  "daemon down" into visible error notices on every event. They also don't exist in Codex.
- A tiny static Rust binary in exec form (`"command": "/path/moa-hook", "args": ["claude"]`,
  no shell) costs ~1–3 ms of fork+exec. It can always exit 0 silently, spool to disk when
  the daemon is down, talk to a **Unix socket**, and work unchanged for Codex. Mark it
  `async: true` on everything except SessionStart, where moa returns `additionalContext`.
- **Recommend the binary.** Keep `http` as a later optimisation for Claude-only
  high-frequency events (PostToolUse) if those are ever tracked.

### 2.5 Configuration scopes
`~/.claude/settings.json` (here meaning `$CLAUDE_CONFIG_DIR/settings.json`),
`.claude/settings.json`, `.claude/settings.local.json`, managed policy, **plugin
`hooks/hooks.json`** (or the `hooks` key in plugin.json), skill frontmatter and subagent
frontmatter. Hooks from all levels merge, and none replaces another. `--settings <json>`
on the command line adds a session-only settings layer.

---

## 3. Claude Code plugins
Sources: https://code.claude.com/docs/en/plugins-reference, `/plugins/loading`,
`/plugins/cli-reference`, `/channels-reference`, `/mcp`.

- **Layout:**
  - `.claude-plugin/plugin.json` (optional), `skills/<name>/SKILL.md`, `commands/`,
    `agents/`, `hooks/hooks.json`, `.mcp.json`, `.lsp.json`, `output-styles/`,
    `workflows/`, `monitors/monitors.json` (experimental).
  - **`bin/`: "Files here are on the Bash tool's `PATH` while the plugin is enabled."**
  - A CLAUDE.md at the plugin root is *not* loaded; put guidance in a skill.
- **Path vars:**
  - `${CLAUDE_PLUGIN_ROOT}` (the installed copy; read-only in spirit),
    `${CLAUDE_PLUGIN_DATA}` (persistent; deleted on the last uninstall),
    `${CLAUDE_PROJECT_DIR}`.
  - They're exported to hook commands and stdio MCP servers, **but not to the Bash tool's
    environment**. So `bin/moactl` must find the daemon some other way (a well-known socket
    path, or env).
- **Hooks inside the plugin: yes.** Installing or enabling the plugin also installs the
  tracking. Two caveats:
  - plugin hook timeouts don't raise the SessionEnd budget;
  - the identical-handler dedupe doesn't apply across a plugin and user settings, so
    don't install both.
- **Local directory marketplace:** `claude plugin marketplace add /abs/dir` (scope
  user/project/local), then `claude plugin install moa@<mkt> --scope user`. Both run per
  `CLAUDE_CONFIG_DIR`. Relative-path plugins in a directory marketplace **load in place**
  (edits apply at the next session start or `/reload-plugins`). The prototype confirmed
  that `extraKnownMarketplaces` + `enabledPlugins` settings alone don't register a local
  marketplace. Both `install` and `marketplace add` support `--json` for scripting.
- **Session-only loading (new, important):**
  - `claude --plugin-dir <dir>` (repeatable), or **`CLAUDE_CODE_PLUGIN_DIRS=/a:/b`**
    (2.1.280+, absolute paths). This loads a plugin for that session without installing
    it, and it shows up as `<name>@inline`.
  - From the Agent SDK it's the `plugins` option, reachable through
    `_meta.claudeCode.options.plugins` on the ACP adapter.
  - A session-only copy shadows an installed plugin of the same name.
  - Managed `disableSideloadFlags` can forbid it.
  - Consequence: **tracking and messaging for moa-launched sessions need no per-account
    install at all.** Installing per account is only for reaching sessions started
    *outside* moa (the prototype's "every session knows moa" goal). moa can even
    *generate* the plugin dir at startup (templating the socket path or port into
    `hooks.json`).
- **Settings `env` and `enabledPlugins`:**
  - `env` in user settings reaches the session, its hooks and Bash.
  - `CLAUDE_CONFIG_DIR` and `CLAUDE_CODE_PROJECT_DIR_NAME` are ignored in project/local
    `env`.
  - `enabledPlugins` is the on/off map keyed `name@marketplace`.
- **MCP server vs CLI for agent→moa messaging:**

  | | `bin/moactl` (CLI + skill) | Plugin MCP server |
  |---|---|---|
  | Idle cost | none | stdio: one process per session; http/ws: none (daemon serves it) |
  | Context cost | skill description always loaded | tool names + server `instructions`; definitions deferred by tool search |
  | Caller identity | `CLAUDE_CODE_SESSION_ID` env in Bash | stdio: same env var (kept from spawn, so stale after `/clear`); http: `headersHelper`/env-header |
  | Permissions | needs `Bash(moactl *)` allow rule; the prototype saw prompts and `$M` tricks | `mcp__plugin_moa_moa__*` allow rule; structured args, no shell quoting |
  | Long waits (`ask --wait`) | Bash timeout (default 2 min, max 10) | per-server `timeout`; progress notifications |
  | Usable by humans and scripts | yes | no |
  | Push *into* the session | no (tmux typing) | **yes, via Channels** (preview) |
  | Works for Codex | yes | yes (Codex MCP) |
  | ACP central agent | yes | stdio yes; **http plugin MCP broken under ACP (#1388)** |

  **Recommendation:** keep **moactl + skill** as the primary path, since it's also the
  human CLI and the Codex path. Add an MCP surface only for what a CLI can't do: Channels
  push. **Channels** (`notifications/claude/channel`, with `<channel source=… meta…>`
  injected into the session; two-way via a reply tool; permission relay via
  `notifications/claude/channel/permission_request` and `.../permission`) would replace
  `tmux send-keys` for `moactl ask`. Blockers today:
  - it's a research preview;
  - custom channels need `--dangerously-load-development-channels`, which **prompts for
    confirmation and is ignored with `-p`/SDK**;
  - it needs claude.ai/Console auth, and Team/Enterprise orgs must enable `channelsEnabled`;
  - a channel server that negotiates MCP revision 2026-07-28 isn't registered (set
    `MCP_PROTOCOL_NEGOTIATION=legacy`).

  Revisit when custom channels leave the allowlist.

---

## 4. Starting and resuming Claude sessions
Sources: https://code.claude.com/docs/en/cli-reference, `/sessions`, `/env-vars`, `/statusline`,
`/agent-view`, `/terminal-config`.

- **`--session-id <uuid>`** pre-assigns the id on a fresh launch, so moa knows it before
  any hook. It must be a valid UUID. Use UUIDv4 or v7 generated by moa.
- **`--resume <id|name|/abs/path.jsonl>`** keeps the same id. Since 2.1.223 a bare id is
  searched across *all* projects on that config dir. **`--fork-session`** issues a new id
  (SessionStart `source:"fork"`). **`--continue`** takes the most recent in the cwd and
  skips `-p`/SDK sessions, so don't use it from moa. **`--name/-n`** sets the display
  name, and `--resume <name>` works.
- **ID rotation:** `/clear` ends the session (SessionEnd `reason:clear`) and starts a new
  id (SessionStart `source:clear`). Interactive `/resume` switches ids too
  (`reason:resume`). `CLAUDE_CODE_SESSION_ID` follows `/clear` in Bash and hooks, but a
  stdio MCP server keeps its spawn id. **moa must key a "slot" (pane/agent) separately
  from the Claude session id** and link them through an env token
  (`MOA_AGENT_ID=<slot>`) that the hook echoes back.
- **Transcripts:** `$CLAUDE_CONFIG_DIR/projects/<cwd with non-alphanumerics→'->/<session-id>.jsonl`.
  Names over 200 chars are truncated and hashed. `CLAUDE_CODE_PROJECT_DIR_NAME` (with
  `CLAUDE_CONFIG_DIR`) names the folder explicitly. Retention defaults to 30 days
  (`cleanupPeriodDays`). The format is explicitly internal and unstable, so don't parse it
  for state. Use hooks, and the transcript only for "read the last message" fallbacks.
- **Env vars worth setting at launch:**
  - `CLAUDE_CONFIG_DIR` (account), `CLAUDE_CODE_PLUGIN_DIRS` (moa plugin), `MOA_AGENT_ID`
    (slot).
  - `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS` (only if SessionEnd has to do work).
  - Leave `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` alone. The title/status glyph is a free
    secondary signal: inside tmux, Claude Code queries tmux for OSC 7501 "Program
    Status" support, otherwise it prefixes the pane title with a state glyph.
- **Waiting for permission vs idle, reliably:**
  - **Needs you (permission):** `PermissionRequest` fires immediately. Clear it on the
    next `PostToolUse`/`PostToolUseFailure`/`PermissionDenied` for that tool, or on
    `Stop`/`UserPromptSubmit`. Don't rely on `Notification:permission_prompt`: it fires
    after ~6 s without typing and is reset by keystrokes (the prototype saw this noise).
  - **Needs you (question):** `PreToolUse` with `tool_name` `AskUserQuestion`, or
    `ExitPlanMode` in plan mode. `Notification:elicitation_dialog` covers MCP forms.
  - **Done/idle:** `Stop` (look at `background_tasks`/`session_crons` to tell finished
    from waiting on background work). `idle_prompt` comes ~60 s later and isn't needed.
  - **Errored:** `StopFailure` (e.g. `rate_limit`). Also
    `Notification:quota_auto_resume_*`.
  - **Working:** `UserPromptSubmit` → working until Stop or StopFailure. An interrupt
    (Esc) fires *no* Stop ("does not run if the stoppage occurred due to a user
    interrupt"). Detect it by a following UserPromptSubmit, or by a liveness check on the
    pane (process alive, no events for N minutes, pane title glyph).
  - **Gone:** `SessionEnd`, plus process exit seen by moa (a hard kill skips SessionEnd).
- **Statusline as a state source:**
  - It runs on each assistant message, compact, mode change, etc., debounced 300 ms, plus
    an optional `refreshInterval`.
  - Its JSON has `session_id`, `session_name`, `model`, `cost`, `context_window`,
    **`rate_limits.five_hour/seven_day.{used_percentage,resets_at}`**, `prompt_cache`,
    `worktree` and `pr`.
  - Not good for working/waiting state: it doesn't run on permission prompts. Very good
    for **per-account quota** and context-fill, which matter with several accounts.
  - It's one command per config dir and conflicts with the user's own statusline. moa
    could offer an optional wrapper that tees the JSON to the daemon and then runs the
    user's command.
- **Claude Code's own background sessions overlap with moa.** There's a supervisor daemon,
  `claude --bg`, `claude agents --json`, `attach`/`logs`/`stop`, and states like "Needs
  input/Working/Completed". It's worth watching. `claude agents --json` per config dir is
  another read-only state source, but moa should not depend on it.

---

## 5. Codex
Sources: https://developers.openai.com/codex/hooks, `/codex/cli/reference`, `/codex/config-advanced`,
`/codex/app-server`; local `codex --help` 0.167.2; local `~/.codex/sessions/`.

- **Hooks:**
  - Locations: `$CODEX_HOME/hooks.json` (default `~/.codex`), inline `[hooks]` in
    `config.toml`, `<repo>/.codex/…` (only when the project is trusted), and plugin-bundled
    `hooks/hooks.json` (Codex also sets `CLAUDE_PLUGIN_ROOT`/`_DATA` for compatibility).
  - Events: SessionStart (`source` = startup|resume|clear|compact|fork), SessionEnd
    (`reason` is always `other`; also fires after 30 min idle and not open), SubagentStart,
    PreToolUse, **PermissionRequest** (`tool_name`, `tool_input`, `turn_id`; fires when
    Codex is about to ask), PostToolUse, PreModelRequest/PostModelRequest, Pre/PostCompact,
    UserPromptSubmit (`prompt`, `turn_id`), SubagentStop, **Stop** (`last_assistant_message`,
    `stop_hook_active`, `turn_id`), **Interrupt** (1 s timeout, 3 s max).
  - Common input: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`,
    `permission_mode`.
  - Handlers: `command` and `mcp_tool` only; `prompt`/`agent` are parsed and skipped.
    There's no http. `async` is supported. Default timeout 600 s, 1 s for
    SessionEnd/Interrupt.
  - **Trust:** every non-managed hook must be reviewed and trusted in `/hooks`, recorded
    against the hook's hash. A changed command string, or a plugin script update, needs
    re-trust. `--dangerously-bypass-hook-trust` skips that for one run, but it bypasses
    *all* hooks, repo ones included. Managed hooks (`requirements.toml`) are trusted by
    policy.
  - **Recommendation:** install one stable entry per `CODEX_HOME`
    (`/abs/stable/path/moa-hook codex`) and trust it once. The binary gates itself on
    `MOA_AGENT_ID` and exits 0 silently otherwise.
- **No `--session-id` at launch.** moa learns the id from SessionStart (`session_id`),
  correlated by `MOA_AGENT_ID` in the env. Resume with `codex resume <uuid|name> [prompt]`
  (`--last`, `--all`). There's also `codex fork <id>`.
- **`codex queue --thread <id> --message …`** queues a message to a session and can
  resume it on a daemon. That's a native "message a peer" primitive with no tmux typing.
- **Transcripts:** `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl` (seen
  locally, UUIDv7 ids). The format isn't a stable interface.
- **`notify`:** `notify = ["prog", …]` gets one JSON argv. The type is only
  `agent-turn-complete`, with `thread-id`, `turn-id`, `cwd`, `input-messages` and
  `last-assistant-message`. Hooks supersede it, so skip it.
- **App-server, the stronger option.**
  - `codex app-server --listen unix://PATH|ws://…` is JSON-RPC: `thread/start`,
    `thread/resume`, `turn/start`, steer, `thread/loaded/list`, and
    **`thread/status/changed`** with `{type:"active", activeFlags:["waitingOnApproval"]}`.
  - The TUI attaches with `codex --remote unix://PATH`.
  - moa could run one app-server per `CODEX_HOME` and launch TUIs against it. State would
    then come from a subscription rather than hooks, and approvals and messaging would be
    native.
  - It's marked `[experimental]`. Treat it as a phase-2 Codex backend; hooks are phase 1.

---

## 6. Recommendations

### 6.1 Event contract: daemon ← agents
**Transport:**
- `moa-hook <agent-kind>` is a small static binary in exec form. It reads stdin, wraps it,
  writes one NDJSON line to `$MOA_RUNTIME/moa.sock` (Unix socket, `0600`), and exits 0.
- It **never fails the agent:** if the socket is down it appends to `$MOA_STATE/spool/` and
  exits 0.
- Only `SessionStart` waits (≤500 ms) for a reply carrying `additionalContext`. Everything
  else is fire-and-forget (`async: true` in Claude).
- `moactl` uses the same socket with request/response framing.

**Envelope** (normalized; the raw payload is kept verbatim for forward compatibility):
```json
{
  "v": 1,
  "source": "claude" | "codex" | "acp" | "moa",
  "account": "<config dir id, from CLAUDE_CONFIG_DIR / CODEX_HOME>",
  "slot": "<MOA_AGENT_ID from env, null if not moa-launched>",
  "session_id": "<agent's own id>",
  "kind": "session.started|session.ended|turn.started|turn.ended|turn.failed|turn.interrupted|needs.permission|needs.answer|needs.cleared|tool.started|tool.ended|subagent.started|subagent.ended|cwd.changed|model.changed|usage|compacted",
  "at": "<RFC3339 received-at>",
  "seq": "<per-hook-process monotonic, for dedupe>",
  "cwd": "...", "transcript_path": "...", "permission_mode": "...",
  "detail": { "source|reason|prompt|last_message|tool|tool_input|error|old_cwd|new_cwd|rate_limits|context_pct": "..." },
  "raw": { "...the original hook JSON..." }
}
```

**Mapping (Claude → kind):**

| Claude event | moa kind |
|---|---|
| SessionStart | `session.started` (+`detail.source`) |
| UserPromptSubmit | `turn.started` |
| Stop | `turn.ended` (+`last_message`, background tasks) |
| StopFailure | `turn.failed` |
| PermissionRequest | `needs.permission` |
| PreToolUse[AskUserQuestion\|ExitPlanMode] | `needs.answer` |
| PostToolUse / PostToolUseFailure / PermissionDenied | `needs.cleared` |
| SessionEnd | `session.ended` |
| CwdChanged | `cwd.changed` |
| PostModelSwitch | `model.changed` |
| SubagentStart/Stop | `subagent.*` |
| statusline (optional) | `usage` |

Codex maps the same way. Codex's own `Interrupt` maps to `turn.interrupted`. The ACP
central agent's `session/update` stream maps onto the same kinds inside the daemon, so the
UI has a single model.

**Hook events to subscribe:** SessionStart, UserPromptSubmit, Stop, StopFailure,
PermissionRequest, PreToolUse (matcher `AskUserQuestion|ExitPlanMode` only, so tool calls
don't flood), PostToolUse/PostToolUseFailure/PermissionDenied (only to clear "needs",
or skip and clear on Stop), SessionEnd, CwdChanged. **Drop** `Notification` except
`elicitation_dialog`. Keep it as a fallback for permission only when PermissionRequest
isn't available.

**Delivery:** one moa-generated plugin dir per agent kind, injected with
`CLAUDE_CODE_PLUGIN_DIRS` at launch. It holds `hooks/hooks.json`, `skills/moa/SKILL.md`
and `bin/moactl`. Per-account `claude plugin install` from a directory marketplace is the
opt-in "make every session on this account moa-aware" mode, gated by
`MOA_ONLY_UNDER`/slot as today.

### 6.2 Hosting the central agent
- **Supervision:**
  - The daemon owns one ACP subprocess per (agent-kind, account): e.g.
    `claude-agent-acp` with `CLAUDE_CONFIG_DIR=…` and
    `CLAUDE_CODE_EXECUTABLE=$(which claude)`, pinned to a moa-tested version.
  - It speaks ACP v1 (`CLAUDE_AGENT_ACP_PROTOCOL=v1`) and moves to v2 only once v2 is
    stable.
  - It restarts with backoff when the process dies, and resumes the current thread with
    `session/resume`.
- **Clients never talk ACP.** The console(s) attach to the daemon. The daemon persists
  every `session/update` (it must drain the pipe continuously, issue #1269). It
  multiplexes prompts and permissions to whichever console is attached, and parks
  permission requests as "needs you" items when none is. Closing or restarting the console
  doesn't touch the agent. Restarting the daemon costs one `session/resume`.
- **Thread per day:**
  - On the first prompt after the local day boundary: `session/new` with
    `cwd = moa's brain dir`, `systemPrompt {mode:"append"}` = moa constitution +
    compact brief (~2 KB, per the prototype learnings).
  - Pass `_meta.claudeCode.options.plugins` for moactl if it isn't installed.
  - Store `day → sessionId` in SQLite. Yesterday's thread stays resumable (it's a normal
    transcript, so `claude --resume <id>` works from a terminal too).
  - Carry continuity in through a "yesterday summary" in the brief, not by resuming a
    month-long context.
- **Turn watchdog:** if no `session/update` arrives for N minutes during a turn, send
  `session/cancel`. If the prompt still doesn't resolve, kill and respawn the adapter
  (issues #1193, #1445).
- **Pluggability:** the agent spec is `{command, args, env, protocol}`. It has been tested
  against the claude-agent-acp capability set. Codex (`codex-acp`) and Gemini
  (`gemini --acp`) get the same treatment, with feature detection for
  resume/load/systemPrompt.

### 6.3 Risks
1. **ACP churn.**
   - The Rust crate had 3 majors in 6 weeks.
   - The adapter ships daily and has known hangs. Our Nix build (0.71) is 28 minors behind.
   - **Mitigation:** pin both. Isolate ACP behind one module. Keep a schema-only fallback.
     Run a smoke test in CI against a recorded adapter transcript.
2. **The hidden coupling of `settingSources: user,project,local`.** Every user hook and
   plugin also runs inside moa's central agent, moa's own tracking included. Mark
   moa's session (`MOA_AGENT_ID=moa`) so the daemon doesn't list it as a worker.
3. **Hook trust and visibility.**
   - Claude interactive sessions don't run any hooks until the workspace trust dialog is
     accepted. A fresh folder looks "untracked" until then.
   - Codex needs `/hooks` trust per hash, so keep the hook command string byte-stable
     across moa upgrades: point it at a stable symlink.
4. **Interrupts emit no Stop.** State can stick on "working". Pair hooks with a liveness
   check (pane/process alive, time since last event, pane-title glyph).
5. **ID rotation on `/clear` and interactive `/resume`.** Without a slot id carried in
   env, moa would see new unrelated sessions.
6. **Transcript formats are declared internal** in both Claude and Codex. Use
   `last_assistant_message` from Stop and avoid parsing JSONL except for best-effort
   previews.
7. **SessionEnd budget is 1.5 s**, and plugin hook timeouts can't raise it. The hook must
   just enqueue.
8. **HTTP hooks** would surface "hook error" notices whenever the daemon is down. That's
   another reason for the binary + spool.
9. **Channels** are the right long-term inbound path, but preview-only. Until then
   `moactl ask` keeps typing into panes for Claude. For Codex, `codex queue` /
   app-server steer is cleaner.
10. **Claude Code is growing its own supervisor and agent view** (`claude --bg`,
    `claude agents --json`). Expect overlap; keep moa's value in the cross-account brain
    and console rather than in the process supervision itself.

---

## Sources
- ACP spec: https://agentclientprotocol.com/protocol/v1/overview ·
  https://agentclientprotocol.com/protocol/v1/session-setup ·
  https://agentclientprotocol.com/protocol/v1/prompt-turn ·
  https://agentclientprotocol.com/protocol/v1/tool-calls ·
  https://agentclientprotocol.com/protocol/v1/cancellation ·
  https://agentclientprotocol.com/protocol/v1/slash-commands ·
  https://agentclientprotocol.com/protocol/v1/session-config-options ·
  https://agentclientprotocol.com/protocol/v2/migration ·
  https://agentclientprotocol.com/rfds/updates · https://agentclientprotocol.com/get-started/agents
- Rust SDK: https://github.com/agentclientprotocol/rust-sdk ·
  https://crates.io/crates/agent-client-protocol (5.3.0, 2026-11-14) ·
  https://docs.rs/agent-client-protocol
- Claude adapter: https://github.com/agentclientprotocol/claude-agent-acp (README;
  src/v1/acp-agent.ts `createSession`; src/new-session-meta.ts; src/client-system-prompt.ts;
  src/paths.ts; issues #1193 #1269 #1307 #1319 #1343 #1388 #1445 #1489 #1621) ·
  https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp (0.99.1)
- Codex adapter: https://github.com/agentclientprotocol/codex-acp (2.5.0) ·
  https://github.com/zed-industries/codex-acp (archived)
- Claude Code: https://code.claude.com/docs/en/hooks · https://code.claude.com/docs/en/plugins-reference ·
  https://code.claude.com/docs/en/plugins/loading · https://code.claude.com/docs/en/plugins/cli-reference ·
  https://code.claude.com/docs/en/channels · https://code.claude.com/docs/en/channels-reference ·
  https://code.claude.com/docs/en/mcp · https://code.claude.com/docs/en/cli-reference ·
  https://code.claude.com/docs/en/sessions · https://code.claude.com/docs/en/env-vars ·
  https://code.claude.com/docs/en/statusline · https://code.claude.com/docs/en/agent-view ·
  https://code.claude.com/docs/en/terminal-config
- Codex: https://developers.openai.com/codex/hooks · https://developers.openai.com/codex/cli/reference ·
  https://developers.openai.com/codex/config-advanced (notify, CODEX_HOME) ·
  https://developers.openai.com/codex/app-server
