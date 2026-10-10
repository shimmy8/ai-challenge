#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
    echo "Использование: sudo $0 --listen-address <фиксированный IPv4 Pi> --lan-cidr <CIDR подсети> [--skip-pull]" >&2
}

LISTEN_ADDRESS=""
LAN_CIDR=""
SKIP_PULL=false
while (($#)); do
    case "$1" in
        --listen-address)
            LISTEN_ADDRESS="${2:-}"
            shift 2
            ;;
        --lan-cidr)
            LAN_CIDR="${2:-}"
            shift 2
            ;;
        --skip-pull)
            SKIP_PULL=true
            shift
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

[[ $EUID -eq 0 ]] || { echo "Запустите скрипт через sudo." >&2; exit 1; }
[[ $(uname -m) == "aarch64" || $(uname -m) == "arm64" ]] || {
    echo "Этот setup предназначен для ARM64 Raspberry Pi." >&2
    exit 1
}
[[ $LISTEN_ADDRESS =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || { usage; exit 2; }
[[ $LAN_CIDR =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}/[0-9]{1,2}$ ]] || { usage; exit 2; }
command -v apt-get >/dev/null || { echo "Нужна Raspberry Pi OS или Debian с apt-get." >&2; exit 1; }

echo "[setup] Устанавливаю системные зависимости..."
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl nginx

if ! command -v ollama >/dev/null; then
    echo "[setup] Устанавливаю Ollama официальным ARM64-инсталлятором..."
    OLLAMA_INSTALLER=$(mktemp)
    cleanup_installer() {
        rm -f -- "$OLLAMA_INSTALLER"
    }
    trap cleanup_installer EXIT
    curl --fail --silent --show-error --location \
        https://ollama.com/install.sh \
        --output "$OLLAMA_INSTALLER"
    sh "$OLLAMA_INSTALLER"
    cleanup_installer
    trap - EXIT
fi

for command in ollama nginx systemctl curl sed install ip grep; do
    command -v "$command" >/dev/null || { echo "Не найдена команда: $command" >&2; exit 1; }
done
ip -brief address | grep -Fq "$LISTEN_ADDRESS/" || {
    echo "Адрес $LISTEN_ADDRESS не назначен этой Raspberry Pi." >&2
    exit 1
}

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
BACKUP_DIR="/var/backups/fox-llm-day30/$STAMP"
OVERRIDE_TARGET="/etc/systemd/system/ollama.service.d/override.conf"
NGINX_TARGET="/etc/nginx/conf.d/fox-llm.conf"
MODELFILE_TARGET="/etc/fox-llm/Modelfile"
mkdir -p "$BACKUP_DIR"

backup_target() {
    local target=$1 name=$2
    if [[ -f $target ]]; then
        cp --preserve=mode,timestamps "$target" "$BACKUP_DIR/$name"
    else
        touch "$BACKUP_DIR/$name.absent"
    fi
}

backup_target "$OVERRIDE_TARGET" override.conf
backup_target "$NGINX_TARGET" fox-llm.conf
backup_target "$MODELFILE_TARGET" Modelfile

rollback_on_error() {
    echo "Ошибка setup. Автоматический откат конфигурации из $BACKUP_DIR" >&2
    "$SCRIPT_DIR/rollback.sh" "$BACKUP_DIR" || true
}
trap rollback_on_error ERR

echo "[setup] Устанавливаю конфигурацию Ollama и Nginx..."
install -D -m 0644 "$SCRIPT_DIR/ollama.service.d/override.conf" "$OVERRIDE_TARGET"
install -D -m 0644 "$SCRIPT_DIR/Modelfile" "$MODELFILE_TARGET"
sed \
    -e "s/__PI_LISTEN_ADDRESS__/$LISTEN_ADDRESS/g" \
    -e "s|__LAN_CIDR__|$LAN_CIDR|g" \
    "$SCRIPT_DIR/nginx/fox-llm.conf.template" >"$BACKUP_DIR/fox-llm.conf.new"
install -D -m 0644 "$BACKUP_DIR/fox-llm.conf.new" "$NGINX_TARGET"

systemctl daemon-reload
systemctl enable ollama nginx
systemctl restart ollama
for _ in {1..30}; do
    curl --silent --fail --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null && break
    sleep 1
done
curl --silent --fail --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null

if [[ $SKIP_PULL == true ]]; then
    echo "[setup] Пропускаю загрузку модели и проверяю локальную qwen3:1.7b..."
    ollama show qwen3:1.7b >/dev/null || {
        echo "Модель qwen3:1.7b не найдена. Импортируйте её или запустите setup без --skip-pull." >&2
        exit 1
    }
else
    echo "[setup] Загружаю модель qwen3:1.7b..."
    ollama pull qwen3:1.7b
fi
echo "[setup] Создаю alias fox-qwen-pi..."
ollama create fox-qwen-pi -f "$MODELFILE_TARGET"

nginx -t
systemctl restart nginx
trap - ERR

echo "Setup завершён: gateway слушает $LISTEN_ADDRESS:11435, Ollama остаётся на loopback."
echo "Для последующих запусков: sudo $SCRIPT_DIR/start.sh"
echo "Backup для отката: $BACKUP_DIR"
