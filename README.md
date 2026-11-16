# logos-mcp

MCP server that lets AI agents use a running [Logos](https://logos.co/) node. Your agent can send and receive messages over Logos Messaging, talk privately to other agents over Logos Chat, store and fetch files on Logos Storage, find other nodes through discovery, hold and send LEZ from its own accounts and check node health, from Claude Code, Codex or any other MCP client.

It runs two ways: as a standalone binary that talks to your node through `logosctl`, the official headless Logos runtime, or as a module inside Basecamp, with the AI Agent app to control what the agent may do.

For agents that hire each other, paid through an escrow, use [logos-agents](https://github.com/drptbl/logos-agents). It builds on this server as a library and adds the `service_*` tools.

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
| `rln_status` | whether the node can send right now: RLN membership state, messages left in this epoch and the node's memberships |
| `chat_status` | starts Logos Chat if needed and shows the agent's chat address, the network state, its conversation count and whether chat reports healthy |
| `chat_start` | opens an end-to-end encrypted conversation with another agent's chat address |
| `chat_send` | sends a message into a conversation. There is no delivery receipt |
| `chat_read` | a conversation's messages, paged with `after`, or every conversation, newest first |
| `wallet_status` | whether the agent wallet is open and the agent may use it, its sequencer, the chain height and the last block its private accounts synced |
| `wallet_accounts` | the wallet's accounts with their addresses, balances in lepta and which are agent accounts; none when the open wallet isn't the agent's |
| `wallet_account` | one public account's balance, nonce and the program data it holds |
| `wallet_labels` | the account behind a label, or an account's labels, in the agent's wallet only |
| `wallet_tx_status` | whether a LEZ transaction is in a block yet, answered at once by the wallet's sequencer |
| `wallet_faucet` | asks the LEZ testnet faucet for its drop, 0.1 LEZ once a day, into one of the agent's own public accounts. Needs Basecamp's LEZ Faucet module |
| `wallet_transfer` | sends lepta from one of the agent accounts to a public account, or waits for [approval](#room-approval): your OK in the AI Agent app, or trusted people in a Muster room |
| `wallet_transfer_status` | where a transfer that waited for approval stands, with its `tx_hash` once sent. A room request also shows its proposal and the trusted approvals so far |
| `wallet_new_account` | creates a public account in the agent wallet, for example a fresh receive address per job |
| `discovery_advertise` / `discovery_stop` | advertises a service on the Logos discovery network, or stops |
| `discovery_lookup` | finds nodes that advertise a service |
| `discovery_peers` | the peers the discovery node is connected to, inbound and outbound |
| `discovery_connect` / `discovery_disconnect` / `discovery_ping` | dials a peer by its multiaddr, drops one, or pings one for its round trip |
| `chain_info` | the Logos chain's id, consensus tip, slot timing and network peers |
| `chain_blocks` | the chain blocks in a range of slots, at most 600 at a time |
| `chain_events` | each new finalized block's height and header id as it arrives, paged by `after_seq` |
| `chain_rewards` / `chain_claim_reward` | the leader reward vouchers and mined tickets the chain node hasn't claimed, or claims one voucher into its wallet. The claim's fee comes from the node's funding key |
| `chain_wallet` | the chain node's own addresses with their balances, or one address with its notes |
| `chain_transfer` | sends TestToken from one of the chain node's addresses. Waits for your OK in the AI Agent app by default, or for a Muster room. Outside Basecamp only a room can approve it |
| `chain_block` / `chain_transaction` | a chain block and its transactions by its header id, or a transaction still waiting in the mempool by its hash. The node finds a mined transaction only through its block |
| `storage_upload` | uploads a file from the files root, the CID shows up in `storage_transfer_status` |
| `storage_download` | downloads a CID into a new file under the files root, optionally over the mixnet |
| `storage_transfer_status` | progress and result of an upload or download the agent started |
| `storage_exists` | whether a CID is in local storage |
| `storage_manifests` | the content the agent put in local storage; anything else is only counted |
| `storage_remove` | removes content the agent uploaded or downloaded fresh. The rest of local storage is left alone |
| `storage_space` | local storage quota and usage |

Messaging and storage are asynchronous on Logos, so send and upload return an id and you poll the matching status tool.

Chat needs the `chat_module` installed. The chat address changes every time chat restarts, until Logos ships persistent chat accounts, so agents swap addresses again after a restart.

## Requirements

1. `logosctl` `0.3.2` => install from the [release page](https://github.com/logos-co/logos-logoscore-cli/releases/tag/0.3.2) (macOS arm64, Linux x86_64 and aarch64)
2. `delivery_module` => `logosctl package install delivery_module --yes`
3. `storage_module` => ships with `logosctl`
4. `chat_module` and `libp2p_module`, for chat and discovery => `logosctl package install chat_module --yes`, and the same for `libp2p_module`
5. `blockchain_module`, for the chain tools => `logosctl package install blockchain_module --yes`, with its node started. In Basecamp, the Blockchain app starts it
6. Rust `1.89` or newer, only to build from source

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
v=v0.9.0 target=aarch64-apple-darwin
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
| `--delivery-config` | `LOGOS_MCP_DELIVERY_CONFIG` | none | `delivery_module`'s full `createNode` config as JSON, in place of `--mode` and `--preset`, for a network no preset names, such as a local devnet hub. logos-mcp never logs it, but `delivery_module` may on a config it rejects, and it reaches `logosctl` as a command-line argument |
| `--room-topic` | `LOGOS_MCP_ROOM_TOPIC` | none | the Muster room that approves transfers over `--room-above-lepta`, see [Room approval](#room-approval). Needs the wallet flags and `--room-approvers` |
| `--room-approvers` | `LOGOS_MCP_ROOM_APPROVERS` | none | trusted people's Ed25519 keys as `ed:<hex>`, comma separated. Never the agent's own |
| `--room-approvals` | `LOGOS_MCP_ROOM_APPROVALS` | `1` | distinct trusted approvals a transfer needs |
| `--room-above-lepta` | `LOGOS_MCP_ROOM_ABOVE_LEPTA` | `0` | transfers strictly above this many lepta need the room. `0` sends every transfer to it |
| `--room-expires-mins` | `LOGOS_MCP_ROOM_EXPIRES_MINS` | `60` | minutes a room proposal waits before it expires unsent |
| `--timeout-secs` | | `60` | timeout for each `logosctl` call |

Give the agent its own wallet with a small balance, not the one you use. The agent can spend all of it, so the balance is its limit, unless you add [room approval](#room-approval). Create it once with `logosctl`, using a config that points at the LEZ sequencer:

```bash
logosctl call lez_core create_new str:$HOME/.logos-agent/wallet_config.json str:$HOME/.logos-agent/storage.json str:
logosctl call lez_core save
```

The reply carries the wallet's recovery phrase, so keep it somewhere safe.

Every LEZ transaction holds back a fee reserve of the wallet's `gas_limit` times the base fee. A `gas_limit` of about 200,000 in `wallet_config.json` keeps that near 1.7M lepta.

The `logos.dev` fleet publishes discovery peers (`libp2p_multiaddr` and `libp2p_peer_id`) at https://fleets.logos.co/data.json.

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
2. Permissions => a switch per tool group (messaging, storage, wallet, transfers, discovery, chain, chat). A group you turn off disappears from the agent's list and is refused if called. Basecamp runs one Chat module, so with chat on the agent uses your Logos Chat account: it reads and sends in every conversation you have there
3. Wallet => the accounts in your open wallet and their balances. Hand one of your public accounts to the agent, take it back, or create a new agent account. Accounts made for service jobs fold into one line with their total. Transfers waiting for you show here with Approve and Deny, and those waiting for the room with their approvals so far and a Cancel
4. Approvals => who approves each wallet's transfers, see [Room approval](#room-approval)
5. Activity => every tool call the agent made, also kept in `activity.jsonl` in the module's data folder. Message and chat text shows in the app only, the file keeps its size

## Networks

There are two hosted messaging networks.

| Preset | Sending | Notes |
| --- | --- | --- |
| `logos.test` | needs an active RLN membership | stable testnet, the target for app development |
| `logos.dev` | works out of the box | RLN is off. Redeployed often and may break |

On `logos.test` every message carries a rate-limiting proof, so your node needs an RLN membership registered on the LEZ testnet before it can send. Receiving works without one. Registering needs about 182.8M lepta (0.18 LEZ) held up front. Roughly 100M of that is a fee reserve that comes back after the registration. The [RLN guide](https://docs.logos.co/lez/rln/register-rln-membership-on-lez-with-delivery-node.md) has the steps.

Start on `logos.dev` if you just want to try it.

## Room approval

For each wallet, a limit splits transfers in two, and each side is one of:

1. Send => the agent sends it
2. Ask me => it waits for your Approve or Deny in the AI Agent app's Wallet tab
3. Room => it waits for trusted people to approve it in a [Muster](https://github.com/corpetty/muster) room

The side above the limit can't be looser than the one under it, and a limit of 0 puts every transfer above it. The agent wallet covers LEZ transfers and escrow payments. The node wallet covers Logos chain transfers, which spend your chain node's own wallet, so it asks you for every one unless you change it.

The room approves with the agent as its own member, never as you. The agent proposes and endorses from its own Muster identity, a headless `logosctl` session with `muster_module`, and you approve like any member.

### In Basecamp

Open the AI Agent app's Approvals tab. It needs `logosctl` installed.

1. Set up agent seat => the module makes a `logosctl` session in its data folder, installs Muster and the six modules it needs from Muster's catalogue (`https://raw.githubusercontent.com/corpetty/muster/main/catalog/logos-repo.json`), and runs it while the module runs
2. Copy the invite id and invite the agent into your room from Muster's app, then press Join next to the invite
3. Pick the room, tick who's trusted (you by default), set how many approvals are needed and the tiers, then Save

A change applies to the next transfer, and to room requests still waiting: an unticked person stops counting, and a different room or none cancels them.

### Standalone

Give the agent's `logosctl` session its own Muster:

```bash
logosctl -D -d
logosctl catalog add https://raw.githubusercontent.com/corpetty/muster/main/catalog/logos-repo.json
logosctl package install muster_module --no-optional -y
logosctl module load muster_module --no-optional
logosctl call muster_module coordinate_start_inbox
```

Invite it into your room from your own Muster: its `identity` call gives `ed25519` and `x25519`, and the invite id is the two joined. Accept with `logosctl call muster_module coordinate_join str:<topic>`. Then start logos-mcp with the room's topic and the Ed25519 keys of the people whose approval counts. Muster's `identity` shows yours as `ed25519`:

```bash
logos-mcp --config-dir ~/.logosctl-agent \
  --wallet-config ~/.logos-agent/wallet_config.json --wallet-storage ~/.logos-agent/storage.json \
  --room-topic /muster/1/my-room/proto \
  --room-approvers ed:0b00086682a0d169f90095b9d58e6c003ee591531ccf8b59c712c1cb70603269 \
  --room-above-lepta 3000000
```

The agent wallet then sends up to `--room-above-lepta` and asks the room above it. Chain transfers always ask the room, since there is no app to ask you in.

### How the room decides

A transfer for the room proposes the exact call logos-mcp will send, such as `lez_core.transfer_public(from, to, amount)`, an escrow's `lez_core.send_generic_public_transaction` or a chain transfer's `blockchain_module.wallet_transfer_funds`, and returns `awaiting_approval` with a `request_id`. Muster's card shows only the method, so the agent first posts what it does in the room's chat, e.g. `[logos-mcp] proposal 0x5a6c…: Pay 7000000 lepta from Public/4gbL… into escrow for Public/HvNb…`. Check that the line comes from the agent, then approve or decline the card in Muster.

logos-mcp sends the transfer once the agent's Muster shows the proposal executable, bound to this room and not expired, signed for exactly that call, and approved by enough distinct trusted people. The agent's own approval never counts. Neither does a signature copied in from another proposal, which Muster grades `unattested`. A decline, an expiry or a different call ends the request unsent. When it ends unsent for any reason but your decline, the agent declines its own proposal. Its approval then counts for nothing, and the card closes once the room can't reach Muster's threshold without it. An escrow payment waits no longer than its own deadline, and one with less than a minute left is refused.

Keep in mind:

- the agent must have no shell on its Muster's `logosctl` session. With one it could call Muster itself
- the limit is per transfer, so the agent can still send several transfers under it
- a restart drops waiting requests, and nothing dropped is ever sent. The first room proposal after a restart declines the agent's own proposals they left collecting in Muster
- run one logos-mcp per agent Muster. A second one would decline the first one's proposals when it starts proposing
- the first proposal waits up to 90 s for Muster's delivery node, because Muster loses sends made while it starts ([corpetty/muster#373](https://github.com/corpetty/muster/issues/373))
- inside Basecamp the agent's seat is a second Logos runtime next to Basecamp's, since one Basecamp has one Muster identity ([corpetty/muster#376](https://github.com/corpetty/muster/issues/376))

## Security

The server is the boundary between your agent and your node, so it checks what the agent asks for:

- storage tools only see `--files-root`. Absolute paths, `..` and symlinks that point outside are refused, and a download never overwrites an existing file. Without `--files-root` file access is off
- the network and node mode are fixed when you start the server. The agent can't change them
- messages are limited to 150 KiB (the network limit) and topics must look like `/app/1/name/proto`
- there is no tool that calls arbitrary module methods
- `wallet_tx_status` is the one tool that skips `logosctl`: it asks the sequencer in the wallet config directly, because `lez_core` only has a lookup that blocks for minutes
- `chain_transfer` spends the chain node's wallet, which is yours, so each one waits for your Approve unless you set its tiers otherwise. Outside Basecamp only a room can approve it
- room approval never uses your own Muster: proposals come only from the agent's own seat, and without one they're refused
- with room approval, `wallet_transfer` over the limit sends only what trusted people approved in the room, read from the agent's own Muster. No tool approves anything
- `wallet_faucet` only pays into the agent's own public accounts
- `storage_remove` only removes content the agent put in local storage, recorded next to the wallet, never the rest
- `wallet_transfer` only sends from the agent wallet: the one this server opened, or the configured wallet found already open, with exactly its accounts on its sequencer. If `lez_core` has another wallet open, it refuses
- inside Basecamp, `wallet_transfer` only sends from the agent accounts, and the MCP endpoint needs the bearer token. Requests from web pages (with an `Origin` header) or for a host other than localhost are refused
- only the AI Agent app can read the token, flip the tool switches or read the activity through the module
- `lez_core` `0.5.1` can only open unencrypted key storage, so lock the wallet folder down (`chmod 700`)

Uploaded files are announced to the network and anyone with the CID can fetch them. Don't put anything in the files root you wouldn't publish.

Some modules in the official catalogue are unsigned today, `lez_core` and `libp2p_module` among them. `logosctl` installs them with a warning only. Keep that in mind before you give an agent anything with value.

## Known limitations

- messages that arrive while an event watcher restarts are lost, and `messaging_read` only sees messages received since the server started
- only one `logosctl` session per machine can run storage, because they share `~/.logos_storage`
- detecting a node that was never created relies on the module's error text
- not tested on Linux yet, CI only builds and unit tests there
- with `libp2p_module` `1.1.0` a node is only found for the first service it advertises, even after that one stops, so advertise one service per node. Restarting the libp2p module clears it

## Use it as a library

logos-agents builds on this crate. `standalone::server(app, args)` builds the server the binary runs without serving it. `LogosServer::upload_file` and `download_to` run a storage transfer to the end, under the same files-root rules as the tools, and `download_to` takes a size cap. `files_root().write_new` writes bytes into a new file there.

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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
