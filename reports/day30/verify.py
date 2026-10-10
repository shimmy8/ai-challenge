#!/usr/bin/env python3
"""Проверка приватного Ollama-сервиса без записи адреса и заголовков в лог."""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class HttpResult:
    status: int
    body: str
    elapsed_ms: int


class CheckLog:
    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        self.file = path.open("w", encoding="utf-8")

    def close(self) -> None:
        self.file.close()

    def write(self, event: str, text: str = "") -> None:
        line = f"[day30-check][{event}]"
        if text:
            line += f" {text}"
        print(line, flush=True)
        self.file.write(line + "\n")
        self.file.flush()

    def block(self, event: str, text: str) -> None:
        self.write(event)
        for line in text.splitlines() or [""]:
            print(line, flush=True)
            self.file.write(line + "\n")
        self.file.flush()


def validated_base_url(value: str) -> str:
    parsed = urllib.parse.urlsplit(value.strip())
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise argparse.ArgumentTypeError("base URL должен быть корректным HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise argparse.ArgumentTypeError(
            "base URL не должен содержать credentials, query или fragment"
        )
    return urllib.parse.urlunsplit(
        (parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", "")
    )


def request(base_url: str, path: str, payload: dict[str, Any] | None, timeout: int) -> HttpResult:
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        base_url + path,
        data=data,
        method="GET" if payload is None else "POST",
        headers={"Content-Type": "application/json"} if payload is not None else {},
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            body = response.read().decode("utf-8", errors="replace")
            return HttpResult(response.status, body, int((time.monotonic() - started) * 1000))
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        return HttpResult(error.code, body, int((time.monotonic() - started) * 1000))


def safe_error(error: BaseException) -> str:
    if isinstance(error, TimeoutError):
        return "request_timeout"
    if isinstance(error, urllib.error.URLError):
        return "connection_failed"
    if isinstance(error, (json.JSONDecodeError, KeyError, TypeError, ValueError)):
        return "invalid_response"
    return "check_failed"


def response_text(result: HttpResult) -> str:
    value = json.loads(result.body)
    text = value["message"]["content"]
    if not isinstance(text, str) or not text.strip():
        raise ValueError("empty model response")
    return text


def finish(log: CheckLog, case: str, result: HttpResult, passed: bool, error: str = "-") -> bool:
    log.write(
        "case_done",
        f"case={case} http_status={result.status} elapsed_ms={result.elapsed_ms} "
        f"passed={str(passed).lower()} error={error}",
    )
    return passed


def chat_payload(model: str, messages: list[dict[str, str]]) -> dict[str, Any]:
    return {
        "model": model,
        "messages": messages,
        "stream": False,
        "think": False,
        "options": {"num_ctx": 4096, "num_predict": 64},
        "keep_alive": "30m",
    }


def run_chat_case(
    log: CheckLog,
    base_url: str,
    model: str,
    timeout: int,
    case: str,
    messages: list[dict[str, str]],
) -> tuple[bool, str]:
    prompt = messages[-1]["content"]
    log.write("case_start", f"case={case}")
    log.block("prompt", prompt)
    try:
        result = request(base_url, "/api/chat", chat_payload(model, messages), timeout)
        answer = response_text(result)
        log.block("response", answer)
        return finish(log, case, result, result.status == 200), answer
    except Exception as error:  # ошибка переводится в безопасную категорию
        result = HttpResult(0, "", 0)
        log.block("response", "-")
        return finish(log, case, result, False, safe_error(error)), ""


def run_checks(args: argparse.Namespace) -> bool:
    log = CheckLog(args.log)
    passed: list[bool] = []
    try:
        ok, _ = run_chat_case(
            log,
            args.base_url,
            args.model,
            args.timeout,
            "network_smoke",
            [{"role": "user", "content": "Ответь ровно: PI-ONLINE /no_think"}],
        )
        passed.append(ok)

        case = "multi_turn_chat"
        log.write("case_start", f"case={case}")
        first_prompt = "Запомни синтетический код FOX-30 и ответь: ЗАПОМНИЛ. /no_think"
        log.block("prompt", first_prompt)
        started = time.monotonic()
        try:
            first = request(
                args.base_url,
                "/api/chat",
                chat_payload(args.model, [{"role": "user", "content": first_prompt}]),
                args.timeout,
            )
            first_answer = response_text(first)
            log.block("response", first_answer)
            second_prompt = "Какой синтетический код я просил запомнить? /no_think"
            log.block("prompt", second_prompt)
            second = request(
                args.base_url,
                "/api/chat",
                chat_payload(
                    args.model,
                    [
                        {"role": "user", "content": first_prompt},
                        {"role": "assistant", "content": first_answer},
                        {"role": "user", "content": second_prompt},
                    ],
                ),
                args.timeout,
            )
            second_answer = response_text(second)
            log.block("response", second_answer)
            combined = HttpResult(second.status, second.body, int((time.monotonic() - started) * 1000))
            passed.append(finish(log, case, combined, second.status == 200 and "FOX-30" in second_answer.upper()))
        except Exception as error:
            failed = HttpResult(0, "", int((time.monotonic() - started) * 1000))
            log.block("response", "-")
            passed.append(finish(log, case, failed, False, safe_error(error)))

        for index in range(1, args.sequential_count + 1):
            ok, _ = run_chat_case(
                log,
                args.base_url,
                args.model,
                args.timeout,
                f"sequential_{index}",
                [{"role": "user", "content": f"Ответь ровно: SEQ-{index}-OK /no_think"}],
            )
            passed.append(ok)

        case = "context_4096"
        log.write("case_start", f"case={case}")
        started = time.monotonic()
        boundary_prompt = (
            "Это синтетическая проверка границы контекста. "
            + "fox " * args.context_words
            + "Ответь одним словом: CONTEXT-OK /no_think"
        )
        log.block("prompt", boundary_prompt)
        try:
            show = request(args.base_url, "/api/show", {"model": args.model}, args.timeout)
            parameters = str(json.loads(show.body).get("parameters", ""))
            context_configured = "num_ctx" in parameters and "4096" in parameters
            context_result = request(
                args.base_url,
                "/api/chat",
                chat_payload(args.model, [{"role": "user", "content": boundary_prompt}]),
                args.timeout,
            )
            answer = response_text(context_result)
            log.block("response", f"configured_num_ctx=4096\n{answer}")
            combined = HttpResult(
                context_result.status,
                context_result.body,
                int((time.monotonic() - started) * 1000),
            )
            passed.append(finish(log, case, combined, context_configured and context_result.status == 200))
        except Exception as error:
            failed = HttpResult(0, "", int((time.monotonic() - started) * 1000))
            log.block("response", "-")
            passed.append(finish(log, case, failed, False, safe_error(error)))

        case = "rate_limit"
        log.write("case_start", f"case={case}")
        started = time.monotonic()
        limited = False
        last = HttpResult(0, "", 0)
        try:
            for attempt in range(1, args.rate_attempts + 1):
                log.block("prompt", f"GET /api/tags attempt={attempt}")
                last = request(args.base_url, "/api/tags", None, args.timeout)
                log.block("response", last.body)
                log.write(
                    "rate_attempt",
                    f"attempt={attempt} http_status={last.status} elapsed_ms={last.elapsed_ms} "
                    f"expected_rejection={str(last.status == 429).lower()}",
                )
                if last.status == 429:
                    limited = True
                    break
            time.sleep(args.rate_window_seconds)
            log.block("prompt", "GET /api/tags after_control_interval")
            recovered = request(args.base_url, "/api/tags", None, args.timeout)
            log.block("response", recovered.body)
            combined = HttpResult(
                recovered.status,
                recovered.body,
                int((time.monotonic() - started) * 1000),
            )
            passed.append(finish(log, case, combined, limited and recovered.status == 200))
        except Exception as error:
            failed = HttpResult(last.status, "", int((time.monotonic() - started) * 1000))
            log.block("response", "-")
            passed.append(finish(log, case, failed, False, safe_error(error)))

        log.write("summary", f"passed={sum(passed)} total={len(passed)} all_passed={str(all(passed)).lower()}")
        return all(passed)
    finally:
        log.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True, type=validated_base_url)
    parser.add_argument("--model", default="fox-qwen-pi")
    parser.add_argument("--log", type=Path, default=Path("reports/day30/verification.log"))
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--sequential-count", type=int, default=5, choices=range(5, 11))
    parser.add_argument("--rate-attempts", type=int, default=12)
    parser.add_argument("--rate-window-seconds", type=float, default=3.0)
    parser.add_argument("--context-words", type=int, default=3800)
    return parser.parse_args()


if __name__ == "__main__":
    sys.exit(0 if run_checks(parse_args()) else 1)
