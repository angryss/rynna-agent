#!/bin/sh
# Select a scenario through the executable symlink name, without global env changes.
scenario=${0##*/}
if [ "$1" = "--version" ]; then
  case "$scenario" in
    current) printf '%s\n' 'codex-cli 0.155.1' ;;
    future) printf '%s\n' 'codex-cli 99.0.0' ;;
    custom) printf '%s\n' 'Codex development build (custom)' ;;
    *) exit 2 ;;
  esac
  exit 0
fi
[ "$1" = "app-server" ] || exit 2
IFS= read -r initialize
case "$scenario" in
  malformed) printf '%s\n' 'not JSON'; exit 0 ;;
  incompatible) printf '%s\n' '{"id":1,"error":{"code":-32601,"message":"initialize unsupported"}}'; exit 0 ;;
esac
printf '%s\n' '{"id":1,"result":{"userAgent":"fake"}}'
IFS= read -r initialized
IFS= read -r thread
case "$thread" in *'"approvalPolicy":"never"'*) ;; *) exit 3 ;; esac
case "$thread" in *'"sandbox":"read-only"'*) ;; *) exit 4 ;; esac
case "$thread" in *'"features":{"shell_tool":false,"view_image":false}'*) ;; *) exit 5 ;; esac
case "$thread" in *'"update_plan":{"enabled":false}'*) ;; *) exit 6 ;; esac
case "$thread" in *'"web_search":"disabled"'*) ;; *) exit 7 ;; esac
case "$thread" in *'"environments":[]'*) ;; *) exit 8 ;; esac
case "$thread" in *'"ephemeral":true'*) ;; *) exit 9 ;; esac
if [ "$scenario" = missing-thread ]; then
  printf '%s\n' '{"id":2,"result":{"thread":{}}}'
  exit 0
fi
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-1"}}}'
IFS= read -r turn
if [ "$scenario" = missing-turn ]; then
  printf '%s\n' '{"id":3,"result":{"turn":{}}}'
  exit 0
fi
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-1"}}}'
if [ "$scenario" = disabled-tool ]; then
  printf '%s\n' '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"tool-1","type":"commandExecution"}}}'
  exit 0
fi
printf '%s\n' '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"item-1","type":"agentMessage"}}}'
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"Compatible answer"}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}}'
