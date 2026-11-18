#!/bin/sh
calls="${0%/*}/calls"
for argument in "$@"; do printf '%s\n' "$argument" >> "$calls"; done
printf '%s\n' 'END-OF-CALL' >> "$calls"
handle=""; title=""; previous=""
for argument in "$@"; do
  [ "$previous" = "--terminal" ] && handle="$argument"
  [ "$previous" = "--title" ] && title="$argument"
  previous="$argument"
done
case "$1 $2" in
'terminal create')
  case "$title" in
  refused) printf '{"ok":false,"error":{"code":"worktree_not_found","message":"no such worktree"}}\n'; exit 1 ;;
  garbled) printf 'not json\n' ;;
  crashed) printf 'orca: boom\n' >&2; exit 1 ;;
  *) printf '{"ok":true,"result":{"terminal":{"handle":"term-7","tabId":"tab-7","title":"%s"}}}\n' "$title" ;;
  esac ;;
'terminal split')
  printf '{"ok":true,"result":{"split":{"handle":"term-9","tabId":"tab-9"}}}\n' ;;
'terminal wait')
  case "$handle" in
  term-busy) printf '{"ok":false,"error":{"code":"timeout","message":"timeout"}}\n'; exit 1 ;;
  term-unsettled) printf '{"ok":true,"result":{"wait":{"satisfied":false}}}\n' ;;
  term-gone) printf '{"ok":false,"error":{"code":"terminal_handle_stale","message":"stale handle"}}\n'; exit 1 ;;
  *) printf '{"ok":true,"result":{"wait":{"satisfied":true}}}\n' ;;
  esac ;;
'terminal send')
  printf '{"ok":true,"result":{"send":{"accepted":true}}}\n' ;;
'terminal close')
  case "$handle" in
  term-gone) printf '{"ok":false,"error":{"code":"terminal_handle_stale","message":"stale handle"}}\n'; exit 1 ;;
  *) printf '{"ok":true,"result":{"close":{"closed":true}}}\n' ;;
  esac ;;
*)
  printf 'unsupported: %s\n' "$*" >&2; exit 2 ;;
esac
