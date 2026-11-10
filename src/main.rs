use logos_mcp::standalone::{App, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(
        App {
            name: "logos-mcp",
            version: env!("CARGO_PKG_VERSION"),
            about: "MCP server that lets AI agents use a running Logos node through logosctl.",
            log_filter: "logos_mcp=info",
        },
        |server| server,
    )
    .await
}
