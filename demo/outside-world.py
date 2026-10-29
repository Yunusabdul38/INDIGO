#!/usr/bin/env python3
"""The "outside world" for the iOS demo: a tiny local HTTP server that plays
everyone who is not Alice's app on the regtest stack. It pays Alice's
Lightning invoices, issues bills for her to pay, funds on-chain addresses and
mines blocks, through the same Docker services the terminal demo uses.

    demo/outside-world.py            listens on 127.0.0.1:8733

GET  /federation          the federation's invite code
GET  /invoice?sats=N      a Lightning bill of N sats, from the outside world
POST /pay      {invoice}  pays an invoice from the outside world
POST /fund     {address, btc}  sends coins on chain, then mines a block
POST /mine     {blocks}   mines blocks
"""

import json
import pathlib
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

ROOT = pathlib.Path(__file__).resolve().parent.parent
COMPOSE = ["docker", "compose", "-f", str(ROOT / "infra" / "docker-compose.regtest.yml"), "exec", "-T"]
BTC = COMPOSE + ["bitcoind", "bitcoin-cli", "-regtest", "-rpcuser=indigo", "-rpcpassword=indigo", "-rpcwallet=indigo"]
LND = COMPOSE + ["lnd", "lncli", "--network=regtest"]


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip()


def mine(blocks):
    run(BTC + ["-generate", str(blocks)])


class Handler(BaseHTTPRequestHandler):
    def reply(self, body, status=200):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        # The web demo (another local origin) calls it from the browser.
        self.send_header("access-control-allow-origin", "*")
        self.end_headers()
        self.wfile.write(data)

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("access-control-allow-origin", "*")
        self.send_header("access-control-allow-methods", "GET, POST")
        self.send_header("access-control-allow-headers", "content-type")
        self.end_headers()

    def body(self):
        length = int(self.headers.get("content-length", 0))
        return json.loads(self.rfile.read(length) or b"{}")

    def do_GET(self):
        url = urlparse(self.path)
        try:
            if url.path == "/federation":
                invite = run(COMPOSE + ["fedimintd", "cat", "/data/invite-code"])
                return self.reply({"invite": invite})
            if url.path == "/invoice":
                sats = int(parse_qs(url.query)["sats"][0])
                invoice = json.loads(run(LND + ["addinvoice", "--amt", str(sats)]))["payment_request"]
                return self.reply({"invoice": invoice})
            self.reply({"error": "not found"}, 404)
        except Exception as e:  # noqa: BLE001 - a demo helper: report, don't crash
            self.reply({"error": str(e)}, 500)

    def do_POST(self):
        try:
            body = self.body()
            if self.path == "/pay":
                out = run(LND + ["sendpayment", "--pay_req", body["invoice"], "--force"])
                return self.reply({"ok": "SUCCEEDED" in out, "detail": out.splitlines()[-1] if out else ""})
            if self.path == "/fund":
                txid = run(BTC + ["sendtoaddress", body["address"], str(body["btc"])])
                mine(1)
                return self.reply({"txid": txid})
            if self.path == "/mine":
                mine(int(body.get("blocks", 1)))
                return self.reply({"ok": True})
            self.reply({"error": "not found"}, 404)
        except Exception as e:  # noqa: BLE001
            self.reply({"error": str(e)}, 500)

    def log_message(self, fmt, *args):
        print("outside world:", fmt % args)


if __name__ == "__main__":
    try:
        server = ThreadingHTTPServer(("127.0.0.1", 8733), Handler)
    except OSError:
        raise SystemExit("port 8733 is taken: the outside world is probably already running "
                         "(stop it with: pkill -f outside-world.py)")
    print("outside world listening on http://127.0.0.1:8733")
    server.serve_forever()
