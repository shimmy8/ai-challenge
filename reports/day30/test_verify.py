#!/usr/bin/env python3

import json
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class FixtureHandler(BaseHTTPRequestHandler):
    tags_calls = 0

    def log_message(self, _format, *args):
        pass

    def send_json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path != "/api/tags":
            self.send_json(404, {"error": "not found"})
            return
        type(self).tags_calls += 1
        if type(self).tags_calls == 4:
            self.send_json(429, {"error": "rate limited"})
        else:
            self.send_json(200, {"models": [{"name": "fox-qwen-pi"}]})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length))
        if self.path == "/api/show":
            self.send_json(200, {"parameters": "num_ctx 4096"})
            return
        if self.path != "/api/chat":
            self.send_json(404, {"error": "not found"})
            return
        if payload.get("think") is not False:
            self.send_json(400, {"error": "thinking must be disabled"})
            return
        prompt = payload["messages"][-1]["content"]
        if "Какой синтетический код" in prompt:
            answer = "FOX-30"
        elif "SEQ-" in prompt:
            answer = prompt.split()[2]
        elif "CONTEXT-OK" in prompt:
            answer = "CONTEXT-OK"
        else:
            answer = "PI-ONLINE" if "PI-ONLINE" in prompt else "ЗАПОМНИЛ"
        self.send_json(200, {"message": {"role": "assistant", "content": answer}})


class VerifyScriptTest(unittest.TestCase):
    def test_human_log_contains_full_synthetic_exchange_without_endpoint(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), FixtureHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                log = Path(directory) / "verification.log"
                base_url = f"http://127.0.0.1:{server.server_port}"
                result = subprocess.run(
                    [
                        sys.executable,
                        str(Path(__file__).with_name("verify.py")),
                        "--base-url",
                        base_url,
                        "--log",
                        str(log),
                        "--context-words",
                        "32",
                        "--rate-window-seconds",
                        "0.01",
                    ],
                    check=False,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                text = log.read_text()
                self.assertIn("[day30-check][case_start]", text)
                self.assertIn("[day30-check][prompt]", text)
                self.assertIn("Ответь ровно: PI-ONLINE", text)
                self.assertIn("[day30-check][response]\nPI-ONLINE", text)
                self.assertIn("http_status=200", text)
                self.assertIn("elapsed_ms=", text)
                self.assertIn("passed=true error=-", text)
                self.assertIn("rate limited", text)
                self.assertNotIn(base_url, text)
                self.assertNotIn("Authorization", text)
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
