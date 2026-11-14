-- THROWAWAY SPIKE: the brain for the "master chat" feel test. Not the product schema.
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS projects (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  path          TEXT,
  area          TEXT NOT NULL DEFAULT 'private',
  status        TEXT NOT NULL DEFAULT 'active',  -- idea | active | paused | done
  summary       TEXT,
  last_activity TEXT,                            -- ISO date of last commit / session
  created_at    TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS tasks (
  id          INTEGER PRIMARY KEY,
  project_id  INTEGER REFERENCES projects(id),
  title       TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'todo',  -- idea | todo | active | waiting | blocked | done | dropped
  blocked_on  TEXT,
  today       TEXT,                          -- YYYY-MM-DD when it's on that day's list
  session_id  TEXT,
  note        TEXT,
  who         TEXT,                          -- you | agent | other: whose move it is
  due         TEXT,                          -- YYYY-MM-DD: when it's expected (a wait) or due
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS notes (
  id          INTEGER PRIMARY KEY,
  project_id  INTEGER REFERENCES projects(id),
  kind        TEXT NOT NULL DEFAULT 'fact',  -- fact | preference | routine | observation | thread-summary
  text        TEXT NOT NULL,
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS suggestions (
  id          INTEGER PRIMARY KEY,
  text        TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'open',  -- open | accepted | rejected | later
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  resolved_at TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
  session_id  TEXT PRIMARY KEY,
  account     TEXT,
  cwd         TEXT,
  project_id  INTEGER REFERENCES projects(id),
  title       TEXT,
  state       TEXT,                          -- working | idle | waiting | ended
  last_prompt TEXT,
  first_seen  TEXT,
  last_seen   TEXT,
  pane        TEXT,                          -- tmux pane id, when it runs in tmux
  tmux        TEXT,                          -- tmux socket path
  grp         TEXT                           -- tree group, e.g. 'Private/Neeo'
);

CREATE TABLE IF NOT EXISTS events (
  id          INTEGER PRIMARY KEY,
  ts          TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  session_id  TEXT,
  account     TEXT,
  cwd         TEXT,
  kind        TEXT NOT NULL,                 -- start | prompt | stop | waiting | end
  text        TEXT
);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT
);

-- Agents asking each other through moa (`moactl ask`). to_sid is a session ID, or 'moa'
-- for moa itself. status: pending (waiting for the peer to be free) | sent (typed in) | received (its prompt
-- was submitted; the turn it starts answers it) | answered | failed (never arrived as a prompt) | dropped
-- (the asker stopped waiting before the peer was free). moa's own asks skip received. A failed or
-- dropped ask's reply says why.
CREATE TABLE IF NOT EXISTS asks (
  id          INTEGER PRIMARY KEY,
  ts          TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  from_sid    TEXT,
  from_name   TEXT,
  to_sid      TEXT NOT NULL,
  text        TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'pending',
  reply       TEXT,
  answered_at TEXT
);
