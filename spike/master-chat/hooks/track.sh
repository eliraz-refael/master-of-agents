#!/bin/bash
# THROWAWAY SPIKE: opt-in session tracker, installed in ~/.claude-lead/settings.json.
# Does nothing unless the session was started with MOA_TRACK=1.
[ -n "${MOA_TRACK:-}" ] || exit 0
db="${MOA_DB:-/Users/elirazk/private/master-of-agents/spike/master-chat/brain.db}"
[ -f "$db" ] || exit 0

in=$(cat)
event=$(jq -r '.hook_event_name // empty' <<<"$in")
text=""
case "$event" in
  SessionStart)     kind=start;   text=$(jq -r '.source // ""' <<<"$in") ;;
  UserPromptSubmit) kind=prompt;  text=$(jq -r '.prompt // ""' <<<"$in" | head -c 500) ;;
  Stop)
    kind=stop
    text=$(jq -r '.last_assistant_message // ""' <<<"$in")
    if [ -z "$text" ]; then
      tp=$(jq -r '.transcript_path // ""' <<<"$in")
      [ -f "$tp" ] && text=$(tail -n 60 "$tp" | jq -rs '[.[] | select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text] | last // ""' 2>/dev/null)
    fi
    # the end of a reply is where "what's next" usually is: keep the start and the end
    text=$(jq -Rrs 'if length > 2400 then .[:600] + "\n[…]\n" + .[-1800:] else . end' <<<"$text") ;;
  Notification)     kind=waiting; text=$(jq -r '.message // ""' <<<"$in") ;;
  SessionEnd)       kind=end;     text=$(jq -r '.reason // ""' <<<"$in") ;;
  *) exit 0 ;;
esac

sid=$(jq -r '.session_id // ""' <<<"$in")
cwd=$(jq -r '.cwd // ""' <<<"$in")
acct=$(basename "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" | sed 's/^\.//')
pane="${TMUX_PANE:-}"; tsock="${TMUX%%,*}"; title="${MOA_TITLE:-}"; grp="${MOA_GROUP:-}"
state=$(case $kind in start|stop) echo idle ;; prompt) echo working ;; waiting) echo waiting ;; end) echo ended ;; esac)

q() { printf "'%s'" "${1//\'/\'\'}"; }
prompt_col=""; [ "$kind" = prompt ] && prompt_col=", last_prompt=excluded.last_prompt"

sqlite3 "$db" <<SQL >/dev/null 2>&1
PRAGMA busy_timeout=2000;
INSERT INTO events(session_id,account,cwd,kind,text) VALUES($(q "$sid"),$(q "$acct"),$(q "$cwd"),'$kind',$(q "$text"));
INSERT INTO sessions(session_id,account,cwd,project_id,state,last_prompt,first_seen,last_seen,pane,tmux,title,grp)
VALUES($(q "$sid"),$(q "$acct"),$(q "$cwd"),
       (SELECT id FROM projects WHERE $(q "$cwd") = path OR $(q "$cwd") LIKE path || '/%' OR $(q "$cwd") LIKE path || '-worktrees/%' ORDER BY length(path) DESC LIMIT 1),
       '$state', $( [ "$kind" = prompt ] && q "$text" || echo NULL ), datetime('now','localtime'), datetime('now','localtime'), $(q "$pane"), $(q "$tsock"), nullif($(q "$title"),''), nullif($(q "$grp"),''))
ON CONFLICT(session_id) DO UPDATE SET state=excluded.state, last_seen=excluded.last_seen, cwd=excluded.cwd,
  pane=coalesce(nullif(excluded.pane,''),pane), tmux=coalesce(nullif(excluded.tmux,''),tmux),
  title=coalesce(title,excluded.title), grp=coalesce(grp,excluded.grp)$prompt_col;
UPDATE projects SET last_activity=date('now','localtime')
 WHERE id=(SELECT project_id FROM sessions WHERE session_id=$(q "$sid"));
SQL
exit 0
