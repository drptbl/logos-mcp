//! When the agent's own wallet is already open in lez_core, after a restart or
//! because a devnet session opened it, the server adopts it; anyone else's stays
//! untrusted.

use super::*;

const AGENT_HEX: &str = "e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f";
const SEQUENCER: &str = "https://testnet.lez.logos.co/";

/// A dedicated-mode server whose configured wallet files sit in `dir`, holding
/// `accounts` (hex ids) and pointing at `sequencer`.
fn configured_server(
    fake: Arc<FakeRunner>,
    dir: &std::path::Path,
    accounts: &[&str],
    sequencer: &str,
) -> LogosServer {
    let entries: Vec<Value> = accounts
        .iter()
        .map(|hex| {
            json!({ "Public": {
                "account_id": guard::account_id_base58(hex).unwrap(),
                "data": { "sk": "not a real key" },
            }})
        })
        .collect();
    let storage = dir.join("storage.json");
    let config = dir.join("wallet_config.json");
    std::fs::write(
        &storage,
        json!({ "key_chain": { "accounts": entries } }).to_string(),
    )
    .unwrap();
    std::fs::write(&config, json!({ "sequencer_addr": sequencer }).to_string()).unwrap();
    let mut s = wallet_server(fake);
    s.settings.wallet = Some(WalletMode::Dedicated(WalletPaths { config, storage }));
    s
}

/// lez_core already has a wallet open with `accounts` on `sequencer`.
fn open_wallet(sequencer: &str, accounts: &str) -> Vec<crate::logosctl::RawOutput> {
    vec![
        lez("get_sequencer_addr", &format!("{sequencer:?}")),
        lez("list_accounts", accounts),
        lez("get_current_block_height", "80205"),
        lez("get_last_synced_block", "0"),
    ]
}

/// The configured wallet already open: its accounts match, so the server reads
/// the sequencer once more to be sure both answers came from the same wallet.
fn configured_open(sequencer: &str, accounts: &str) -> Vec<crate::logosctl::RawOutput> {
    let mut replies = open_wallet(sequencer, accounts);
    replies.insert(2, lez("get_sequencer_addr", &format!("{sequencer:?}")));
    replies
}

#[tokio::test]
async fn the_configured_wallet_found_open_is_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(configured_open(SEQUENCER, AGENT_ACCOUNTS));
    let s = configured_server(fake.clone(), dir.path(), &[AGENT_HEX], SEQUENCER);
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], true, "{b}");
    assert_eq!(b["warning"], Value::Null, "{b}");
    assert!(
        !fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.get(3).map(String::as_str) == Some("open")),
        "the open wallet is used as it is"
    );
}

#[tokio::test]
async fn a_trailing_slash_does_not_make_it_another_sequencer() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(configured_open(SEQUENCER, AGENT_ACCOUNTS));
    let s = configured_server(
        fake,
        dir.path(),
        &[AGENT_HEX],
        "https://testnet.lez.logos.co",
    );
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], true, "{b}");
}

#[tokio::test]
async fn the_same_accounts_on_another_sequencer_stay_untrusted() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(open_wallet("http://127.0.0.1:3040/", AGENT_ACCOUNTS));
    let s = configured_server(fake, dir.path(), &[AGENT_HEX], SEQUENCER);
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], false, "{b}");
}

#[tokio::test]
async fn an_open_wallet_with_other_or_extra_accounts_stays_untrusted() {
    let both = format!(
        r#"[{{"account_id":"{AGENT_HEX}","is_public":true}},{{"account_id":"846d9a7014dbd1fd26ffc40e870df5af08f23ee2bf0c151bc2aa17a58b4481bc","is_public":true}}]"#
    );
    for accounts in [OTHER_ACCOUNTS, both.as_str(), "[]"] {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::with(open_wallet(SEQUENCER, accounts));
        let s = configured_server(fake, dir.path(), &[AGENT_HEX], SEQUENCER);
        let b = body(&s.wallet_status().await.unwrap());
        assert_eq!(b["opened_by_this_server"], false, "{accounts}: {b}");
    }
}

#[tokio::test]
async fn an_unreadable_configured_wallet_leaves_an_open_one_untrusted() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeRunner::with(open_wallet(SEQUENCER, AGENT_ACCOUNTS));
    let s = configured_server(fake, dir.path(), &[AGENT_HEX], SEQUENCER);
    std::fs::write(dir.path().join("storage.json"), "not json").unwrap();
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], false, "{b}");
}

#[tokio::test]
async fn private_accounts_in_the_configured_wallet_count_too() {
    let dir = tempfile::tempdir().unwrap();
    let private = "8cfb92d8780a7aa72598218a8711d693a0685fec99a790f87f5141b48954fd25";
    let s_dir = dir.path();
    let storage = s_dir.join("storage.json");
    let config = s_dir.join("wallet_config.json");
    std::fs::write(
        &storage,
        json!({ "key_chain": { "accounts": [
            { "Public": { "account_id": guard::account_id_base58(AGENT_HEX).unwrap() } },
            { "Private": { "account_id": guard::account_id_base58(private).unwrap() } },
        ]}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(&config, json!({ "sequencer_addr": SEQUENCER }).to_string()).unwrap();
    let listed = format!(
        r#"[{{"account_id":"{AGENT_HEX}","is_public":true}},{{"account_id":"{private}","is_public":false}}]"#
    );
    let mut s = wallet_server(FakeRunner::with(configured_open(SEQUENCER, &listed)));
    s.settings.wallet = Some(WalletMode::Dedicated(WalletPaths { config, storage }));
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], true, "{b}");
}

#[tokio::test]
async fn a_wallet_swapped_while_it_was_being_checked_is_not_adopted() {
    // The first sequencer read came from the configured wallet's chain; by the
    // second, another wallet with the same accounts on another chain is open.
    let dir = tempfile::tempdir().unwrap();
    let mut replies = open_wallet(SEQUENCER, AGENT_ACCOUNTS);
    replies.insert(2, lez("get_sequencer_addr", r#""http://127.0.0.1:3040/""#));
    let s = configured_server(
        FakeRunner::with(replies),
        dir.path(),
        &[AGENT_HEX],
        SEQUENCER,
    );
    let b = body(&s.wallet_status().await.unwrap());
    assert_eq!(b["opened_by_this_server"], false, "{b}");
}

#[tokio::test]
async fn an_adopted_wallet_swapped_later_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let mut replies = configured_open(SEQUENCER, AGENT_ACCOUNTS);
    // later someone else's wallet is open instead
    replies.extend([
        lez("get_sequencer_addr", &format!("{SEQUENCER:?}")),
        lez("list_accounts", OTHER_ACCOUNTS),
        lez("get_balance", r#""5""#),
    ]);
    let s = configured_server(
        FakeRunner::with(replies),
        dir.path(),
        &[AGENT_HEX],
        SEQUENCER,
    );
    assert_eq!(
        body(&s.wallet_status().await.unwrap())["opened_by_this_server"],
        true
    );
    let later = body(&s.wallet_accounts().await.unwrap());
    assert!(
        later["warning"]
            .as_str()
            .unwrap()
            .contains("may not be the agent wallet"),
        "{later}"
    );
}

#[tokio::test]
async fn the_same_accounts_later_open_on_another_sequencer_are_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let mut replies = configured_open(SEQUENCER, AGENT_ACCOUNTS);
    // later the same keys are open against another chain
    replies.extend([
        lez("get_sequencer_addr", r#""http://127.0.0.1:3040/""#),
        lez("list_accounts", AGENT_ACCOUNTS),
        lez("get_balance", r#""5""#),
    ]);
    let s = configured_server(
        FakeRunner::with(replies),
        dir.path(),
        &[AGENT_HEX],
        SEQUENCER,
    );
    assert_eq!(
        body(&s.wallet_status().await.unwrap())["opened_by_this_server"],
        true
    );
    let later = body(&s.wallet_accounts().await.unwrap());
    assert!(
        later["warning"]
            .as_str()
            .unwrap()
            .contains("may not be the agent wallet"),
        "{later}"
    );
}
