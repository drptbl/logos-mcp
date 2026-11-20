#!/usr/bin/env python3
"""End-to-end check: drive a logos-mcp binary over stdio as an MCP client.

messaging: connect, subscribe, send a message and read it back.
storage:   upload a file, download it by CID and compare the bytes.

Needs a running logosctl daemon for the chosen session. Exits non-zero on failure.

With --devnet, it runs two servers on the local devnet that `logos-devnet up` starts
from logos-devnet.toml, one per session, and checks them against each other:

messaging: one sends through the devnet's hub, the other reads it.
storage:   one uploads, the other downloads by CID through the shared storage node.
wallet:    one sends lepta from its funded wallet, the other sees its balance grow.
discovery: one advertises a service, the other finds it.
"""
import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time


class Client:
    def __init__(self, cmd, env, stderr):
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=stderr, env=env, text=True, bufsize=1)
        self.next_id = 0

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        self._send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params or {}})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("server closed stdout")
            reply = json.loads(line)
            if reply.get("id") == self.next_id:
                return reply

    def notify(self, method):
        self._send({"jsonrpc": "2.0", "method": method})

    def tool(self, name, **arguments):
        reply = self.request("tools/call", {"name": name, "arguments": arguments})
        if "error" in reply:
            return True, reply["error"]
        result = reply["result"]
        text = result["content"][0]["text"]
        try:
            body = json.loads(text)
        except ValueError:
            body = text
        ok = not result.get("isError")
        print(f"{'ok ' if ok else 'ERR'} {name}: {json.dumps(body)[:200]}")
        return ok, body

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=10)


def poll(fn, done, attempts=30, delay=1.0):
    value = None
    for _ in range(attempts):
        time.sleep(delay)
        value = fn()
        if done(value):
            break
    return value


def check_messaging(c):
    c.tool("messaging_connect")
    topic = "/logos-mcp/1/e2e/proto"
    c.tool("messaging_subscribe", topic=topic)
    time.sleep(3)
    text = f"logos-mcp e2e {time.time_ns()}"
    ok, sent = c.tool("messaging_send", topic=topic, text=text)
    if not ok:
        return False
    status = poll(lambda: c.tool("messaging_send_status", request_id=sent["request_id"])[1],
                  lambda s: s.get("propagated") or s.get("sent") or s.get("error"), attempts=15, delay=2)
    if status.get("error"):
        return False
    _, got = c.tool("messaging_read", topic=topic)
    matches = [m for m in got["messages"] if m.get("text") == text]
    return len(matches) == 1


def check_storage(c, root):
    original = root / "e2e-source.txt"
    original.write_bytes(b"stored via logos-mcp at %d\n" % time.time_ns())
    ok, refused = c.tool("storage_upload", path="../outside.txt")
    if ok:
        print("path guard did not refuse '..'")
        return False
    ok, up = c.tool("storage_upload", path=original.name)
    if not ok:
        return False
    done = lambda t: isinstance(t, dict) and t.get("done")
    t = poll(lambda: c.tool("storage_transfer_status", session_id=up["session_id"])[1], done)
    cid = t.get("cid") if isinstance(t, dict) else None
    if not cid:
        return False
    copy = f"e2e-copy-{time.time_ns()}.txt"
    ok, dl = c.tool("storage_download", cid=cid, path=copy)
    if not ok:
        return False
    poll(lambda: c.tool("storage_transfer_status", session_id=dl["session_id"])[1], done)
    return (root / copy).read_bytes() == original.read_bytes()


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def devnet(*args):
    """A logos-devnet command's JSON output."""
    return json.loads(run(["logos-devnet", "--json", *args]))


def start(bin, env, root, name):
    """A logos-mcp server for one side, initialized, its log in root."""
    c = Client([bin], env, open(root / f"{name}.log", "w"))
    c.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                             "clientInfo": {"name": f"logos-mcp-e2e-{name}", "version": "0"}})
    c.notify("notifications/initialized")
    return c


def devnet_servers(bin):
    """Two servers, one per devnet session: each its session's daemon and funded wallet,
    messaging and discovery through the devnet's hub, a files root of its own."""
    env = devnet("env")
    hub = (env.get("LOGOS_DEVNET_DELIVERY_CONFIG"), env.get("LOGOS_DEVNET_DISCOVERY_BOOTSTRAP"))
    if not all(hub):
        sys.exit("the devnet runs no messaging hub: run logos-devnet up in this repo")
    sides = {}
    for name in ("mcp-a", "mcp-b"):
        session = devnet("session", "env", name)
        # libp2p_module finds a node only by the first service it advertises: start
        # each run with a fresh daemon, its wallet and modules kept.
        run(["logos-devnet", "session", "restart", name])
        root = pathlib.Path(tempfile.mkdtemp(prefix=f"logos-mcp-e2e-{name}-"))
        server_env = dict(os.environ, **env,
                          LOGOSCTL_CONFIG_DIR=session["LOGOSCTL_CONFIG_DIR"],
                          LOGOS_MCP_DELIVERY_CONFIG=hub[0],
                          LOGOS_MCP_DISCOVERY_BOOTSTRAP=hub[1],
                          LOGOS_MCP_FILES_ROOT=str(root),
                          LOGOS_MCP_WALLET_CONFIG=session["LOGOS_DEVNET_WALLET_CONFIG"],
                          LOGOS_MCP_WALLET_STORAGE=session["LOGOS_DEVNET_WALLET_STORAGE"])
        sides[name] = (start(bin, server_env, root, name), root, session["LOGOS_DEVNET_ACCOUNT"])
        print(f"{name}: session {session['LOGOSCTL_CONFIG_DIR']}, log {root / (name + '.log')}")
    return sides["mcp-a"], sides["mcp-b"]


def check_messaging_between(a, b):
    topic = "/logos-mcp/1/e2e-devnet/proto"
    for c in (a, b):
        c.tool("messaging_connect")
    b.tool("messaging_subscribe", topic=topic)
    time.sleep(3)
    text = f"logos-mcp devnet e2e {time.time_ns()}"
    ok, _ = a.tool("messaging_send", topic=topic, text=text)
    if not ok:
        return False
    got = poll(lambda: b.tool("messaging_read", topic=topic)[1],
               lambda r: any(m.get("text") == text for m in r.get("messages", [])), attempts=30, delay=2)
    return any(m.get("text") == text for m in got.get("messages", []))


def check_storage_between(a, root_a, b, root_b):
    original = root_a / "e2e-shared.txt"
    original.write_bytes(b"shared via logos-mcp at %d\n" % time.time_ns())
    ok, up = a.tool("storage_upload", path=original.name)
    if not ok:
        return False
    done = lambda t: isinstance(t, dict) and t.get("done")
    t = poll(lambda: a.tool("storage_transfer_status", session_id=up["session_id"])[1], done)
    cid = t.get("cid") if isinstance(t, dict) else None
    if not cid:
        return False
    ok, dl = b.tool("storage_download", cid=cid, path="e2e-shared-copy.txt")
    if not ok:
        return False
    t = poll(lambda: b.tool("storage_transfer_status", session_id=dl["session_id"])[1], done, attempts=60)
    copy = root_b / "e2e-shared-copy.txt"
    return isinstance(t, dict) and t.get("success") and copy.exists() and copy.read_bytes() == original.read_bytes()


def balance(c, address):
    _, listed = c.tool("wallet_accounts")
    for account in listed.get("accounts", []):
        if account.get("address") == address:
            return int(account.get("balance_lepta") or 0)
    return None


def check_wallet_between(a, from_address, b, to_address):
    before = balance(b, to_address)
    if before is None or balance(a, from_address) is None:
        print("a session's funded account isn't in its wallet")
        return False
    amount = 1000
    ok, sent = a.tool("wallet_transfer", **{"from": from_address}, to=to_address, amount_lepta=str(amount))
    if not ok or not sent.get("tx_hash"):
        return False
    included = poll(lambda: a.tool("wallet_tx_status", tx_hash=sent["tx_hash"])[1],
                    lambda s: isinstance(s, dict) and s.get("included"), attempts=30, delay=2)
    if not (isinstance(included, dict) and included.get("included")):
        return False
    after = poll(lambda: balance(b, to_address), lambda v: v is not None and v >= before + amount,
                 attempts=15, delay=2)
    return after is not None and after >= before + amount


def check_discovery_between(a, b):
    service = "logos-mcp/e2e/v1"
    tag = f"e2e {time.time_ns() % 10**9}"
    ok, _ = a.tool("discovery_advertise", service_id=service, tag=tag)
    if not ok:
        return False
    found = poll(lambda: b.tool("discovery_lookup", service_id=service)[1],
                 lambda r: isinstance(r, dict) and any(p.get("tag") == tag for p in r.get("providers", [])),
                 attempts=30, delay=2)
    return isinstance(found, dict) and any(p.get("tag") == tag for p in found.get("providers", []))


def main_devnet(args):
    (a, root_a, account_a), (b, root_b, account_b) = devnet_servers(args.bin)
    for c in (a, b):
        c.tool("node_status")
    results = {
        "messaging": check_messaging_between(a, b),
        "storage": check_storage_between(a, root_a, b, root_b),
        "wallet": check_wallet_between(a, account_a, b, account_b),
        "discovery": check_discovery_between(a, b),
    }
    for c in (a, b):
        c.close()
    for name, passed in results.items():
        print(f"{name}: {'PASS' if passed else 'FAIL'}")
    print(f"server logs: {root_a} and {root_b}")
    sys.exit(0 if all(results.values()) else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="target/debug/logos-mcp", help="logos-mcp binary")
    ap.add_argument("--config-dir", help="logosctl session (LOGOSCTL_CONFIG_DIR)")
    ap.add_argument("--preset", default="logos.dev", help="messaging network (default: logos.dev, no RLN)")
    ap.add_argument("--check", choices=["messaging", "storage", "all"], default="all")
    ap.add_argument("--devnet", action="store_true",
                    help="two servers on the devnet logos-devnet.toml sets up, checked against each other")
    args = ap.parse_args()
    if args.devnet:
        main_devnet(args)

    root = pathlib.Path(tempfile.mkdtemp(prefix="logos-mcp-e2e-"))
    env = dict(os.environ, LOGOS_MCP_PRESET=args.preset, LOGOS_MCP_FILES_ROOT=str(root))
    if args.config_dir:
        env["LOGOSCTL_CONFIG_DIR"] = args.config_dir
    log = open(root / "server.log", "w")
    c = Client([args.bin], env, log)
    c.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                             "clientInfo": {"name": "logos-mcp-e2e", "version": "0"}})
    c.notify("notifications/initialized")
    c.tool("node_status")

    results = {}
    if args.check in ("messaging", "all"):
        results["messaging"] = check_messaging(c)
    if args.check in ("storage", "all"):
        results["storage"] = check_storage(c, root)
    c.close()

    for name, passed in results.items():
        print(f"{name}: {'PASS' if passed else 'FAIL'}")
    print(f"server log: {root / 'server.log'}")
    sys.exit(0 if all(results.values()) else 1)


if __name__ == "__main__":
    main()
