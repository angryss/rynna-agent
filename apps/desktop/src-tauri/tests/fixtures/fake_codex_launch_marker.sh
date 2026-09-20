#!/bin/sh
[ "$1" = "app-server" ] || exit 2
printf launched > "$CODEX_HOME/launched"
exit 9
