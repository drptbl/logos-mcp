#!/usr/bin/env python3
"""End-to-end check: drive a logos-mcp binary over stdio as an MCP client.

messaging: connect, subscribe, send a message and read it back.
storage:   upload a file, download it by CID and compare the bytes.

Needs a running logosctl daemon for the chosen session. Exits non-zero on failure.
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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="target/debug/logos-mcp", help="logos-mcp binary")
    ap.add_argument("--config-dir", help="logosctl session (LOGOSCTL_CONFIG_DIR)")
    ap.add_argument("--preset", default="logos.dev", help="messaging network (default: logos.dev, no RLN)")
    ap.add_argument("--check", choices=["messaging", "storage", "all"], default="all")
    args = ap.parse_args()

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
