CREATE TABLE IF NOT EXISTS projects (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  path          TEXT,
  area          TEXT NOT NULL DEFAULT 'private',
  status        TEXT NOT NULL DEFAULT 'active',
  summary       TEXT,
  last_activity TEXT,
  created_at    TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS tasks (
  id          INTEGER PRIMARY KEY,
  project_id  INTEGER REFERENCES projects(id),
  title       TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'todo',
  blocked_on  TEXT,
  today       TEXT,
  session_id  TEXT,
  note        TEXT,
  who         TEXT,
  due         TEXT,
  priority    TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS notes (
  id          INTEGER PRIMARY KEY,
  project_id  INTEGER REFERENCES projects(id),
  kind        TEXT NOT NULL DEFAULT 'fact',
  text        TEXT NOT NULL,
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime'))
);

CREATE TABLE IF NOT EXISTS suggestions (
  id          INTEGER PRIMARY KEY,
  text        TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'open',
  action      TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  resolved_at TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
  session_id  TEXT PRIMARY KEY,
  account     TEXT,
  cwd         TEXT,
  project_id  INTEGER REFERENCES projects(id),
  title       TEXT,
  state       TEXT,
  last_prompt TEXT,
  first_seen  TEXT,
  last_seen   TEXT,
  pane        TEXT,
  tmux        TEXT,
  grp         TEXT,
  asleep_at   TEXT
);

CREATE TABLE IF NOT EXISTS events (
  id          INTEGER PRIMARY KEY,
  ts          TEXT NOT NULL DEFAULT (datetime('now','localtime')),
  session_id  TEXT,
  account     TEXT,
  cwd         TEXT,
  kind        TEXT NOT NULL,
  text        TEXT
);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT
);

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

CREATE INDEX IF NOT EXISTS events_session ON events(session_id, id);
CREATE TABLE IF NOT EXISTS groups (path TEXT PRIMARY KEY, created_at TEXT DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS activity (
  id INTEGER PRIMARY KEY, ts TEXT NOT NULL DEFAULT (datetime('now','localtime')), who TEXT, text TEXT);
