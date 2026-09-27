#!/usr/bin/env bash
set -euo pipefail

checker="scripts/check-serial-markers.sh"
markers="$(mktemp)"
serial_log="$(mktemp)"
trap 'rm -f "$markers" "$serial_log"' EXIT

cat > "$markers" <<'EOF'
# комментарий

bootstrap: OK
memory self-test: OK
EOF

cat > "$serial_log" <<'EOF'
[INFO] bootstrap: OK
[INFO] memory self-test: OK
EOF

bash "$checker" "$markers" "$serial_log"

cat > "$serial_log" <<'EOF'
[INFO] bootstrap: OK
EOF

if bash "$checker" "$markers" "$serial_log"; then
  echo "Проверка должна была обнаружить отсутствующий маркер" >&2
  exit 1
fi

echo "Проверка списка маркеров работает корректно"
