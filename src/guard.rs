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
mod tests;
