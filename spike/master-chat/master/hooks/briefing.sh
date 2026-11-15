#!/bin/bash
# THROWAWAY SPIKE: SessionStart hook for the master. Stdout becomes context.
# The brief comes from the brain API (moactl), kept short on purpose.
chat="$(cd "$(dirname "$0")/../.." && pwd)"
# which brain this moa is (the moa launcher sets these; a moa started before brains: the default)
scope="${MOA_SCOPE:-$chat/private-scope.md}"
db="${MOA_DB:-$chat/brain.db}"
echo "## Who you are (${MOA_BRAIN:-the default} brain; your brain.db is $db)"
cat "$scope" 2>/dev/null || echo "(no scope file at $scope)"
echo
"$chat/../moa-proto/moactl" brief
sqlite3 "$db" "INSERT INTO meta(key,value) VALUES('cursor',(SELECT coalesce(max(id),0) FROM events)) ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
exit 0
