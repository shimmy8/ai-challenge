#!/usr/bin/env bash
set -Eeuo pipefail

[[ $EUID -eq 0 ]] || { echo "Запустите скрипт через sudo." >&2; exit 1; }

for target in \
    /etc/systemd/system/ollama.service.d/override.conf \
    /etc/nginx/conf.d/fox-llm.conf \
    /etc/fox-llm/Modelfile; do
    [[ -f $target ]] || {
        echo "Сервис ещё не настроен: отсутствует $target. Сначала запустите setup.sh." >&2
        exit 1
    }
done
for command in systemctl nginx curl ollama; do
    command -v "$command" >/dev/null || { echo "Не найдена команда: $command" >&2; exit 1; }
done

echo "[start] Перезапускаю Ollama..."
systemctl restart ollama
for _ in {1..30}; do
    curl --silent --fail --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null && break
    sleep 1
done
curl --silent --fail --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null
ollama show fox-qwen-pi >/dev/null

echo "[start] Проверяю конфигурацию и перезапускаю Nginx..."
nginx -t
systemctl restart nginx

echo "Приватный LLM-сервис запущен."
