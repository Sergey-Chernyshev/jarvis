#!/bin/sh
set -eu

register_body=$(printf '{"protocolVersion":%s,"pid":%s}' "$JARVIS_PLUGIN_PROTOCOL" "$$")

curl --silent --show-error --fail \
  --unix-socket "$JARVIS_SOCKET" \
  --header "x-jarvis-token: $JARVIS_PLUGIN_TOKEN" \
  --header "content-type: application/json" \
  --data "$register_body" \
  http://localhost/plugin/register

if [ "${JARVIS_FAKE_ONESHOT:-0}" = "1" ]; then
  exit 0
fi

after=0
while :; do
  response=$(curl --silent --show-error --fail \
    --unix-socket "$JARVIS_SOCKET" \
    --header "x-jarvis-token: $JARVIS_PLUGIN_TOKEN" \
    "http://localhost/plugin/events?after=$after&limit=64&waitMs=25000")
  next=$(printf '%s' "$response" | sed -n 's/.*"nextSeq":\([0-9][0-9]*\).*/\1/p')
  if [ -n "$next" ]; then
    after=$next
  fi
done
