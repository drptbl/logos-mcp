//! Reading a node's network from its ENR (EIP-778): which cluster its `rs` or
//! `rsv` (relay shards) entry names, so a report can say where the node really is.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// The networks logos-delivery's presets put on each cluster
/// (`logos_delivery/waku/factory/networks_config.nim`).
const CLUSTERS: [(u16, &str); 3] = [(1, "twn"), (2, "logos.test"), (3, "logos.dev")];

/// The cluster id in an `enr:` record, if it is a record with an `rs` or `rsv` entry.
pub fn cluster(enr: &str) -> Option<u16> {
    let raw = URL_SAFE_NO_PAD
        .decode(enr.strip_prefix("enr:")?.trim_end_matches('='))
        .ok()?;
    let (true, mut fields, _) = item(&raw)? else {
        return None;
    };
    // Signature and sequence number come first, then key/value pairs.
    for _ in 0..2 {
        fields = item(fields)?.2;
    }
    while !fields.is_empty() {
        let (_, key, rest) = item(fields)?;
        let (_, value, rest) = item(rest)?;
        // `rs` lists the shards, `rsv` maps them as bits: both start with the cluster.
        if key == b"rs" || key == b"rsv" {
            return Some(u16::from_be_bytes([*value.first()?, *value.get(1)?]));
        }
        fields = rest;
    }
    None
}

/// The network a record belongs to: a preset name, else its cluster, else "unknown".
pub fn network_name(enr: &str) -> String {
    match cluster(enr) {
        Some(id) => CLUSTERS
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("cluster {id}")),
        None => "unknown".to_string(),
    }
}

/// One RLP item: whether it is a list, its payload, and what follows it.
fn item(bytes: &[u8]) -> Option<(bool, &[u8], &[u8])> {
    let (&first, rest) = bytes.split_first()?;
    let (list, len, rest) = match first {
        0x00..=0x7f => return Some((false, &bytes[..1], rest)),
        0x80..=0xb7 => (false, usize::from(first - 0x80), rest),
        0xb8..=0xbf => long(false, usize::from(first - 0xb7), rest)?,
        0xc0..=0xf7 => (true, usize::from(first - 0xc0), rest),
        0xf8..=0xff => long(true, usize::from(first - 0xf7), rest)?,
    };
    (rest.len() >= len).then(|| (list, &rest[..len], &rest[len..]))
}

/// A length given in the next `size` bytes.
fn long(list: bool, size: usize, bytes: &[u8]) -> Option<(bool, usize, &[u8])> {
    if size > std::mem::size_of::<usize>() || bytes.len() < size {
        return None;
    }
    let len = bytes[..size]
        .iter()
        .fold(0usize, |n, &b| (n << 8) | usize::from(b));
    Some((list, len, &bytes[size..]))
}

#[cfg(test)]
mod tests;
