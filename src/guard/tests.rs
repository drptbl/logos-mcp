use super::*;
use std::fs;

#[test]
fn accepts_well_formed_topics() {
    assert!(validate_topic("/logos-mcp/1/smoke/proto").is_ok());
    assert!(validate_topic("/0/logos-mcp/1/smoke/proto").is_ok());
}

#[test]
fn rejects_malformed_topics() {
    for bad in [
        "",
        "logos/1/a/proto",
        "/a/1/proto",
        "/a//b/proto",
        "/a/1/b c/proto",
        "/a/1/b/c/d/e",
    ] {
        assert!(validate_topic(bad).is_err(), "{bad:?} should be rejected");
    }
    assert!(validate_topic(&format!("/{}/1/a/proto", "x".repeat(300))).is_err());
}

#[test]
fn enforces_payload_bounds() {
    assert!(validate_payload(b"").is_err());
    assert!(validate_payload(&vec![0u8; MAX_PAYLOAD_BYTES]).is_ok());
    assert!(validate_payload(&vec![0u8; MAX_PAYLOAD_BYTES + 1]).is_err());
}

#[test]
fn validates_cids() {
    assert!(validate_cid("zDvZRwzkxN3WEyVGhqJrvvDoXnVy6AphUSNX7e3VDCkpBkFMoJQP").is_ok());
    assert!(validate_cid("../etc").is_err());
    assert!(validate_cid("").is_err());
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, FilesRoot) {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("sub/ok.txt"), "ok").unwrap();
    fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    let files = FilesRoot::new(root.path()).unwrap();
    (root, outside, files)
}

#[test]
fn existing_file_inside_root_is_allowed() {
    let (_r, _o, files) = setup();
    let p = files.existing_file("sub/ok.txt").unwrap();
    assert!(p.starts_with(files.path()));
}

#[test]
fn traversal_and_absolute_paths_are_refused() {
    let (_r, outside, files) = setup();
    assert!(files.existing_file("../secret.txt").is_err());
    assert!(files.existing_file("sub/../../secret.txt").is_err());
    let abs = outside.path().join("secret.txt");
    assert!(files.existing_file(abs.to_str().unwrap()).is_err());
    assert!(files.new_file(abs.to_str().unwrap()).is_err());
    assert!(files.new_file("../x.bin").is_err());
}

#[test]
fn symlink_escapes_are_refused() {
    let (root, outside, files) = setup();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        root.path().join("link.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("outdir")).unwrap();
    assert!(
        files.existing_file("link.txt").is_err(),
        "file symlink pointing outside"
    );
    assert!(
        files.existing_file("outdir/secret.txt").is_err(),
        "directory symlink pointing outside"
    );
    assert!(
        files.new_file("outdir/new.bin").is_err(),
        "write through directory symlink"
    );
}

#[test]
fn new_file_refuses_existing_targets_and_directories() {
    let (_r, _o, files) = setup();
    assert!(files.new_file("sub/ok.txt").is_err());
    assert!(files.existing_file("sub").is_err());
    let p = files.new_file("sub/fresh.bin").unwrap();
    assert!(p.starts_with(files.path()) && p.ends_with("fresh.bin"));
}

#[test]
fn encodes_lez_account_ids_as_base58() {
    // Expected values from lez_core's own account_id_to_base58 and an independent encoder.
    let cases = [
        (
            "e9ce8b81376428e3fb1a8192026027ac25dc68bfafb100c82e6107f068c8213f",
            "GjgfTEYoYVFjomLH7zRzFHHGpLNEwD4iGtf53JhwSdMY",
        ),
        (
            "8126abb4eb9f0a26919bf51a57244a6b7d3a39811e6393c1885f9e07aa32fec1",
            "9h9o2fwbcu7Mbuq8hzVf1UVDpMQpCdCr5ZnNVo3vTsjN",
        ),
        (
            "846d9a7014dbd1fd26ffc40e870df5af08f23ee2bf0c151bc2aa17a58b4481bc",
            "9uwkVJuXScDwLKVtQMcHFU9Hvb3uZjiEDVvDv4PYjCfM",
        ),
    ];
    for (hex, b58) in cases {
        assert_eq!(account_id_base58(hex).as_deref(), Some(b58), "{hex}");
    }
    assert_eq!(
        account_id_base58("00").as_deref(),
        None,
        "ids are exactly 32 bytes"
    );
    assert_eq!(account_id_base58(&"zz".repeat(32)).as_deref(), None);
    let zero_led = format!("00{}", "11".repeat(31));
    assert!(
        account_id_base58(&zero_led).unwrap().starts_with('1'),
        "leading zero bytes become '1'"
    );
}

#[test]
fn parses_public_recipients_to_account_hex() {
    let hex = "846d9a7014dbd1fd26ffc40e870df5af08f23ee2bf0c151bc2aa17a58b4481bc";
    assert_eq!(
        public_account_hex("Public/9uwkVJuXScDwLKVtQMcHFU9Hvb3uZjiEDVvDv4PYjCfM").as_deref(),
        Ok(hex)
    );
    assert_eq!(
        public_account_hex(hex).as_deref(),
        Ok(hex),
        "hex ids pass through"
    );
    assert_eq!(
        public_account_hex(&hex.to_uppercase()).as_deref(),
        Ok(hex),
        "hex is normalised to lowercase"
    );
    assert!(public_account_hex("Private/9uwkVJuXScDwLKVtQMcHFU9Hvb3uZjiEDVvDv4PYjCfM").is_err());
    assert!(public_account_hex("Public/0OIl").is_err(), "not base58");
    assert!(public_account_hex("Public/9uwk").is_err(), "not 32 bytes");
    assert!(public_account_hex("").is_err());
}

#[test]
fn encodes_amounts_as_16_byte_little_endian_hex() {
    // 100,000,000 lepta (one faucet claim) = 0x05f5e100
    assert_eq!(
        amount_le16_hex("100000000").as_deref(),
        Ok("00e1f505000000000000000000000000")
    );
    assert_eq!(
        amount_le16_hex("1").as_deref(),
        Ok("01000000000000000000000000000000")
    );
    assert_eq!(
        amount_le16_hex("340282366920938463463374607431768211455").as_deref(),
        Ok("ffffffffffffffffffffffffffffffff"),
        "u128::MAX"
    );
    for bad in [
        "0",
        "",
        "-5",
        "1.5",
        "1e9",
        " 10",
        "340282366920938463463374607431768211456",
    ] {
        assert!(amount_le16_hex(bad).is_err(), "{bad:?} should be refused");
    }
}

#[test]
fn splits_bootstrap_multiaddrs_into_libp2p_entries() {
    let node = bootstrap_node(
        "/ip4/138.68.122.137/tcp/30304/p2p/16Uiu2HAm1YeKNp72mcWY3GD2FdkGLk85XVBHMpuGooPNEuzW5Ajc",
    )
    .unwrap();
    assert_eq!(
        node["peerId"],
        "16Uiu2HAm1YeKNp72mcWY3GD2FdkGLk85XVBHMpuGooPNEuzW5Ajc"
    );
    assert_eq!(node["addrs"][0], "/ip4/138.68.122.137/tcp/30304");
    for bad in [
        "/ip4/1.2.3.4/tcp/30304",
        "/p2p/16Uiu2",
        "",
        "/ip4/1.2.3.4/tcp/1/p2p/",
        "garbage/p2p/not-a-peer",
        "/ip4/1.2.3.4/tcp/1/p2p/0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl",
    ] {
        assert!(bootstrap_node(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_shown_url_drops_credentials_query_and_fragment() {
    assert_eq!(
        shown_url("https://user:secret@seq.example:8443/rpc?token=abc#x"),
        "https://seq.example:8443/rpc"
    );
    assert_eq!(
        shown_url("https://testnet.lez.logos.co/"),
        "https://testnet.lez.logos.co/"
    );
    assert_eq!(shown_url("not a url"), "not a url");
    // An @ in the path is not a credential.
    assert_eq!(shown_url("http://host/a@b"), "http://host/a@b");
}

#[test]
fn a_url_whose_credentials_hold_a_slash_is_hidden_whole() {
    // A password with an unencoded '/' would otherwise read as the start of the path.
    let shown = shown_url("https://user:pa/ss@seq.example/rpc");
    assert!(
        !shown.contains("pa/ss") && !shown.contains("ss@"),
        "{shown}"
    );
    assert!(shown.starts_with("https://"), "{shown}");
}
