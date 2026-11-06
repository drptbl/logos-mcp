#!/usr/bin/env python3
"""Paid agent-to-agent job over Logos, driven through two logos-mcp servers.

provider: advertises a service, quotes a fresh receive account per job, waits
          until that account holds the price, does the work, replies.
buyer:    finds the provider through discovery, asks for a quote, pays with
          wallet_transfer, sends the job, reads the result.

Both sides talk over Logos Messaging. Payment is matched by the per-job receive
account's balance, so no transaction has to be decoded or polled.

Each side needs its own logosctl session and wallet (lez_core holds one wallet
per daemon). Exits non-zero if the job does not complete.
"""
import argparse
import json
import os
import secrets
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e import Client  # noqa: E402

SERVICE = "logos-mcp-demo/summarize/v1"


def start(name, bin_, session, wallet_dir, bootstrap, preset, log_dir):
    env = dict(
        os.environ,
        LOGOSCTL_CONFIG_DIR=session,
        LOGOS_MCP_PRESET=preset,
        LOGOS_MCP_WALLET_CONFIG=os.path.join(wallet_dir, "wallet_config.json"),
        LOGOS_MCP_WALLET_STORAGE=os.path.join(wallet_dir, "storage.json"),
        LOGOS_MCP_DISCOVERY_BOOTSTRAP=bootstrap,
    )
    c = Client([bin_], env, open(os.path.join(log_dir, f"{name}.log"), "w"))
    c.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                             "clientInfo": {"name": name, "version": "0"}})
    c.notify("notifications/initialized")
    return c


def call(c, who, tool, **args):
    print(f"[{who}] {tool}", flush=True)
    ok, body = c.tool(tool, **args)
    if not ok:
        sys.exit(f"[{who}] {tool} failed: {body}")
    return body


def wait_for(fn, what, timeout, every=3):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = fn()
        if value is not None:
            return value
        time.sleep(every)
    sys.exit(f"timed out waiting for {what}")


class Inbox:
    """Reads one topic and returns new JSON messages of a given type."""

    def __init__(self, c, topic):
        self.c, self.topic, self.after = c, topic, 0

    def next(self, kind):
        _, body = self.c.tool("messaging_read", topic=self.topic, after_seq=self.after)
        for m in body.get("messages", []):
            self.after = max(self.after, m["seq"])
            try:
                msg = json.loads(m.get("text") or "")
            except ValueError:
                continue
            if msg.get("type") == kind:
                return msg
        return None


def balance(c, address):
    _, body = c.tool("wallet_accounts")
    for a in body.get("accounts", []):
        if a.get("address") == address and a.get("balance_lepta") is not None:
            return int(a["balance_lepta"])
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="target/debug/logos-mcp")
    ap.add_argument("--provider-session", required=True)
    ap.add_argument("--provider-wallet", required=True, help="folder with wallet_config.json and storage.json")
    ap.add_argument("--buyer-session", required=True)
    ap.add_argument("--buyer-wallet", required=True)
    ap.add_argument("--buyer-from", required=True, help="buyer's funded Public/<base58> account")
    ap.add_argument("--bootstrap", required=True, help="comma-separated /p2p/ discovery multiaddrs")
    ap.add_argument("--preset", default="logos.dev")
    ap.add_argument("--price", default="1000000", help="lepta per job")
    args = ap.parse_args()

    logs = tempfile.mkdtemp(prefix="logos-mcp-demo-")
    provider = start("provider", args.bin, args.provider_session, args.provider_wallet, args.bootstrap, args.preset, logs)
    buyer = start("buyer", args.bin, args.buyer_session, args.buyer_wallet, args.bootstrap, args.preset, logs)

    # provider: come online and advertise
    call(provider, "provider", "messaging_connect")
    ad = call(provider, "provider", "discovery_advertise", service_id=SERVICE, tag="v1")
    inbox_topic = f"/logos-mcp-demo/1/{ad['peer_id']}/json"
    call(provider, "provider", "messaging_subscribe", topic=inbox_topic)
    provider_inbox = Inbox(provider, inbox_topic)

    # buyer: find the provider, open a reply topic, ask for a quote
    call(buyer, "buyer", "messaging_connect")

    def find():
        _, body = buyer.tool("discovery_lookup", service_id=SERVICE)
        others = [p for p in body.get("providers", []) if not p["this_node"]]
        return others[0] if others else None
    found = wait_for(find, "the provider's advertisement", 120)
    print(f"[buyer] found provider {found['peer_id']}")
    reply_topic = f"/logos-mcp-demo/1/buyer-{secrets.token_hex(4)}/json"
    call(buyer, "buyer", "messaging_subscribe", topic=reply_topic)
    buyer_inbox = Inbox(buyer, reply_topic)
    time.sleep(5)  # let both subscriptions settle on the network
    provider_topic = f"/logos-mcp-demo/1/{found['peer_id']}/json"
    call(buyer, "buyer", "messaging_send", topic=provider_topic,
         text=json.dumps({"type": "quote_request", "service": SERVICE, "reply_to": reply_topic}))

    # provider: quote a fresh receive account for this job
    request = wait_for(lambda: provider_inbox.next("quote_request"), "the quote request", 120)
    invoice = call(provider, "provider", "wallet_new_account")["address"]
    job_id = secrets.token_hex(4)
    call(provider, "provider", "messaging_send", topic=request["reply_to"],
         text=json.dumps({"type": "quote", "job_id": job_id, "price_lepta": args.price, "pay_to": invoice}))

    # buyer: pay the quote, then send the job
    quote = wait_for(lambda: buyer_inbox.next("quote"), "the quote", 120)
    paid = call(buyer, "buyer", "wallet_transfer", **{"from": args.buyer_from, "to": quote["pay_to"],
                                                     "amount_lepta": quote["price_lepta"]})
    document = ("Logos is a privacy-first stack for decentralised apps. It combines a blockchain with "
                "private execution, peer-to-peer messaging, content-addressed storage and a mixnet.")
    call(buyer, "buyer", "messaging_send", topic=provider_topic,
         text=json.dumps({"type": "job", "job_id": quote["job_id"], "tx_hash": paid["tx_hash"], "text": document}))

    # provider: wait for the job and for the invoice account to hold the price, then work
    job = wait_for(lambda: provider_inbox.next("job"), "the job", 120)
    t = time.time()
    wait_for(lambda: True if balance(provider, invoice) >= int(args.price) else None,
             "payment to the invoice account", 900, every=15)
    print(f"[provider] payment confirmed on {invoice} after {time.time() - t:.0f}s")
    first = job["text"].split(". ")[0].rstrip(".") + "."
    summary = f"{first} ({len(job['text'].split())} words)"
    call(provider, "provider", "messaging_send", topic=request["reply_to"],
         text=json.dumps({"type": "result", "job_id": job["job_id"], "summary": summary}))

    # buyer: read the result
    result = wait_for(lambda: buyer_inbox.next("result"), "the result", 120)
    print(f"[buyer] result for job {result['job_id']}: {result['summary']}")
    call(provider, "provider", "discovery_stop", service_id=SERVICE)
    provider.close()
    buyer.close()
    print(f"DEMO: PASS (logs in {logs})")


if __name__ == "__main__":
    main()
