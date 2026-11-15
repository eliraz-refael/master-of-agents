#!/bin/bash
# THROWAWAY SPIKE: opt-in session tracker, installed in ~/.claude-lead/settings.json.
# Does nothing unless the session was started with MOA_TRACK=1.
[ -n "${MOA_TRACK:-}" ] || exit 0
db="${MOA_DB:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/brain.db}"  # the moa that started it, else the default brain

in=$(cat)
sid=$(jq -r '.session_id // ""' <<<"$in")
cwd=$(jq -r '.cwd // ""' <<<"$in")
q() { printf "'%s'" "${1//\'/\'\'}"; }
# Where an account's sessions go, set in its hook command by `moactl setup`:
# - MOA_ONLY_UNDER (colon-separated folders): report only sessions under those folders and
#   drop the rest, e.g. for an account that mostly works on things that don't belong here.
# - MOA_ACCOUNT_BRAIN (a brain's name): the account's sessions go to that brain's moa,
#   except under MOA_EXCEPT_UNDER (folders), where they stay with the moa that started
#   them, or the default one.
# Checked before any other work. A session already tracked stays where it is if it later
# moves, so it doesn't freeze as "working" or split across two brains.
# (sqlite3 creates a missing file even to read it, hence the -f)
tracked() { [ -f "$1" ] && [ -n "$(sqlite3 "$1" "SELECT 1 FROM sessions WHERE session_id=$(q "$sid")" 2>/dev/null)" ]; }
under() {  # is the session's folder under one of these colon-separated folders?
  local r roots; IFS=: read -ra roots <<<"$1"
  for r in "${roots[@]}"; do
    [ -n "$r" ] || continue   # an empty entry ("a::b", a trailing ":") must not match everything
    case "$cwd/" in "${r%/}"/*) return 0 ;; esac
  done
  return 1
}
if [ -n "${MOA_ACCOUNT_BRAIN:-}" ]; then
  . "$(dirname "${BASH_SOURCE[0]}")/../../moa-proto/brain.sh"
  other=$(moa_brain "$MOA_ACCOUNT_BRAIN"; echo "$MOA_DB")
  if under "${MOA_EXCEPT_UNDER:-}"; then
    if ! tracked "$db" && tracked "$other"; then db="$other"; fi
  elif ! tracked "$db"; then
    db="$other"
  fi
elif [ -n "${MOA_ONLY_UNDER:-}" ]; then
  under "$MOA_ONLY_UNDER" || tracked "$db" || exit 0
fi
[ -f "$db" ] || exit 0
event=$(jq -r '.hook_event_name // empty' <<<"$in")
text=""
case "$event" in
  SessionStart)     kind=start;   text=$(jq -r '.source // ""' <<<"$in") ;;
  UserPromptSubmit) kind=prompt;  full=$(jq -r '.prompt // ""' <<<"$in"); text=$(printf %s "$full" | head -c 500) ;;
  Stop)
    kind=stop
    text=$(jq -r '.last_assistant_message // ""' <<<"$in")
    if [ -z "$text" ]; then
      tp=$(jq -r '.transcript_path // ""' <<<"$in")
      [ -f "$tp" ] && text=$(tail -n 60 "$tp" | jq -rs '[.[] | select(.type=="assistant") | .message.content[]? | select(.type=="text") | .text] | last // ""' 2>/dev/null)
    fi
    full="$text"  # an ask's reply goes back whole (see the asks UPDATE below)
    # the end of a reply is where "what's next" usually is: keep the start and the end
    text=$(jq -Rrs 'if length > 2400 then .[:600] + "\n[…]\n" + .[-1800:] else . end' <<<"$text") ;;
  Notification)     kind=waiting; text=$(jq -r '.message // ""' <<<"$in") ;;
  SessionEnd)       kind=end;     text=$(jq -r '.reason // ""' <<<"$in") ;;
  *) exit 0 ;;
esac

acct=$(basename "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" | sed 's/^\.//')
pane="${TMUX_PANE:-}"; tsock="${TMUX%%,*}"; title="${MOA_TITLE:-}"; grp="${MOA_GROUP:-}"
state=$(case $kind in start|stop) echo idle ;; prompt) echo working ;; waiting) echo waiting ;; end) echo ended ;; esac)

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
# Another agent's message (moactl ask) is received when it's submitted as a prompt, and
# the turn it starts ends with the answer. Tying the answer to the received message means a
# prompt Eliraz typed just before the paste landed can't pass its reply off as the answer.
# Last and on their own: a brain without the asks table yet only skips these.
# (anywhere in the prompt: Claude wraps a paste in <pasted_content> tags)
# The header names its ask, so a paste that lands late (after its ask failed) can't take the
# slot of the next ask to this session.
if [ "$kind" = prompt ] && [[ "$full" == *"[moa · message from"* ]]; then  # the whole prompt: text is cut at 500 bytes
  ask_id=$(printf %s "$full" | sed -n 's/.*\[moa · message from .* · ask #\([0-9][0-9]*\)\].*/\1/p' | head -n 1)
  sqlite3 "$db" "PRAGMA busy_timeout=2000; UPDATE asks SET status='received'
    WHERE id=${ask_id:-(SELECT min(id) FROM asks WHERE to_sid=$(q "$sid") AND status='sent')}
      AND to_sid=$(q "$sid") AND status='sent';" >/dev/null 2>&1
fi
if [ "$kind" = stop ]; then
  # on stdin, not argv: a long reply can pass the per-argument limit (128 KB on Linux)
  sqlite3 "$db" <<SQL >/dev/null 2>&1
PRAGMA busy_timeout=2000;
UPDATE asks SET status='answered', reply=$(q "${full:-}"), answered_at=datetime('now','localtime')
 WHERE id=(SELECT min(id) FROM asks WHERE to_sid=$(q "$sid") AND status='received');
SQL
fi
# Running again, so no longer asleep (moactl sleep). On its own, like the asks: a brain
# without the column only skips it.
[ "$kind" = start ] && sqlite3 "$db" "PRAGMA busy_timeout=2000; UPDATE sessions SET asleep_at=NULL WHERE session_id=$(q "$sid");" >/dev/null 2>&1
# A new session learns it has peers (SessionStart output becomes context), so asking
# another agent needs no setup.
if [ "$kind" = start ]; then
  # the plugin's moactl: like this hook, it sends the session to its account's brain (MOA_ACCOUNT_BRAIN)
  ctl="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../moa-plugin/moa/bin" && pwd)/moactl"
  cat <<EOF
You're running inside moa (master-of-agents), alongside other agent sessions and moa itself, which keeps the big picture.
- \`$ctl peers\` lists the agents you can talk to.
- \`$ctl ask NAME "message"\` sends one a message and waits for its reply (up to 9 minutes, so give that shell command a 600000 ms timeout; \`$ctl reply ID\` waits again).
Use it when the user asks you to talk to another session, agent or moa, or when a peer clearly knows something you need.
Messages from peers arrive as "[moa · message from …]"; your final message that turn is your answer to them.
EOF
fi
exit 0
