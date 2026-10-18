"""Recording mock server for traffic parity. Standard library only.

Control endpoints (used by SDK driver programs):
  POST /__scenario  {"id": str, "responses": [{"status": int, "headers": {}, "body": any}]}
  POST /__outcome   {"id": str, "outcome": "ok" | "error"}
Every other request is recorded under the current scenario and answered with the next scripted response.
State is written to /rec/log.json after every change.
"""
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs

LOCK = threading.Lock()
REQUESTS = []
OUTCOMES = {}
STATE = {"scenario": None, "queue": []}
KEEP = {"authorization", "x-api-key"}


def dump():
    tmp = "/rec/log.json.tmp"
    with open(tmp, "w") as f:
        json.dump({"requests": REQUESTS, "outcomes": OUTCOMES}, f)
    os.replace(tmp, "/rec/log.json")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def send_reply(self, status, headers, body):
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def handle_any(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b""
        path, _, qs = self.path.partition("?")

        if path in ("/__scenario", "/__outcome"):
            data = json.loads(raw or b"{}")
            with LOCK:
                if path == "/__scenario":
                    STATE["scenario"] = data.get("id")
                    STATE["queue"] = list(data.get("responses", []))
                else:
                    OUTCOMES[data.get("id")] = data.get("outcome")
                dump()
            return self.send_reply(204, {}, b"")

        body = None
        if raw:
            try:
                body = json.loads(raw)
            except ValueError:
                body = raw.decode("utf-8", "replace")
        headers = {k.lower(): v for k, v in self.headers.items() if k.lower() in KEEP}
        if raw and self.headers.get("content-type"):
            headers["content-type"] = self.headers["content-type"].split(";")[0].strip().lower()

        with LOCK:
            reply = STATE["queue"].pop(0) if STATE["queue"] else {"status": 500, "body": {"error": "no scripted response"}}
            REQUESTS.append({
                "scenario": STATE["scenario"],
                "method": self.command,
                "path": path,
                "query": {k: sorted(v) for k, v in sorted(parse_qs(qs, keep_blank_values=True).items())},
                "body": body,
                "headers": headers,
            })
            dump()

        payload = reply.get("body")
        out_headers = dict(reply.get("headers") or {})
        if isinstance(payload, str):
            data = payload.encode()
        elif payload is None:
            data = b""
        else:
            data = json.dumps(payload).encode()
            out_headers.setdefault("Content-Type", "application/json")
        self.send_reply(int(reply.get("status", 200)), out_headers, data)

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = do_HEAD = do_OPTIONS = handle_any


if __name__ == "__main__":
    server = ThreadingHTTPServer(("0.0.0.0", 8080), Handler)
    dump()
    open("/rec/ready", "w").close()
    server.serve_forever()
