#!/bin/bash
# THROWAWAY SPIKE: (re)create brain.db and seed it with real facts only:
# git state of ~/private/*, Lantern, and what docs/context.md says about Eliraz.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
db="$here/brain.db"
[ "${1:-}" = "--reset" ] && rm -f "$db" "$db-wal" "$db-shm"
sqlite3 "$db" < "$here/schema.sql" >/dev/null

q() { printf "'%s'" "${1//\'/\'\'}"; }

sql=""
for dir in "$HOME"/private/*/; do
  dir=${dir%/}
  [ -d "$dir/.git" ] || continue
  name=$(basename "$dir")
  last=$(git -C "$dir" log -1 --format='%cs' 2>/dev/null || true)
  subj=$(git -C "$dir" log -1 --format='%s' 2>/dev/null | cut -c1-120 || true)
  [ -n "$last" ] || continue
  sql+="INSERT OR IGNORE INTO projects(name,path,summary,last_activity) VALUES($(q "$name"),$(q "$dir"),$(q "last commit: $subj"),$(q "$last"));"$'\n'
done

sql+=$(cat <<'SQL'
INSERT OR IGNORE INTO projects(name,path,status,summary,last_activity) VALUES
 ('master-of-agents','/Users/elirazk/private/master-of-agents','active',
  'This app. Design phase: brainstorming the product with Claude (2026-11-10); spike of the master chat running.',
  '2026-11-10'),
 ('lantern','/Users/elirazk/private/lantern','idea',
  'Smart file explorer: TUI + web app over chosen folders, a DB of file info filled by AI over ACP. Was called Shelf. Only HTML mockups so far (./mockups).',
  '2026-11-06');

INSERT INTO notes(project_id,kind,text) VALUES
 ((SELECT id FROM projects WHERE name='lantern'),'fact',
  'Lantern conversation: 92033b3c-df78-4a5d-8de8-8d68d3e1a072 on claude-lead. Started 2026-11-06 in ~/Downloads as "clean up my Downloads". Moved to ~/private/lantern on 2026-11-10; resume: cd ~/private/lantern && claude-lead --resume 92033b3c-df78-4a5d-8de8-8d68d3e1a072'),
 (NULL,'fact','Eliraz is a tech lead with no fixed workday: the job is to improve, fix and invent things and come up with ideas. Also runs many personal projects under ~/private.'),
 (NULL,'fact','Accounts: claude-work = the day job (never in this brain). claude-lead and claude-neeo = private. claude-personal is unused.'),
 (NULL,'preference','Keyboard-first. Wants clarity and the big picture; wants to be nudged about what needs attention and offered things to hand to other agents.'),
 (NULL,'preference','Nudges only inside the app/session, no notifications or Slack.');
SQL
)

sqlite3 "$db" "$sql"
sqlite3 "$db" "SELECT count(*) || ' projects seeded' FROM projects;"
