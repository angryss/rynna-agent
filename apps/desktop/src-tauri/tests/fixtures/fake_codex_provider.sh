#!/bin/sh
[ "$1" = "--version" ] && { printf '%s\n' 'codex-cli 0.155.1'; exit 0; }
[ "$1" = "app-server" ] || exit 2
[ "${CODEX_HOME##*/}" = "rynna-codex" ] || exit 4
IFS= read -r initialize
case "$initialize" in *'"experimentalApi":true'*) ;; *) exit 4 ;; esac
printf '%s\n' '{"id":1,"result":{"userAgent":"fake"}}'
IFS= read -r initialized
IFS= read -r thread
case "$thread" in *'"sandbox":"read-only"'*) ;; *) exit 5 ;; esac
case "$thread" in *'"features":{"shell_tool":false,"view_image":false}'*) ;; *) exit 6 ;; esac
case "$thread" in *'"web_search":"disabled"'*) ;; *) exit 7 ;; esac
case "$thread" in *'"environments":[]'*) ;; *) exit 8 ;; esac
case "$thread" in *'"update_plan":{"enabled":false}'*) ;; *) exit 9 ;; esac
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-1"}}}'
IFS= read -r turn
case "$turn" in *'"effort":"high"'*) ;; *) exit 10 ;; esac
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}'
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"other-thread","turnId":"other-turn","itemId":"item-9","delta":"forged"}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"other-thread","turn":{"id":"other-turn","status":"completed","items":[]}}}'
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-forged","delta":"forged"}}'
printf '%s\n' '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","startedAtMs":1,"item":{"id":"item-1","type":"agentMessage","text":""}}}'
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"Subscription answer"}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed","items":[]}}}'
