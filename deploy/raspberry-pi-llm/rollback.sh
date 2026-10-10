#!/usr/bin/env bash
set -Eeuo pipefail

[[ $EUID -eq 0 ]] || { echo "Запустите скрипт через sudo." >&2; exit 1; }
BACKUP_DIR=${1:-}
case "$BACKUP_DIR" in
    /var/backups/fox-llm-day30/*) ;;
    *) echo "Укажите точный backup из /var/backups/fox-llm-day30/." >&2; exit 2 ;;
esac
[[ -d $BACKUP_DIR ]] || { echo "Backup не найден: $BACKUP_DIR" >&2; exit 1; }

restore_target() {
    local name=$1 target=$2
    if [[ -f $BACKUP_DIR/$name ]]; then
        install -D -m 0644 "$BACKUP_DIR/$name" "$target"
    elif [[ -f $BACKUP_DIR/$name.absent ]]; then
        rm -f -- "$target"
    else
        echo "В backup нет состояния для $name" >&2
        exit 1
    fi
}

restore_target override.conf /etc/systemd/system/ollama.service.d/override.conf
restore_target fox-llm.conf /etc/nginx/conf.d/fox-llm.conf
restore_target Modelfile /etc/fox-llm/Modelfile

systemctl daemon-reload
systemctl restart ollama
if [[ -f /etc/fox-llm/Modelfile ]]; then
    ollama create fox-qwen-pi -f /etc/fox-llm/Modelfile
fi
nginx -t
systemctl reload nginx
echo "Откат из $BACKUP_DIR завершён."
