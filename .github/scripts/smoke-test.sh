#!/usr/bin/env bash
set -euo pipefail

SERVER="$1"

IMG_DIR=$(mktemp -d)
echo 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==' | base64 -d > "$IMG_DIR/1.png"
echo 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==' | base64 -d > "$IMG_DIR/2.png"

PORT=9877
DB=regan-ci.db
rm -f "$DB"

"$SERVER" --port "$PORT" --image-dir "$IMG_DIR" --output "sqlite://$DB" &
SERVER_PID=$!

for i in $(seq 1 30); do
  if curl -sfo /dev/null "http://localhost:$PORT/" 2>/dev/null; then
    echo "Server ready after ${i}s"
    break
  fi
  sleep 1
done

echo "=== GET / ==="
curl -sSf "http://localhost:$PORT/" > /dev/null
echo "OK"

echo "=== GET /style.css ==="
curl -sSf "http://localhost:$PORT/style.css" > /dev/null
echo "OK"

echo "=== GET /matches ==="
curl -sSf "http://localhost:$PORT/matches" > /dev/null
echo "OK"

kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
rm -f "$DB"
rm -rf "$IMG_DIR"
