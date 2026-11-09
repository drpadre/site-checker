#!/usr/bin/env bash
# Обновление site-checker до последнего релиза GitHub.
# Использование: update.sh [--force]
set -euo pipefail

REPO="drpadre/site-checker"
DIR="/opt/site-checker"
SERVICE="site-checker"
ASSET="site-checker-linux-x86_64"

command -v curl >/dev/null || { echo "нужен curl"; exit 1; }
command -v jq >/dev/null || { echo "нужен jq: apt-get install -y jq"; exit 1; }

json="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")"
tag="$(jq -r .tag_name <<<"$json")"
bin_url="$(jq -r --arg n "$ASSET" '.assets[] | select(.name==$n) | .browser_download_url' <<<"$json")"
sums_url="$(jq -r '.assets[] | select(.name=="SHA256SUMS") | .browser_download_url' <<<"$json")"
[[ -n "$bin_url" && -n "$sums_url" ]] || { echo "в релизе $tag нет бинаря или SHA256SUMS"; exit 1; }

current="$(cat "$DIR/.version" 2>/dev/null || echo none)"
if [[ "$current" == "$tag" && "${1:-}" != "--force" ]]; then
    echo "Уже установлена версия $tag"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Скачиваю $tag..."
curl -fsSL -o "$tmp/$ASSET" "$bin_url"
curl -fsSL -o "$tmp/SHA256SUMS" "$sums_url"
(cd "$tmp" && sha256sum -c SHA256SUMS)

cp -a "$DIR/$SERVICE" "$tmp/previous"
systemctl stop "$SERVICE"
install -m 755 -o sitechecker -g sitechecker "$tmp/$ASSET" "$DIR/$SERVICE"
printf '%s\n' "$tag" > "$DIR/.version"
chown sitechecker:sitechecker "$DIR/.version"

systemctl start "$SERVICE"
sleep 3
if ! systemctl is-active --quiet "$SERVICE"; then
    echo "Сервис не запустился — откат на предыдущую версию"
    journalctl -u "$SERVICE" -n 20 --no-pager
    install -m 755 -o sitechecker -g sitechecker "$tmp/previous" "$DIR/$SERVICE"
    printf '%s\n' "$current" > "$DIR/.version"
    chown sitechecker:sitechecker "$DIR/.version"
    systemctl start "$SERVICE"
    exit 1
fi

echo "Обновлено до $tag"
journalctl -u "$SERVICE" -n 5 --no-pager