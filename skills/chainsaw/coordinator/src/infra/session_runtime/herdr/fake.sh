#!/bin/sh
calls="${0%/*}/calls"
for argument in "$@"; do printf '%s\n' "$argument" >> "$calls"; done
printf '%s\n' 'END-OF-CALL' >> "$calls"
case "$1 $2" in
'tab create')
  printf '{"result":{"root_pane":{"pane_id":"pane-7"},"tab":{"tab_id":"tab-7"}}}\n' ;;
'pane split')
  printf '{"result":{"pane":{"pane_id":"pane-9"}}}\n' ;;
'agent start')
  case "$3" in
  late-id|no-id) printf '{"result":{"agent":{"status":"starting"}}}\n' ;;
  *) printf '{"result":{"agent":{"agent_session":{"value":"sess-1"},"status":"idle"}}}\n' ;;
  esac ;;
'agent get')
  if [ "$3" = missing ]; then printf 'no such agent\n' >&2; exit 1; fi
  if [ "$3" = malformed ]; then printf 'not json\n'; exit 0; fi
  if [ "$3" = no-id ]; then printf '{"result":{"agent":{"status":"starting"}}}\n'; exit 0; fi
  if [ "$3" = late-id ]; then
    # The name appears once for `tab create --label`, once for `agent start`, and
    # once per `agent get`; report the id on the third get.
    if [ "$(grep -c '^late-id$' "$calls")" -lt 5 ]; then
      printf '{"result":{"agent":{"status":"starting"}}}\n'; exit 0
    fi
    printf '{"result":{"agent":{"agent_session":{"value":"abc"},"status":"idle"}}}\n'; exit 0
  fi
  printf '{"result":{"agent":{"agent_session":{"value":"sess-1"},"status":"working"}}}\n' ;;
'agent prompt')
  printf '{"result":{"delivered":true}}\n' ;;
'agent send-keys')
  printf '{"result":{"delivered":true}}\n' ;;
'agent wait')
  printf '{"result":{"status":"idle"}}\n' ;;
'pane close')
  if [ "$3" = pane-stuck ]; then printf 'no such pane\n' >&2; exit 1; fi
  printf '{"result":{"closed":true}}\n' ;;
*)
  printf 'unsupported: %s\n' "$*" >&2; exit 2 ;;
esac
