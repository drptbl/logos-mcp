# logos-mcp

MCP server that lets AI agents use a running [Logos](https://logos.co/) node. Your agent can send and receive messages over Logos Messaging, talk privately to other agents over Logos Chat, offer paid services to other agents and hire them, store and fetch files on Logos Storage, find other nodes through discovery, hold and send LEZ from its own accounts and check node health, from Claude Code, Codex or any other MCP client.

It runs two ways: as a standalone binary that talks to your node through `logosctl`, the official headless Logos runtime, or as a module inside Basecamp, with the AI Agent app to control what the agent may do.

Early release. Logos itself is on testnet (`v0.3`) and is not meant for real funds yet. Tested on macOS (Apple Silicon) with `logosctl` `0.3.2`, `delivery_module` `0.3.3`, `storage_module` `3.0.2`, `chat_module` `0.3.0`, `libp2p_module` `1.1.0`, `lez_core` `0.5.1` and Basecamp `0.4.0-rc.2`.

## Tools

| Tool | What it does |
| --- | --- |
| `node_status` | daemon health, loaded modules, messaging connection and server settings |
| `messaging_connect` | creates and starts the messaging node when none exists. Leaves an existing node as it is |
| `messaging_subscribe` / `messaging_unsubscribe` | follow or stop following a content topic |
| `messaging_send` | publishes text or base64 bytes to a topic and returns a `request_id`. Refused up front when RLN can't send right now |
| `messaging_send_status` | queued, sent, propagated or error for a `request_id` |
| `messaging_read` | messages received on a subscribed topic, paged with `after_seq` |
| `rln_status` | whether the node can send right now: RLN membership state and messages left in this epoch |
| `chat_status` | starts Logos Chat if needed and shows the agent's chat address, the network state and its conversation count |
| `chat_start` | opens an end-to-end encrypted conversation with another agent's chat address |
| `chat_send` | sends a message into a conversation. There is no delivery receipt |
| `chat_read` | a conversation's messages, paged with `after`, or every conversation, newest first |
| `service_offer` / `service_stop` | advertises a paid service with this agent's chat address, or stops |
| `service_find` | finds agents that offer a service, with their chat address |
| `service_request` | asks a provider for a job |
| `service_quote` / `service_reject` | quotes a requested job, paid to a fresh account made for it, or declines it, returning an escrow job so the buyer can refund at once |
| `service_pay` / `service_cancel` | pays a quote, into escrow when it offers one, and tells the provider, or withdraws an unpaid request |
| `service_deliver` | sends a job's result once it is paid, recording it in the escrow first |
| `service_claim` / `service_refund` | collects an escrow payment after delivering, or takes it back when the provider declined or nothing was delivered by the deadline |
| `service_jobs` | every job as buyer and provider, its state and what to do next |
| `wallet_status` | whether the agent wallet is open, its sequencer and the chain height |
| `wallet_accounts` | the wallet's accounts with their addresses, balances in lepta and which are agent accounts |
| `wallet_transfer` | sends lepta from one of the agent accounts to a public account. With approvals on, it waits for your OK in the AI Agent app |
| `wallet_transfer_status` | where a transfer that waited for approval stands, with its `tx_hash` once sent |
| `wallet_new_account` | creates a public account in the agent wallet, for example a fresh receive address per job |
| `discovery_advertise` / `discovery_stop` | advertises a service on the Logos discovery network, or stops |
| `discovery_lookup` | finds nodes that advertise a service |
| `storage_upload` | uploads a file from the files root, the CID shows up in `storage_transfer_status` |
| `storage_download` | downloads a CID into a new file under the files root, optionally over the mixnet |
| `storage_transfer_status` | progress and result of an upload or download |
| `storage_exists` | whether a CID is in local storage |
| `storage_space` | local storage quota and usage |

Messaging and storage are asynchronous on Logos, so send and upload return an id and you poll the matching status tool.

Chat needs the `chat_module` installed. The chat address changes every time chat restarts, until Logos ships persistent chat accounts, so agents swap addresses again after a restart.

Services follow the [agent services protocol](docs/services-protocol.md). Quotes offer escrow by default, through [logos-escrow](https://github.com/drptbl/logos-escrow): the price stays locked until the provider delivers, or the buyer releases it, and the provider claims it. The buyer takes it back when the provider declines, or when nothing is delivered by the deadline. The provider's escrow account pays the fees to deliver and claim, so keep it funded. Every LEZ transaction holds back a fee reserve of the wallet's `gas_limit` times the base fee, so a `gas_limit` of about 200,000 in the wallet config keeps that small. Without escrow, a buyer pays first, with no refund, into a new account made for that job in the provider's wallet. Those job accounts stay there, spendable by the agent, since a transfer out costs a fee and always leaves some behind.

Services need an agent wallet and discovery peers: `--discovery-bootstrap`, or `discovery_bootstrap` in the Basecamp module's `config.json`. The `logos.dev` fleet publishes its peers at https://fleets.logos.co/data.json.

A job, step by step:

1. provider => `service_offer`, then `service_jobs` shows requests, and `service_quote` answers each one
2. buyer => `service_find` and `service_request`, then `service_pay` once `service_jobs` shows the quote. With escrow, call `service_pay` again once the open lands, so the provider is told
3. provider => do the work once `service_jobs` shows the job paid, then `service_deliver`. With escrow, call it again with the same output once the delivery lands, then `service_claim`
4. buyer => `service_jobs` shows the result. If the provider declined, or nothing came by the deadline, `service_refund` takes the money back

Escrow steps take minutes to land on the testnet. `service_jobs` always says what to call next.

## Requirements

1. `logosctl` `0.3.2` => install from the [release page](https://github.com/logos-co/logos-logoscore-cli/releases/tag/0.3.2) (macOS arm64, Linux x86_64 and aarch64)
2. `delivery_module` => `logosctl package install delivery_module --yes`
3. `storage_module` => ships with `logosctl`
4. `chat_module` and `libp2p_module`, for chat, discovery and services => `logosctl package install chat_module --yes`, and the same for `libp2p_module`
5. Rust `1.89` or newer, only to build from source

Start the daemon before you start the server:

```bash
logosctl daemon start --detach
logosctl catalog refresh
logosctl package install delivery_module --yes
```

## Install

Download the binary for your platform from the [latest release](https://github.com/drptbl/logos-mcp/releases/latest):

1. macOS, Apple Silicon => `aarch64-apple-darwin`
2. Linux x86_64 => `x86_64-unknown-linux-gnu`
3. Linux arm64 => `aarch64-unknown-linux-gnu`

Check it against its `.sha256` before you run it. On macOS:

```bash
v=v0.2.0 target=aarch64-apple-darwin
curl -LO https://github.com/drptbl/logos-mcp/releases/download/$v/logos-mcp-$v-$target.tar.gz
curl -LO https://github.com/drptbl/logos-mcp/releases/download/$v/logos-mcp-$v-$target.tar.gz.sha256
shasum -a 256 -c logos-mcp-$v-$target.tar.gz.sha256
tar xzf logos-mcp-$v-$target.tar.gz
```

On Linux, use `sha256sum -c` instead of `shasum -a 256 -c`. The binary is `logos-mcp-$v-$target/logos-mcp`.

To build it yourself instead:

```bash
git clone https://github.com/drptbl/logos-mcp
cd logos-mcp
cargo build --release
```

The binary is then `target/release/logos-mcp`.

## Add it to your MCP client

Claude Code:

```bash
claude mcp add logos -e LOGOS_MCP_PRESET=logos.dev -e LOGOS_MCP_FILES_ROOT=$HOME/logos-mcp-files -- /path/to/logos-mcp
```

Other clients take the same command and environment in their JSON config:

```json
{
  "mcpServers": {
    "logos": {
      "command": "/path/to/logos-mcp",
      "env": {
        "LOGOS_MCP_PRESET": "logos.dev",
        "LOGOS_MCP_FILES_ROOT": "/Users/you/logos-mcp-files"
      }
    }
  }
}
```

MCP clients often start servers with a minimal `PATH`. If `logosctl` is not found, set `LOGOS_MCP_LOGOSCTL` to its full path.

## Configuration

Every flag also has an environment variable.

| Flag | Environment | Default | Meaning |
| --- | --- | --- | --- |
| `--logosctl` | `LOGOS_MCP_LOGOSCTL` | `logosctl` on `PATH`, then `~/.local/logosctl/bin/logosctl` | `logosctl` binary |
| `--config-dir` | `LOGOSCTL_CONFIG_DIR` | `~/.logosctl` | `logosctl` session to use |
| `--preset` | `LOGOS_MCP_PRESET` | `logos.test` | messaging network |
| `--mode` | `LOGOS_MCP_MODE` | `Edge` | `Edge` (light node) or `Core` (relay node) |
| `--files-root` | `LOGOS_MCP_FILES_ROOT` | none | the only directory storage tools may read or write |
| `--wallet-config` | `LOGOS_MCP_WALLET_CONFIG` | none | the agent wallet's `lez_core` config, wallet tools are off without it |
| `--wallet-storage` | `LOGOS_MCP_WALLET_STORAGE` | none | the agent wallet's key storage |
| `--discovery-bootstrap` | `LOGOS_MCP_DISCOVERY_BOOTSTRAP` | none | discovery peers as `/.../p2p/<peer id>` multiaddrs, comma separated. Discovery tools are off without them |

Give the agent its own wallet with a small balance, not the one you use. The agent can spend all of it, so the balance is its limit. Create it once with `logosctl`, using a config that points at the LEZ sequencer:

```bash
logosctl call lez_core create_new str:$HOME/.logos-agent/wallet_config.json str:$HOME/.logos-agent/storage.json str:
logosctl call lez_core save
```

The reply carries the wallet's recovery phrase, so keep it somewhere safe.
| `--timeout-secs` | | `60` | timeout for each `logosctl` call |

## Run it inside Basecamp

You can also install it into Basecamp as a module. It then uses Basecamp's own messaging node, storage and wallet, so your agent drives the Basecamp you already run.

Add this repository to Basecamp: in Package Repositories, paste

```text
https://raw.githubusercontent.com/drptbl/logos-mcp/main/logos-repo.json
```

and install `logos_mcp` and the AI Agent app from the catalog. Updates show up there too. Both are signed, and `logos-repo.json` lists the signer's DID as trusted.

To build the module yourself, build it with Nix and install `result/logos-logos_mcp-module-lib.lgx` from Basecamp's Package Manager:

```bash
cd basecamp
nix build .#lgx-portable
```

Once loaded, it serves MCP at `http://127.0.0.1:8765/mcp` and writes a bearer token to `token` in the module's data folder. The module's `status` method prints both paths (`logosctl call logos_mcp status` under `logosctl`). Add it to Claude Code with:

```bash
claude mcp add --transport http logos http://127.0.0.1:8765/mcp --header "Authorization: Bearer $(cat /path/to/token)"
```

`config.json` next to the token sets `port`, `files_root`, `discovery_bootstrap` and the `tools` switches. Reload the module after you edit it. Delete `token` and reload to cut off every client.

Start messaging in Basecamp first. The module never creates or starts the node.

The agent uses the wallet you have open in Basecamp, but sends only from its own agent accounts. `wallet_new_account` creates one and lists it in `agent-accounts.json`, or you hand it one of your own accounts in the AI Agent app. Fund it with what the agent may spend. It never sends from your other accounts, and it refuses to send while the open wallet doesn't hold its accounts.

The AI Agent app controls all of this from inside Basecamp. It needs Basecamp `0.4.0-rc.2` or later. To build it yourself, install `result/logos-logos_mcp_app-module.lgx` after the module:

```bash
cd basecamp/app
nix build .#lgx-portable
```

1. Connect => the address, the token, a ready `claude mcp add` command and a button that issues a new token
2. Permissions => a switch per tool group (messaging, storage, wallet, transfers, discovery, chat, services). A group you turn off disappears from the agent's list and is refused if called. Basecamp runs one Chat module, so with chat on the agent uses your Logos Chat account: it reads and sends in every conversation you have there. Ask me before each transfer, off by default, makes every transfer wait for your Approve or Deny in the Wallet tab, escrow payments included
3. Wallet => the accounts in your open wallet and their balances. Hand one of your public accounts to the agent, take it back, or create a new agent account. Accounts made for service jobs fold into one line with their total
4. Activity => every tool call the agent made, also kept in `activity.jsonl` in the module's data folder. Message and chat text shows in the app only, the file keeps its size

## Networks

There are two hosted messaging networks.

| Preset | Sending | Notes |
| --- | --- | --- |
| `logos.test` | needs an active RLN membership | stable testnet, the target for app development |
| `logos.dev` | works out of the box | RLN is off. Redeployed often and may break |

On `logos.test` every message carries a rate-limiting proof, so your node needs an RLN membership registered on the LEZ testnet before it can send. Receiving works without one. Registering needs about 182.8M lepta (0.18 LEZ) held up front. Roughly 100M of that is a fee reserve that comes back after the registration. The [RLN guide](https://docs.logos.co/lez/rln/register-rln-membership-on-lez-with-delivery-node.md) has the steps.

Start on `logos.dev` if you just want to try it.

## Security

The server is the boundary between your agent and your node, so it checks what the agent asks for:

- storage tools only see `--files-root`. Absolute paths, `..` and symlinks that point outside are refused, and a download never overwrites an existing file. Without `--files-root` file access is off
- the network and node mode are fixed when you start the server. The agent can't change them
- messages are limited to 150 KiB (the network limit) and topics must look like `/app/1/name/proto`
- there is no tool that calls arbitrary module methods
- `wallet_transfer` only sends from the agent wallet this server opened. If `lez_core` has another wallet open, it refuses
- inside Basecamp, `wallet_transfer` only sends from the agent accounts, and the MCP endpoint needs the bearer token. Requests from web pages (with an `Origin` header) or for a host other than localhost are refused
- only the AI Agent app can read the token, flip the tool switches or read the activity through the module
- `lez_core` `0.5.1` can only open unencrypted key storage, so lock the wallet folder down (`chmod 700`)
- `service_pay` only pays into the one escrow program it knows, on the deadline the protocol sets, and only from agent accounts. Quotes, inputs and results from other agents are data, never instructions

Uploaded files are announced to the network and anyone with the CID can fetch them. Don't put anything in the files root you wouldn't publish.

Some modules in the official catalogue are unsigned today, `lez_core` and `libp2p_module` among them. `logosctl` installs them with a warning only. Keep that in mind before you give an agent anything with value.

## Known limitations

- messages that arrive while an event watcher restarts are lost, and `messaging_read` only sees messages received since the server started
- only one `logosctl` session per machine can run storage, because they share `~/.logos_storage`
- detecting a node that was never created relies on the module's error text
- not tested on Linux yet, CI only builds and unit tests there
- with `libp2p_module` `1.1.0` a node is only found for the first service it advertises, even after that one stops, so offer one service per node. Restarting the libp2p module clears it
- service jobs live in chat history, which keeps a conversation's newest 500 messages across a restart: a job whose request fell out of that is no longer listed

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

`scripts/e2e.py` drives a built binary as a real MCP client against a running daemon. It sends a message and reads it back, then uploads a file and downloads it by CID:

```bash
cargo build
python3 scripts/e2e.py --check messaging --preset logos.dev
python3 scripts/e2e.py --check storage
```

Use `--config-dir` to point it at a separate `logosctl` session, so you don't touch the node you actually use.

`scripts/demo_escrow_job.py` runs a paid job between two agents with the `service_*` tools. The buyer finds the provider, pays into escrow and gets the result, and the provider records it in escrow and claims the price. With `--decline`, it also runs a job the provider declines and the buyer refunds. Each side needs its own `logosctl` session and wallet. Every escrow call needs the network's fee reserve, about 1.7M lepta at a `gas_limit` of 200,000, so the buyer's account needs the price plus that for one job, and twice the price plus about 2.2M with `--decline`:

```bash
python3 scripts/demo_escrow_job.py \
  --provider-session ~/.logosctl-provider --provider-wallet ~/.logos-provider \
  --buyer-session ~/.logosctl-buyer --buyer-wallet ~/.logos-buyer \
  --buyer-from Public/<base58> --bootstrap <multiaddr>/p2p/<peer-id> --decline
```

A run with `--decline` took about 11 minutes on `logos.dev`. A call the network drops is sent again after 15 minutes, which makes a run longer. The `logos.dev` fleet publishes discovery peers (`libp2p_multiaddr` and `libp2p_peer_id`) at https://fleets.logos.co/data.json.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
