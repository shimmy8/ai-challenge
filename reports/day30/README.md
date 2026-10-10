# День 30. Локальная LLM как приватный сервис

Решение запускает inference `qwen3:1.7b` на Raspberry Pi 5 (8 ГБ), а MacBook
использует Pi как сетевой HTTP-сервис. Облако в этом маршруте не участвует.

```text
fox-llm на MacBook
  └─ Ollama Remote (ollama_remote)
       └─ HTTP :11435, только разрешённая LAN
            └─ Nginx: CIDR + 30 req/min, burst 5
                 └─ 127.0.0.1:11434
                      └─ Ollama + fox-qwen-pi (qwen3:1.7b, num_ctx=4096)
```

Локальный провайдер `Ollama` не изменён: он по-прежнему использует
`127.0.0.1:11434` и модель `qwen3.5:4b`. Новый `Ollama Remote` имеет собственные
endpoint, модель, session identity и metrics identity `ollama_remote`; API key
ему не нужен.

## Развёртывание

ARM64-конфигурация, Nginx-шаблон, установка и откат находятся в
[`deploy/raspberry-pi-llm`](../../deploy/raspberry-pi-llm/README.md).

На Raspberry Pi:

```bash
cd deploy/raspberry-pi-llm
sudo ./setup.sh --listen-address <PI_FIXED_IP> --lan-cidr <LAN_CIDR>
```

Для заранее импортированной `qwen3:1.7b`, например при недоступности registry
с Pi, используйте ту же команду с `--skip-pull`. Setup проверит наличие модели
локально и не выполнит сетевую загрузку:

```bash
sudo ./setup.sh --listen-address <PI_FIXED_IP> --lan-cidr <LAN_CIDR> --skip-pull
```

При последующих запусках параметры больше не нужны:

```bash
cd deploy/raspberry-pi-llm
sudo ./start.sh
```

В `fox-llm` на MacBook выберите `Ollama Remote`. При первом выборе введите
`http://<PI_FIXED_IP>:11435/v1`; позднее адрес меняется командой `/endpoint`.
Модель deployment alias — `fox-qwen-pi`. Можно выбрать её через `/model`.

## Воспроизводимая проверка

Из корня репозитория на MacBook:

```bash
python3 reports/day30/verify.py \
  --base-url http://<PI_FIXED_IP>:11435 \
  --model fox-qwen-pi
```

Скрипт последовательно проверяет:

1. сетевой запрос к модели;
2. чат из двух связанных реплик;
3. пять коротких запросов без конкурентной нагрузки;
4. `num_ctx=4096` и синтетический запрос около границы контекста;
5. получение `429` и восстановление после контрольного интервала.

Прогресс одновременно выводится в терминал и записывается в
[`verification.log`](verification.log). В логе есть полный встроенный
синтетический prompt и полный ответ модели, но нет base URL, IP, HTTP-заголовков,
credentials или пользовательских сессий.

Backend проверяется отдельно с MacBook:

```bash
curl --fail http://<PI_FIXED_IP>:11435/api/tags
curl --connect-timeout 2 http://<PI_FIXED_IP>:11434/api/tags
```

Первый запрос должен быть успешным, прямой запрос к `11434` — недоступным.

## Результаты

Локально проверены конфигурация провайдера, URL routing, serde/session/metrics
identity, русскоязычные ошибки и формат безопасного текстового лога. Fixture
проверочного сценария проходит командой:

```bash
python3 -m unittest reports/day30/test_verify.py -v
```

Проверки кода перед поставкой:

- `cargo fmt --all -- --check` — пройдено;
- `cargo clippy --all-targets --all-features -- -D warnings` — пройдено;
- `cargo test` — пройдено, 242 теста;
- `python3 -m unittest reports/day30/test_verify.py -v` — пройдено, 1 тест;
- `bash -n deploy/raspberry-pi-llm/setup.sh deploy/raspberry-pi-llm/start.sh deploy/raspberry-pi-llm/rollback.sh` — пройдено.

Фактическая проверка выполнена 10 октября 2026 года с MacBook против Raspberry
Pi 5 и модели `fox-qwen-pi` (`qwen3:1.7b`, `Q4_K_M`):

- LAN gateway вернул HTTP `200`, прямой backend-порт `11434` с MacBook был
  недоступен;
- сетевой smoke-запрос вернул `PI-ONLINE`;
- связанный чат сохранил и восстановил синтетический код `FOX-30`;
- пять последовательных запросов завершились с HTTP `200` без перезапуска;
- проверка прочитала `num_ctx=4096` и успешно обработала синтетический prompt
  около границы контекста;
- gateway вернул HTTP `429`, а после контрольного интервала снова HTTP `200`;
- в записанном видео показана ручная проверка через `fox-llm`: выбран провайдер
  `Ollama Remote`, выполнен многошаговый диалог с моделью `fox-qwen-pi` и
  подтверждено восстановление сессии с identity `ollama_remote`;
- итог: `passed=9 total=9 all_passed=true`.

Полные синтетические prompt, ответы и длительности сохранены в
[`verification.log`](verification.log). Проверка приватности не нашла в нём
base URL, IP-адрес, HTTP-заголовки или credentials.

## Ограничения

- Модель 1.7B подходит для учебного чата, но уступает большим моделям в качестве.
- CPU inference на Pi заметно медленнее desktop/GPU; это не benchmark.
- Доступ ограничен доверенной подсетью, без TLS и API key. Для чужой сети нужен
  VPN или отдельная аутентификация.
- `client_max_body_size=256k` ограничивает байты HTTP-запроса и не является
  токенным лимитом. `num_ctx=4096` проверяется отдельно на уровне модели.
- Проверка стабильности последовательная и короткая; нагрузочного тестирования
  в задании нет.
