# logos-mcp

MCP server that lets AI agents use a running [Logos](https://logos.co/) node. Your agent can send and receive messages over Logos Messaging, store and fetch files on Logos Storage and check node health, from Claude Code, Codex or any other MCP client.

It talks to the node through `logosctl`, the official headless Logos runtime, so it works with the same modules and session as your node.

Early release. Logos itself is on testnet (`v0.3`) and is not meant for real funds yet. Tested on macOS (Apple Silicon) with `logosctl` `0.3.2`, `delivery_module` `0.3.3` and `storage_module` `3.0.2`.

## Tools

| Tool | What it does |
| --- | --- |
| `node_status` | daemon health, loaded modules, messaging connection and server settings |
| `messaging_connect` | creates and starts the messaging node when none exists. Leaves an existing node as it is |
| `messaging_subscribe` / `messaging_unsubscribe` | follow or stop following a content topic |
| `messaging_send` | publishes text or base64 bytes to a topic and returns a `request_id` |
| `messaging_send_status` | queued, sent, propagated or error for a `request_id` |
| `messaging_read` | messages received on a subscribed topic, paged with `after_seq` |
| `storage_upload` | uploads a file from the files root, the CID shows up in `storage_transfer_status` |
| `storage_download` | downloads a CID into a new file under the files root, optionally over the mixnet |
| `storage_transfer_status` | progress and result of an upload or download |
| `storage_exists` | whether a CID is in local storage |
| `storage_space` | local storage quota and usage |

Messaging and storage are asynchronous on Logos, so send and upload return an id and you poll the matching status tool.

## Requirements

1. `logosctl` `0.3.2` => install from the [release page](https://github.com/logos-co/logos-logoscore-cli/releases/tag/0.3.2) (macOS arm64, Linux x86_64 and aarch64)
2. `delivery_module` => `logosctl package install delivery_module --yes`
3. `storage_module` => ships with `logosctl`
4. Rust `1.89` or newer to build

Start the daemon before you start the server:

```bash
logosctl daemon start --detach
logosctl catalog refresh
logosctl package install delivery_module --yes
```

## Build

```bash
git clone https://github.com/drptbl/logos-mcp
cd logos-mcp
cargo build --release
```

The binary is `target/release/logos-mcp`.

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
| `--timeout-secs` | | `60` | timeout for each `logosctl` call |

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
- there is no tool that calls arbitrary module methods, and no wallet tools yet

Uploaded files are announced to the network and anyone with the CID can fetch them. Don't put anything in the files root you wouldn't publish.

Some modules in the official catalogue are unsigned today, `lez_core` and `libp2p_module` among them. `logosctl` installs them with a warning only. Keep that in mind before you give an agent anything with value.

## Known limitations

- messages that arrive while an event watcher restarts are lost, and `messaging_read` only sees messages received since the server started
- only one `logosctl` session per machine can run storage, because they share `~/.logos_storage`
- detecting a node that was never created relies on the module's error text
- not tested on Linux yet, CI only builds and unit tests there

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
