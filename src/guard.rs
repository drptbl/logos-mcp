//! Input checks that form this server's security boundary: what an agent may
//! publish, and which local files it may hand to (or receive from) the network.

use std::{
    io,
    path::{Component, Path, PathBuf},
};

/// Max message size on both hosted fleets (logos-delivery-module docs/networks.md).
pub const MAX_PAYLOAD_BYTES: usize = 150 * 1024;

/// A content topic: `/{app}/{version}/{name}/{encoding}`, optionally prefixed
/// with a generation segment (`/0/app/...`).
pub fn validate_topic(topic: &str) -> Result<(), String> {
    if topic.len() > 256 {
        return Err("content topic is longer than 256 characters".into());
    }
    if topic.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("content topic must not contain whitespace".into());
    }
    let Some(rest) = topic.strip_prefix('/') else {
        return Err("content topic must start with '/', e.g. /my-app/1/chat/proto".into());
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if !(parts.len() == 4 || parts.len() == 5) || parts.iter().any(|p| p.is_empty()) {
        return Err("content topic must look like /{app}/{version}/{name}/{encoding}".into());
    }
    Ok(())
}

pub fn validate_payload(payload: &[u8]) -> Result<(), String> {
    if payload.is_empty() {
        return Err("payload is empty".into());
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "payload is {} bytes; the network limit is {MAX_PAYLOAD_BYTES}",
            payload.len()
        ));
    }
    Ok(())
}

/// A content id as Logos Storage prints it (base58/base32 multibase).
pub fn validate_cid(cid: &str) -> Result<(), String> {
    if cid.is_empty() || cid.len() > 128 || !cid.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("not a valid content id".into());
    }
    Ok(())
}

/// The one directory storage tools may read from and write into.
#[derive(Debug, Clone)]
pub struct FilesRoot {
    root: PathBuf,
}

impl FilesRoot {
    pub fn new(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "files root is not a directory",
            ));
        }
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Only plain relative paths: no absolute paths, no `..`.
    fn relative(user: &str) -> Result<&Path, String> {
        let p = Path::new(user);
        if user.is_empty() || p.is_absolute() {
            return Err("give a path relative to the files root".into());
        }
        if p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err("path must not contain '..'".into());
        }
        Ok(p)
    }

    /// An existing regular file inside the root, after resolving symlinks.
    pub fn existing_file(&self, user: &str) -> Result<PathBuf, String> {
        let rel = Self::relative(user)?;
        let full = self
            .root
            .join(rel)
            .canonicalize()
            .map_err(|e| format!("cannot open {user}: {e}"))?;
        if !full.starts_with(&self.root) {
            return Err("path resolves outside the files root".into());
        }
        if !full.is_file() {
            return Err(format!("{user} is not a regular file"));
        }
        Ok(full)
    }

    /// A not-yet-existing file whose parent directory is inside the root.
    /// Existing targets are refused so a download never overwrites a file.
    pub fn new_file(&self, user: &str) -> Result<PathBuf, String> {
        let rel = Self::relative(user)?;
        let name = rel.file_name().ok_or("path must name a file")?;
        let parent = self
            .root
            .join(rel.parent().unwrap_or(Path::new("")))
            .canonicalize()
            .map_err(|e| format!("parent directory of {user} is not usable: {e}"))?;
        if !parent.starts_with(&self.root) {
            return Err("path resolves outside the files root".into());
        }
        let full = parent.join(name);
        if full.symlink_metadata().is_ok() {
            return Err(format!("{user} already exists; choose a new file name"));
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
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
}
