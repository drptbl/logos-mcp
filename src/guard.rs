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

/// A 32-byte LEZ account id (64 hex chars) in the Bitcoin base58 alphabet that
/// `Public/<base58>` addresses use. `None` for anything that isn't such an id.
pub fn account_id_base58(hex: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if hex.len() != 64 {
        return None;
    }
    let bytes = (0..32)
        .map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    // Repeated division by 58 over the big-endian number, least significant digit first.
    let mut digits: Vec<u8> = Vec::new();
    for &byte in &bytes {
        let mut carry = u32::from(byte);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let encoded = std::iter::repeat_n('1', zeros)
        .chain(digits.iter().rev().map(|&d| ALPHABET[d as usize] as char))
        .collect();
    Some(encoded)
}

/// The 64-char lowercase hex id of a public LEZ account given as
/// `Public/<base58>` or as hex. Private accounts are refused.
pub fn public_account_hex(input: &str) -> Result<String, String> {
    if input.len() == 64 && input.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(input.to_ascii_lowercase());
    }
    let Some(b58) = input.strip_prefix("Public/") else {
        return Err("give a public account as Public/<base58> or as 64 hex characters".into());
    };
    const ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    // Repeated multiply-by-58 into a big-endian byte buffer, least significant byte first.
    let mut bytes: Vec<u8> = Vec::new();
    for c in b58.chars() {
        let mut carry = ALPHABET
            .find(c)
            .ok_or("account address is not valid base58")? as u32;
        for b in bytes.iter_mut() {
            carry += u32::from(*b) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let zeros = b58.chars().take_while(|&c| c == '1').count();
    bytes.extend(std::iter::repeat_n(0, zeros));
    if bytes.len() != 32 {
        return Err("account address must decode to 32 bytes".into());
    }
    Ok(bytes.iter().rev().map(|b| format!("{b:02x}")).collect())
}

/// A URL as it may be shown to an agent: no user info, query or fragment, where
/// a credential could sit.
pub fn shown_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    // An '@' after a ':' but past the first '/' may be credentials whose password
    // holds an unencoded '/': show nothing of the address then.
    if path.contains('@') && rest.rfind('@').is_some_and(|at| rest[..at].contains(':')) {
        return format!("{scheme}://<address hidden>");
    }
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

/// An account as `Public/<base58>`, `Private/<base58>` or 64 hex characters
/// (public): its id in hex, and whether it is private.
pub fn account_hex(input: &str) -> Result<(String, bool), String> {
    match input.strip_prefix("Private/") {
        Some(b58) => public_account_hex(&format!("Public/{b58}"))
            .map(|hex| (hex, true))
            .map_err(|_| "a private account is Private/<base58>".to_string()),
        None => public_account_hex(input).map(|hex| (hex, false)),
    }
}

/// A positive amount in lepta, given as a decimal string, encoded the way
/// `lez_core` takes it: 16 bytes little-endian as 32 hex characters.
pub fn amount_le16_hex(lepta: &str) -> Result<String, String> {
    if lepta.is_empty() || !lepta.chars().all(|c| c.is_ascii_digit()) {
        return Err("amount must be a whole number of lepta, as digits".into());
    }
    let amount: u128 = lepta.parse().map_err(|_| "amount is too large")?;
    if amount == 0 {
        return Err("amount must be more than 0".into());
    }
    Ok(amount
        .to_le_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Whether `peer` looks like a libp2p peer id: a base58 multihash (12D3KooW…,
/// 16Uiu2…, Qm…) of 40 to 60 characters.
pub fn is_peer_id(peer: &str) -> bool {
    const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    (40..=60).contains(&peer.len()) && peer.chars().all(|c| BASE58.contains(c))
}

/// A `/.../p2p/<peerId>` multiaddr as libp2p_module's `{peerId, addrs}` bootstrap entry.
pub fn bootstrap_node(multiaddr: &str) -> Result<serde_json::Value, String> {
    let bad =
        || format!("bootstrap peer must be a multiaddr ending in /p2p/<peer id>: {multiaddr:?}");
    let (addr, peer) = multiaddr.rsplit_once("/p2p/").ok_or_else(bad)?;
    if !addr.starts_with('/') || addr.len() < 2 || !is_peer_id(peer) {
        return Err(bad());
    }
    Ok(serde_json::json!({ "peerId": peer, "addrs": [addr] }))
}

/// A service id to advertise or look up: short, printable, no whitespace.
pub fn validate_service_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(
            "service id must be 1 to 128 characters with no whitespace, e.g. my-app/summarize/v1"
                .into(),
        );
    }
    Ok(())
}

/// Advertisement data limit in libp2p_module's service discovery (from its error text).
pub const MAX_AD_TAG_BYTES: usize = 33;

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

    /// Write `bytes` into a new file under the root, as `new_file` places it.
    /// Created exclusively, so it never overwrites or writes through a link.
    pub fn write_new(&self, user: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        use std::io::Write as _;
        let path = self.new_file(user)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot create {user}: {e}"))?;
        if let Err(e) = file.write_all(bytes) {
            // Never leave a half-written file behind.
            let _ = std::fs::remove_file(&path);
            return Err(format!("cannot write {user}: {e}"));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests;

/// Write a file readable and writable by this user only. Written beside it and
/// renamed into place, so a crash leaves the old contents or the new, never half.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover temp file (or a link planted there) is replaced, never written through.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// A JSON map kept in a file next to the wallet: empty when there is no file
/// yet, refused when there is one that can't be read, so a caller never saves
/// over records it couldn't see.
pub fn read_side_map<T: serde::de::DeserializeOwned>(
    file: Option<PathBuf>,
) -> Result<std::collections::BTreeMap<String, T>, String> {
    match file.map(std::fs::read) {
        None => Ok(std::collections::BTreeMap::new()),
        Some(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
            Ok(std::collections::BTreeMap::new())
        }
        Some(Err(e)) => Err(e.to_string()),
        Some(Ok(bytes)) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
    }
}
