#!/bin/bash
# THROWAWAY SPIKE: SessionStart hook for the master. Stdout becomes context.
# The brief comes from the brain API (moactl), kept short on purpose.
"$(cd "$(dirname "$0")/../../.." && pwd)/moa-proto/moactl" brief
db="$(cd "$(dirname "$0")/../.." && pwd)/brain.db"
sqlite3 "$db" "INSERT INTO meta(key,value) VALUES('cursor',(SELECT coalesce(max(id),0) FROM events)) ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
exit 0
