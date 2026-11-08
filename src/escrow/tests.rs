use super::*;

// From lez-escrow's live run A on the testnet (docs/live-runs.md).
const BUYER: &str = "b70dc1e8a652446df3e59a9b86f94f671e13ef13774c46c86fbed67898cedc53";
const PROVIDER: &str = "381b90ccf84e376b999138f3edc5c8697b1fc7faf865dae4280755a6a78cab0c";
const JOB_REF: &str = "10e77eb5e89b3e325d566683fb6236c274e169623a0e6d14f8261cc99687d0b3";
const JOB_ACCOUNT: &str = "643506b0c55680bc3b193e51f6d9eadd20f522a6a8d66036ed91c5a89f1dac66";
// That job's record as get_account_public returned it, after the claim.
const RECORD: &str = "b70dc1e8a652446df3e59a9b86f94f671e13ef13774c46c86fbed67898cedc53381b90ccf84e376b999138f3edc5c8697b1fc7faf865dae4280755a6a78cab0c40420f0000000000000000000000000034280bbda101000001fbaad720bb4a8ddcaebe247f5bcb57db9ebe897a7460af7a25045db9e08226bc02";

fn bytes(hex: &str) -> [u8; 32] {
    let v: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    v.try_into().unwrap()
}

// The program live run A used, before the redeploy that refuses chained calls.
const RUN_A_PROGRAM: &str = "369d7d27dee17ad2f9d39e067ccaf3693c29bc4d432c4d8d6cb684e2ba85a68a";

#[test]
fn the_program_is_the_deployed_one() {
    // lez-escrow's README: the redeploy that refuses calls from other programs.
    assert_eq!(PROGRAM_ID, "CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G");
    assert_eq!(
        PROGRAM_ID_HEX,
        "a5e31b14f1304b129f108abb457eb389ed0f88103fa91b069208af5e9df6560d"
    );
}

#[test]
fn derives_the_job_account_the_live_run_used() {
    assert_eq!(
        job_account_in(&bytes(RUN_A_PROGRAM), &bytes(BUYER), &bytes(JOB_REF)),
        bytes(JOB_ACCOUNT)
    );
    assert_eq!(
        job_account(&bytes(BUYER), &bytes(JOB_REF)),
        job_account_in(&bytes(PROGRAM_ID_HEX), &bytes(BUYER), &bytes(JOB_REF))
    );
}

#[test]
fn encodes_instructions_as_the_program_expects() {
    // The README's worked example.
    let deliver = Instruction::Deliver {
        result: [4; 32],
        deadline: 1_800_000_000_000,
    }
    .encode();
    let mut want = vec![1u8];
    want.extend([4u8; 32]);
    want.extend([0x00, 0x50, 0x5c, 0x18, 0xa3, 0x01, 0x00, 0x00]);
    assert_eq!(deliver, want);
    let open = Instruction::Open {
        job_ref: [1; 32],
        provider: [2; 32],
        amount: 1_000_000,
        deadline: 7,
    }
    .encode();
    assert_eq!((open.len(), open[0]), (89, 0));
    assert_eq!(&open[65..81], &1_000_000u128.to_le_bytes());
    let claim = Instruction::Claim {
        buyer: [1; 32],
        job_ref: [2; 32],
        amount: 5,
    }
    .encode();
    assert_eq!((claim.len(), claim[0]), (81, 2));
    let refund = Instruction::Refund {
        job_ref: [1; 32],
        amount: 5,
        deadline: 9,
    }
    .encode();
    assert_eq!((refund.len(), refund[0]), (57, 4));
}

#[test]
fn reads_a_job_record() {
    let raw: Vec<u8> = (0..RECORD.len() / 2)
        .map(|i| u8::from_str_radix(&RECORD[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let r = Record::decode(&raw).unwrap();
    assert_eq!(r.buyer, bytes(BUYER));
    assert_eq!(r.provider, bytes(PROVIDER));
    assert_eq!(r.amount, 1_000_000);
    assert_eq!(
        r.deadline,
        u64::from_le_bytes([0x34, 0x28, 0x0b, 0xbd, 0xa1, 0x01, 0x00, 0x00])
    );
    assert!(r.result.is_some());
    assert_eq!(r.state, JobState::Claimed);
    assert!(Record::decode(&raw[..raw.len() - 1]).is_none());
}

#[test]
fn finds_the_record_in_the_programs_shard() {
    let account = serde_json::json!({
        "nonce": "0",
        "shards": [
            { "program": "0000000000000000000000000000000000000000000000000000000000000000", "data": "00" },
            { "program": PROGRAM_ID_HEX, "data": RECORD }
        ]
    })
    .to_string();
    assert_eq!(
        record_in(&account).unwrap().unwrap().state,
        JobState::Claimed
    );
    let empty = serde_json::json!({ "nonce": "0", "shards": [] }).to_string();
    assert_eq!(record_in(&empty).unwrap(), None);
    assert!(record_in("not json").is_err());
}
