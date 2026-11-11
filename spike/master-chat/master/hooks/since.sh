#!/bin/bash
# THROWAWAY SPIKE: UserPromptSubmit hook for the master. Prints what tracked
# sessions did since the master's last turn; prints nothing if nothing happened.
db="$(cd "$(dirname "$0")/../.." && pwd)/brain.db"
[ -f "$db" ] || exit 0
cur=$(sqlite3 "$db" "SELECT coalesce((SELECT value FROM meta WHERE key='cursor'),0);")
out=$(sqlite3 -cmd ".timeout 2000" -separator ' | ' "$db" "
  SELECT e.ts, substr(e.session_id,1,8), coalesce(p.name, e.cwd), e.kind, replace(substr(coalesce(e.text,''),1,200), char(10), ' ')
  FROM events e LEFT JOIN sessions s ON s.session_id=e.session_id LEFT JOIN projects p ON p.id=s.project_id
  WHERE e.id > $cur ORDER BY e.id;" 2>/dev/null)
if [ -n "$out" ]; then
  echo "## Since your last message (tracked sessions: time | session | project | event | text)"
  echo "$out"
fi
sqlite3 "$db" "INSERT INTO meta(key,value) VALUES('cursor',(SELECT coalesce(max(id),0) FROM events)) ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
exit 0
