#!/usr/bin/env python3
"""Opt-in official-runtime probe. Disposable home, synthetic keys, loopback only.

Run with python3 scripts/test-codex-provider-runtime.py. Requires `codex` on PATH.
No model-generated tools, live login, user config, or cloud inference are used.
"""
import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading


class Endpoint(http.server.BaseHTTPRequestHandler):
    requests = []
    revoked = False

    def log_message(self, *_args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.requests.append((self.path, self.headers.get("Authorization"), body["model"]))
        if self.revoked and self.path.startswith("/a/"):
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        response = {"id": "resp_fixture", "object": "response", "status": "completed", "output": [],
                    "usage": {"input_tokens": 1, "output_tokens": 0, "total_tokens": 1}}
        data = ("data: " + json.dumps({"type": "response.completed", "response": response}) + "\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class RPC:
    def __init__(self, executable, home):
        self.process = subprocess.Popen(
            [executable, "app-server"], cwd=home, text=True,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env={"HOME": str(home), "CODEX_HOME": str(home / "codex"),
                 "PATH": os.environ["PATH"], "TMPDIR": str(home), "RUST_LOG": "off"},
        )
        self.messages = queue.Queue()
        self.sequence = 0
        def read():
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        threading.Thread(target=read, daemon=True).start()
        self.call("initialize", {"clientInfo": {"name": "switchboard_fixture", "version": "1"},
                                 "capabilities": {"experimentalApi": True}})

    def call(self, method, params):
        self.sequence += 1
        self.process.stdin.write(json.dumps({"id": self.sequence, "method": method, "params": params}) + "\n")
        self.process.stdin.flush()
        while True:
            message = self.messages.get(timeout=20)
            if message.get("id") == self.sequence:
                if "error" in message:
                    raise RuntimeError(message["error"])
                return message["result"]

    def turn(self, thread):
        self.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "Synthetic fixture."}]})
        while True:
            message = self.messages.get(timeout=20)
            if message.get("method") == "turn/completed":
                return message["params"]["turn"]["status"]

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def main():
    executable = shutil.which("codex")
    if not executable:
        raise SystemExit("Install the official Codex CLI before running this opt-in probe.")
    print(subprocess.check_output([executable, "--version"], text=True).strip())
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="switchboard-runtime-") as tmp:
            home = Path(tmp)
            codex = home / "codex"
            codex.mkdir()
            ids = {name: "switchboard_" + digit * 32 for name, digit in [("a", "a"), ("b", "b")]}
            ids["openai"] = "openai"
            keys = {name: "SWITCHBOARD_KEY_" + digit * 32 for name, digit in [("a", "A"), ("b", "B")]}
            keys["openai"] = "FIXTURE_SUBSCRIPTION"
            def config(default, missing=False):
                # Subscription is the built-in default; an inert loopback URL catches fallback. OAuth is not tested.
                text = f'model_provider="{ids[default]}"\nmodel="fixture-{default}"\nweb_search="disabled"\ncli_auth_credentials_store="file"\n[features]\nremote_models=false\n'
                for name in ("a", "b"):
                    path = "/openai/v1" if name == "b" else ""
                    text += (f'[model_providers.{ids[name]}]\nname="fixture-{name}"\n'
                             f'base_url="http://127.0.0.1:{server.server_port}/{name}{path}"\n'
                             f'env_key="{keys[name]}"\nwire_api="responses"\nrequires_openai_auth=false\n'
                             'request_max_retries=0\nstream_max_retries=0\n')
                (codex / "config.toml").write_text(text)
                (codex / ".env").write_text(f"OPENAI_BASE_URL=http://127.0.0.1:{server.server_port}/fallback\nOPENAI_API_KEY=synthetic-trap\n" + "".join(f'{keys[n]}=synthetic-{n}\n' for n in ids if not (missing and n == "a")))
            def run(default, operation, missing=False):
                config(default, missing)
                rpc = RPC(executable, home)
                try:
                    return operation(rpc)
                finally:
                    rpc.close()
            def create(rpc):
                started = rpc.call("thread/start", {"cwd": tmp, "approvalPolicy": "never", "sandbox": "read-only"})
                thread = started["thread"]["id"]
                assert rpc.turn(thread) == "completed"
                return thread
            a = run("a", create)
            def resume(thread, provider, expected="completed"):
                def operation(rpc):
                    result = rpc.call("thread/resume", {"threadId": thread})
                    assert result["modelProvider"] == ids[provider], result["modelProvider"]
                    assert result["model"] == f"fixture-{provider}", result["model"]
                    assert rpc.turn(thread) == expected
                return operation
            run("openai", resume(a, "a"))
            b = run("b", create)
            run("b", resume(a, "a"))
            run("openai", resume(b, "b"))
            expected_a = ("/a/responses", "Bearer synthetic-a", "fixture-a")
            expected_b = ("/b/openai/v1/responses", "Bearer synthetic-b", "fixture-b")
            assert Endpoint.requests == [expected_a, expected_a, expected_b, expected_a, expected_b], Endpoint.requests
            Endpoint.revoked = True
            run("openai", resume(a, "a", "failed"))
            assert Endpoint.requests[-1] == expected_a
            before = len(Endpoint.requests)
            try:
                run("openai", resume(a, "a", "failed"), missing=True)
            except RuntimeError as error:
                assert "environment variable" in str(error).lower() or "env" in str(error).lower(), error
            assert len(Endpoint.requests) == before, "Missing A credential must never fall back to another account"
            print("PASS: implicit saved-provider resume, separate keys, Azure-shaped URL, revoked/missing key without fallback")
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
