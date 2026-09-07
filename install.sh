#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

if [[ ! -f "$ROOT_DIR/templates/cloze.typ" ]]; then
  echo "templates/cloze.typ が見つかりません: $ROOT_DIR/templates/cloze.typ" >&2
  exit 1
fi

"${CARGO:-cargo}" install --path "$ROOT_DIR" --force

if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
  CONFIG_HOME="$XDG_CONFIG_HOME"
elif [[ -n "${HOME:-}" ]]; then
  CONFIG_HOME="$HOME/.config"
else
  echo "HOME または XDG_CONFIG_HOME が必要です" >&2
  exit 1
fi

FLOWCLOZE_CONFIG_DIR="$CONFIG_HOME/flowcloze"
TEMPLATE_DIR="$FLOWCLOZE_CONFIG_DIR/templates"
CONFIG_PATH="$FLOWCLOZE_CONFIG_DIR/config.toml"

umask 077
mkdir -p "$TEMPLATE_DIR"
chmod 700 "$FLOWCLOZE_CONFIG_DIR" "$TEMPLATE_DIR" 2>/dev/null || true
install -m 0644 "$ROOT_DIR/templates/cloze.typ" "$TEMPLATE_DIR/cloze.typ"
TEMPLATE_PATH="$(cd "$TEMPLATE_DIR" && pwd -P)/cloze.typ"

# Escape the absolute path for a TOML basic string.
TOML_TEMPLATE_PATH="${TEMPLATE_PATH//\\/\\\\}"
TOML_TEMPLATE_PATH="${TOML_TEMPLATE_PATH//\"/\\\"}"
SETTING="typst_template = \"$TOML_TEMPLATE_PATH\""

if [[ -f "$CONFIG_PATH" ]]; then
  TMP="$(mktemp "$FLOWCLOZE_CONFIG_DIR/.config.XXXXXX")"
  replaced=false
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^[[:space:]]*typst_template[[:space:]]*= ]]; then
      printf '%s\n' "$SETTING" >> "$TMP"
      replaced=true
    else
      printf '%s\n' "$line" >> "$TMP"
    fi
  done < "$CONFIG_PATH"
  if [[ "$replaced" == false ]]; then
    printf '\n%s\n' "$SETTING" >> "$TMP"
  fi
  mv "$TMP" "$CONFIG_PATH"
else
  printf '%s\n' "$SETTING" > "$CONFIG_PATH"
fi
chmod 600 "$CONFIG_PATH" 2>/dev/null || true

printf 'FlowCloze installed.\n'
printf '  binary: %s\n' "${CARGO_HOME:-$HOME/.cargo}/bin/flowcloze"
printf '  template: %s\n' "$TEMPLATE_PATH"
printf '  config: %s\n' "$CONFIG_PATH"
