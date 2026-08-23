#!/usr/bin/env bash

FIFO="$HOME/.fx/fxq.fifo"

[[ -p "$FIFO" ]] || mkfifo "$FIFO"

printf '%s\n' "$*" > "$FIFO"

echo "queued"
