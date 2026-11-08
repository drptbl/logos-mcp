use super::*;

// Recorded from delivery_module's getNodeInfo("MyENR") on each network.
const LOGOS_DEV: &str = "enr:-K24QN6sIVvTUFyY43EG8XDgzmk7yJ47RSCBDlMiH6U7sEZwCpygNxlsH9mlin9P1WSbHaHSHqiunbDNe6bahLsR3HtKgmlkgnY0im11bHRpYWRkcnOAgnJzkwADCAAAAAEAAgADAAQABQAGAAeJc2VjcDI1NmsxoQM0rGKBJR2xGVsXh09fXRXCX0wISE-HgZUqCHvqrNWU7IN0Y3CCweuDdWRwgsbihXdha3UyAA";
const LOGOS_TEST: &str = "enr:-K-4QEtqScAIVZA-pAT0BE-v7OBbpDwzI16aV_Q76rm5MLoJfxtBckAf8EFtI1E9KEo53Rgkxk_YVVj1Tm1a5PxXE7WCBcGCaWSCdjSKbXVsdGlhZGRyc4CCcnOTAAIIAAAAAQACAAMABAAFAAYAB4lzZWNwMjU2azGhAgcPu3b-Y0n-8xrgdBw56URSEdLXrcZCqfIALawUf45yg3RjcILmMIN1ZHCC1_aFd2FrdTIN";

#[test]
fn reads_the_cluster_a_node_records() {
    assert_eq!(cluster(LOGOS_DEV), Some(3));
    assert_eq!(cluster(LOGOS_TEST), Some(2));
}

#[test]
fn names_the_logos_networks_by_cluster() {
    assert_eq!(network_name(LOGOS_DEV), "logos.dev");
    assert_eq!(network_name(LOGOS_TEST), "logos.test");
}

#[test]
fn anything_else_is_not_a_cluster() {
    for bad in [
        "",
        "enr:",
        "enr:!!!",
        "not-an-enr",
        &LOGOS_DEV[..40],
        // Valid base64 that is no RLP list.
        "enr:AAAA",
    ] {
        assert_eq!(cluster(bad), None, "{bad}");
    }
}

#[test]
fn reads_the_cluster_from_the_shard_bitmap_form_too() {
    // [signature, seq, "rsv", cluster 2 + bitmap]: a node on many shards
    // advertises them as a bitmap under `rsv` instead of a list under `rs`.
    let mut value = vec![0x00, 0x02];
    value.extend([0u8; 128]);
    let mut fields = vec![0x80, 0x01, 0x83, b'r', b's', b'v', 0xb8, value.len() as u8];
    fields.extend(&value);
    let mut record = vec![0xf8, fields.len() as u8];
    record.extend(fields);
    let enr = format!("enr:{}", URL_SAFE_NO_PAD.encode(record));
    assert_eq!(cluster(&enr), Some(2));
}
