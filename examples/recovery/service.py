"""Noncritical demo service. An injected unhealthy state clears only on restart."""
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

failed = False
booted = time.time()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print(json.dumps({"time": time.time(), "message": fmt % args}), flush=True)

    def do_GET(self):
        self.send_response(503 if failed else 200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"healthy": not failed, "booted_at": booted}).encode())

    def do_POST(self):
        global failed
        if self.path != "/fail":
            self.send_error(404)
            return
        failed = True
        print(json.dumps({"event": "failure_injected", "cause": "test worker stalled", "time": time.time()}), flush=True)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'{"failure_injected":true}')


print(json.dumps({"event": "service_started", "time": booted}), flush=True)
ThreadingHTTPServer(("0.0.0.0", 8099), Handler).serve_forever()
