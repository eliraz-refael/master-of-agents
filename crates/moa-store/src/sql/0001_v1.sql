CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE, path TEXT,
  status TEXT NOT NULL DEFAULT 'active', summary TEXT, last_activity_on TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE sessions (id TEXT PRIMARY KEY, agent_kind TEXT NOT NULL, account TEXT, cwd TEXT, start_cwd TEXT,
  project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL, title TEXT, title_locked INTEGER NOT NULL DEFAULT 0,
  group_path TEXT, lifecycle TEXT NOT NULL, activity TEXT NOT NULL DEFAULT 'idle', activity_at INTEGER,
  activity_why TEXT, last_prompt TEXT, last_turn_end_at INTEGER, seen_at INTEGER, asleep_at INTEGER,
  archived_at INTEGER, location TEXT NOT NULL DEFAULT 'here', resume_failed INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, last_seen_at INTEGER) STRICT;
CREATE TABLE tasks (id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
  title TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'todo', blocked_on TEXT, today TEXT,
  session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL, note TEXT, who TEXT, due TEXT, priority TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, done_at INTEGER) STRICT;
CREATE TABLE notes (id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
  kind TEXT NOT NULL DEFAULT 'fact', text TEXT NOT NULL, created_at INTEGER NOT NULL) STRICT;
CREATE TABLE suggestions (id INTEGER PRIMARY KEY, text TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'open',
  actions TEXT NOT NULL DEFAULT '[]', actions_raw TEXT, actions_invalid TEXT, result TEXT,
  created_at INTEGER NOT NULL, resolved_at INTEGER) STRICT;
CREATE TABLE conversations (agent_kind TEXT NOT NULL, conversation_id TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES sessions(id), store_dir TEXT, spike_account TEXT, reason TEXT NOT NULL,
  first_seen_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL, transcript_path TEXT,
  transcript_present INTEGER, transcript_checked_at INTEGER, PRIMARY KEY (agent_kind, conversation_id)) STRICT;
CREATE INDEX conversations_session ON conversations(session_id, first_seen_at);
CREATE TABLE pane_bindings (session_id TEXT PRIMARY KEY REFERENCES sessions(id), server_pid INTEGER NOT NULL,
  server_start INTEGER NOT NULL, socket TEXT NOT NULL, pane_id TEXT NOT NULL, pane_pid INTEGER NOT NULL,
  bound_at INTEGER NOT NULL, tracked INTEGER NOT NULL DEFAULT 1) STRICT;
CREATE TABLE spike_panes (session_id TEXT PRIMARY KEY REFERENCES sessions(id), pane_id TEXT NOT NULL,
  tmux TEXT, imported_at INTEGER NOT NULL) STRICT;
CREATE TABLE terminals (id INTEGER PRIMARY KEY, server_pid INTEGER NOT NULL, server_start INTEGER NOT NULL,
  pane_id TEXT NOT NULL, opened_by TEXT NOT NULL, cwd TEXT NOT NULL,
  session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL, opened_at INTEGER NOT NULL, touched_at INTEGER,
  name TEXT NOT NULL, note TEXT, timeout_at INTEGER, shell TEXT NOT NULL, command TEXT NOT NULL,
  command_since INTEGER NOT NULL, closed_at INTEGER) STRICT;
CREATE UNIQUE INDEX terminals_open ON terminals(server_pid, server_start, pane_id) WHERE closed_at IS NULL;
CREATE TABLE events (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, session_id TEXT, conversation_id TEXT,
  account TEXT, cwd TEXT, kind TEXT NOT NULL, text TEXT, detail TEXT) STRICT;
CREATE INDEX events_session ON events(session_id, id);
CREATE TABLE activity (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, who TEXT NOT NULL, text TEXT NOT NULL,
  ref_kind TEXT, ref_id TEXT) STRICT;
CREATE TABLE groups (path TEXT PRIMARY KEY, collapsed INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL) STRICT;
CREATE TABLE asks (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, from_session TEXT, from_name TEXT,
  to_session TEXT, text TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', reply TEXT,
  claimed_at INTEGER, answered_at INTEGER) STRICT;
CREATE TABLE moa_threads (thread_id TEXT PRIMARY KEY, account TEXT NOT NULL, day TEXT NOT NULL,
  kind TEXT NOT NULL, started_at INTEGER NOT NULL) STRICT;
CREATE TABLE feed (id INTEGER PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES moa_threads(thread_id),
  at INTEGER NOT NULL, kind TEXT NOT NULL, message_id TEXT, automatic INTEGER NOT NULL DEFAULT 0,
  body TEXT NOT NULL, state TEXT) STRICT;
CREATE TABLE nags (task_id INTEGER NOT NULL, due TEXT NOT NULL, day TEXT NOT NULL,
  PRIMARY KEY (task_id, due, day)) STRICT;
CREATE TABLE reports (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, kind TEXT NOT NULL,
  queued_at INTEGER NOT NULL, last_at INTEGER NOT NULL, turns TEXT NOT NULL,
  UNIQUE (session_id, kind)) STRICT;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT) STRICT;
