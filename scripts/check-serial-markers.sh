#!/usr/bin/env bash
set -euo pipefail

MARKERS_FILE="${1:?usage: check-serial-markers.sh <markers-file> <serial-log>}"
SERIAL_LOG="${2:?usage: check-serial-markers.sh <markers-file> <serial-log>}"

if [[ ! -f "$MARKERS_FILE" ]]; then
  echo "Не найден файл ожидаемых маркеров: $MARKERS_FILE" >&2
  exit 2
fi

if [[ ! -f "$SERIAL_LOG" ]]; then
  echo "Не найден журнал последовательного вывода: $SERIAL_LOG" >&2
  exit 2
fi

missing=0

while IFS= read -r marker || [[ -n "$marker" ]]; do
  marker="${marker%$'\r'}"

  if [[ -z "$marker" || "${marker:0:1}" == "#" ]]; then
    continue
  fi

  if grep -Fq -- "$marker" "$SERIAL_LOG"; then
    continue
  fi

  echo "Не найден ожидаемый маркер последовательного вывода: $marker" >&2
  missing=1
done < "$MARKERS_FILE"

exit "$missing"
