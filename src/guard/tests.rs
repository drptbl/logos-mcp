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
