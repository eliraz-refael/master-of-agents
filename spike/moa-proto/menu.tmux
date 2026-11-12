# THROWAWAY PROTOTYPE: the moa menu (Ctrl-a Space, or Ctrl-a twice). Sourced by tmux.conf's key bindings;
# $M is the moamenu path, a %hidden variable tmux.conf defines.
display-menu -T "#[align=centre fg=#88c0d0 bold] moa " -x C -y C \
  "Console"                 c "run-shell -b '$M goto console'" \
  "Sessions tree"           t "run-shell -b '$M goto tree'" \
  "Big picture"             p "run-shell -b '$M goto plan'" \
  "Agent spot"              g "run-shell -b '$M goto agent'" \
  "Jump to any session…"    s "choose-tree -Zw" \
  "New session…"            n "command-prompt -p 'folder:,title:' -I '~/private/,' \"run-shell -b \\\"$M open '%1' --title '%2'\\\"\"" \
  "Back to moa"             m "select-window -t moa:moa" \
  "Zoom this pane"          z "resize-pane -Z" \
  "" \
  "Layout: cockpit"         1 "run-shell -b '$M layout apply cockpit'" \
  "Layout: focus (agent)"   2 "run-shell -b '$M layout apply focus'" \
  "Layout: classic"         3 "run-shell -b '$M layout apply classic'" \
  "Repair panels"           r "run-shell -b '$M layout apply'" \
  "" \
  "-Ctrl-a ←↑↓→  move between panes" "" "" \
  "Previous window"         [ "previous-window" \
  "Next window"             ] "next-window" \
  "Keys and help"           ? "display-popup -E -w 84 -h 32 'less -R $HOME/private/master-of-agents/spike/moa-proto/help.txt'" \
  "Send Ctrl-a to the app"  a "send-keys C-a"
