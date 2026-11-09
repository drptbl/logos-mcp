#!/usr/bin/env python3
"""Paid agent-to-agent job over Logos, through escrow, driven through two
logos-mcp servers with the service_* tools.

provider: offers a service, quotes the job, records the result in escrow,
          sends it, then claims the price.
buyer:    finds the provider, asks for the job, pays into escrow, reads the
          result.

With --decline, a second job is declined instead: the provider returns it in
escrow and the buyer takes the money back at once.

Each side needs its own logosctl session and wallet (lez_core holds one wallet
per daemon), with lez_core, chat_module and libp2p_module installed. Every
escrow call needs the network's fee reserve, gas_limit x base fee, about 1.7M
lepta at a gas_limit of 200,000. So the buyer needs about the price plus 1.7M
lepta for one job, and twice the price plus 2.2M with --decline; the provider
needs about 1.7M. Each escrow step takes a few minutes to land on the testnet. libp2p_module 1.1.0 lets a
node be found only for the first service it advertises, so reload it on the
provider's session between runs: logosctl module reload libp2p_module. A
server only uses a wallet it opened itself, so if another logos-mcp server
used a session since lez_core loaded, reload lez_core there too.

Exits non-zero if a job does not complete.
"""
import argparse
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e import Client  # noqa: E402

DOCUMENT = ("Logos is a privacy-first stack for decentralised apps. It combines a blockchain with "
            "private execution, peer-to-peer messaging, content-addressed storage and a mixnet.")


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


def log(*parts):
    print(time.strftime("%H:%M:%S"), *parts, flush=True)


def call(c, who, tool, **args):
    ok, body = c.tool(tool, **args)
    if not ok:
        sys.exit(f"[{who}] {tool} failed: {body}")
    log(f"[{who}] {tool}")
    return body


def wait_for(fn, what, timeout, every=15):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(every)
    sys.exit(f"timed out waiting for {what}")


# The server holds back a second escrow call while the first may still land,
# and lets it go once that one counts as dropped, after 15 minutes.
RESEND_AFTER = 16 * 60


def until_landed(c, who, tool, args, landed, what, timeout):
    """Wait for an escrow call to land, calling the tool again now and then,
    as an agent would: a call the network dropped is then sent once more."""
    deadline = time.time() + timeout
    tried = time.time()
    while time.time() < deadline:
        if landed():
            return
        if time.time() - tried > RESEND_AFTER:
            ok, body = c.tool(tool, **args)
            log(f"[{who}] {tool} again: {'sent' if ok else body}")
            tried = time.time()
        time.sleep(45)
    sys.exit(f"timed out waiting for {what}")


def job_in(c, job):
    _, body = c.tool("service_jobs")
    return next((j for j in body.get("jobs", []) if j["job"] == job), None)


def state(c, job):
    return (job_in(c, job) or {}).get("state")


def escrow(c, job):
    return ((job_in(c, job) or {}).get("escrow") or {}).get("state")


def paid_job(provider, buyer, chat_address, args, text):
    """Request, quote and pay a job; returns its id once the provider sees it paid."""
    job = call(buyer, "buyer", "service_request", chat_address=chat_address,
               service_id=args.service, input_text=text)["job"]
    wait_for(lambda: state(provider, job) == "requested", "the request", 300, 5)
    quote = call(provider, "provider", "service_quote", job=job)["quote"]
    if not quote.get("escrow"):
        sys.exit("the quote offers no escrow")
    wait_for(lambda: state(buyer, job) == "quoted", "the quote", 300, 5)
    opened = call(buyer, "buyer", "service_pay", job=job, **{"from": args.buyer_from})
    log(f"[buyer] escrow open {opened.get('escrow_open_tx')} sent, waiting for it to land")

    def told():
        # Tells the provider once the open has landed; refused until then.
        ok, body = buyer.tool("service_pay", job=job, **{"from": args.buyer_from})
        return ok and body.get("paid")
    wait_for(told, "the escrow open to land", args.timeout, 60)
    wait_for(lambda: state(provider, job) == "paid", "the provider to see it paid", 600)
    log(f"[provider] job {job} is paid in escrow")
    return job


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default="target/debug/logos-mcp")
    ap.add_argument("--provider-session", required=True)
    ap.add_argument("--provider-wallet", required=True, help="folder with wallet_config.json and storage.json")
    ap.add_argument("--provider-escrow", help="provider's Public/<base58> account escrow pays (default: its best-funded)")
    ap.add_argument("--buyer-session", required=True)
    ap.add_argument("--buyer-wallet", required=True)
    ap.add_argument("--buyer-from", required=True,
                    help="buyer's Public/<base58> account, holding the price plus the network's fee reserve")
    ap.add_argument("--bootstrap", required=True, help="comma-separated /p2p/ discovery multiaddrs")
    ap.add_argument("--preset", default="logos.dev")
    ap.add_argument("--price", default="1000000", help="lepta per job")
    ap.add_argument("--service", default=f"logos-mcp-demo/summarize/v{int(time.time())}")
    ap.add_argument("--decline", action="store_true", help="also run a job the provider declines")
    ap.add_argument("--timeout", type=int, default=1800, help="seconds to wait for each escrow step")
    args = ap.parse_args()

    logs = tempfile.mkdtemp(prefix="logos-mcp-demo-")
    provider = start("provider", args.bin, args.provider_session, args.provider_wallet, args.bootstrap, args.preset, logs)
    buyer = start("buyer", args.bin, args.buyer_session, args.buyer_wallet, args.bootstrap, args.preset, logs)

    offer = {"service_id": args.service, "tag": "summarize", "price_lepta": args.price, "deliver_within_s": 3600}
    if args.provider_escrow:
        offer["escrow_account"] = args.provider_escrow
    call(provider, "provider", "service_offer", **offer)

    def find():
        _, body = buyer.tool("service_find", service_id=args.service)
        return next((p for p in body.get("providers", []) if not p["this_node"]), None)
    found = wait_for(find, "the provider's offer", 300, 10)
    log(f"[buyer] found provider {found['peer_id']}")

    # A delivered job: the result is recorded in escrow before it's sent,
    # then the provider claims the price.
    job = paid_job(provider, buyer, found["chat_address"], args, DOCUMENT)
    words = len(DOCUMENT.split())
    summary = f"{DOCUMENT.split('. ')[0].rstrip('.')}. ({words} words)"
    deliver = {"job": job, "output_text": summary}
    call(provider, "provider", "service_deliver", **deliver)
    until_landed(provider, "provider", "service_deliver", deliver,
                 lambda: escrow(provider, job) == "delivered", "the delivery to land", args.timeout)
    call(provider, "provider", "service_deliver", **deliver)
    result = wait_for(lambda: (job_in(buyer, job) or {}).get("output") if state(buyer, job) == "delivered" else None,
                      "the result", 600)
    log(f"[buyer] result: {result['text']}")
    call(provider, "provider", "service_claim", job=job)
    until_landed(provider, "provider", "service_claim", {"job": job},
                 lambda: escrow(provider, job) == "claimed", "the claim to land", args.timeout)
    log("[provider] the price is claimed")

    # A declined job: Return moves nothing, then the buyer refunds at once.
    if args.decline:
        job = paid_job(provider, buyer, found["chat_address"], args, "a job to decline")
        call(provider, "provider", "service_reject", job=job, reason="can't do it after all")
        wait_for(lambda: escrow(buyer, job) == "returned", "the return to land", args.timeout, 45)
        call(buyer, "buyer", "service_refund", job=job)
        until_landed(buyer, "buyer", "service_refund", {"job": job},
                     lambda: escrow(buyer, job) == "refunded", "the refund to land", args.timeout)
        log("[buyer] the price is back")

    call(provider, "provider", "service_stop", service_id=args.service)
    provider.close()
    buyer.close()
    print(f"DEMO: PASS (logs in {logs})")


if __name__ == "__main__":
    main()
