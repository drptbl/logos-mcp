use super::*;

// From the escrow's live run A on the testnet, under the program it used then.
const BUYER: &str = "b70dc1e8a652446df3e59a9b86f94f671e13ef13774c46c86fbed67898cedc53";
const JOB_REF: &str = "10e77eb5e89b3e325d566683fb6236c274e169623a0e6d14f8261cc99687d0b3";
const JOB_ACCOUNT: &str = "643506b0c55680bc3b193e51f6d9eadd20f522a6a8d66036ed91c5a89f1dac66";
const RUN_A_PROGRAM: &str = "369d7d27dee17ad2f9d39e067ccaf3693c29bc4d432c4d8d6cb684e2ba85a68a";
// That job's record, in the layout before payout addresses.
const OLD_RECORD: &str = "b70dc1e8a652446df3e59a9b86f94f671e13ef13774c46c86fbed67898cedc53381b90ccf84e376b999138f3edc5c8697b1fc7faf865dae4280755a6a78cab0c40420f0000000000000000000000000034280bbda101000001fbaad720bb4a8ddcaebe247f5bcb57db9ebe897a7460af7a25045db9e08226bc02";

fn bytes(hex: &str) -> [u8; 32] {
    let v: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    v.try_into().unwrap()
}

fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// A record laid out as logos-escrow's README lists the fields.
fn record(state: u8, result: Option<[u8; 32]>) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend([1u8; 32]); // buyer
    raw.extend([2u8; 32]); // provider
    raw.extend([3u8; 32]); // pay_to
    raw.extend([6u8; 32]); // refund_to
    raw.extend(500u128.to_le_bytes());
    raw.extend(1_800_000_000_000u64.to_le_bytes());
    match result {
        Some(r) => {
            raw.push(1);
            raw.extend(r);
        }
        None => raw.push(0),
    }
    raw.push(state);
    raw
}

#[test]
fn the_program_is_the_deployed_one() {
    // logos-escrow's README: pull-only payouts with payout addresses (image 1d646179…).
    assert_eq!(PROGRAM_ID, "HpavNGFYxjyhLsy171aXsvv146yUeXmRWiho22uvT43d");
    assert_eq!(
        PROGRAM_ID_HEX,
        "f9eba477278c918fadafbcb4a6463e947d0fd8de344b5431ee0aa3cd41d7e3a4"
    );
}

#[test]
fn derives_the_job_account_the_escrow_does() {
    // Pinned in logos-escrow's core/tests/escrow.rs.
    assert_eq!(
        hex(&job_account_in(&[7; 32], &[1; 32], &[9; 32])),
        "1fa6999db67e6acc47b26a123f282e33e37ad257f071ce550c6ab33c1879cecc"
    );
    // And the account live run A really used.
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
    // The bytes logos-escrow's own tests pin.
    let open = Instruction::Open {
        job_ref: [9; 32],
        provider: [2; 32],
        pay_to: [3; 32],
        refund_to: [6; 32],
        amount: 500,
        deadline: 1_800_000_000_000,
    };
    assert_eq!(
        hex(&open.encode()),
        concat!(
            "00",
            "0909090909090909090909090909090909090909090909090909090909090909",
            "0202020202020202020202020202020202020202020202020202020202020202",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "0606060606060606060606060606060606060606060606060606060606060606",
            "f4010000000000000000000000000000",
            "00505c18a3010000"
        )
    );
    assert_eq!(
        hex(&Instruction::Deliver {
            result: [4; 32],
            deadline: 1_800_000_000_000
        }
        .encode()),
        "01040404040404040404040404040404040404040404040404040404040404040400505c18a3010000"
    );
    assert_eq!(
        hex(&Instruction::Release { job_ref: [9; 32] }.encode()),
        "030909090909090909090909090909090909090909090909090909090909090909"
    );
    assert_eq!(hex(&Instruction::Return.encode()), "05");
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
    let r = Record::decode(&record(1, Some([4; 32]))).unwrap();
    assert_eq!(
        (r.buyer, r.provider, r.pay_to, r.refund_to),
        ([1; 32], [2; 32], [3; 32], [6; 32])
    );
    assert_eq!((r.amount, r.deadline), (500, 1_800_000_000_000));
    assert_eq!(r.result, Some([4; 32]));
    assert_eq!(r.state, JobState::Delivered);
    let states = [
        JobState::Open,
        JobState::Delivered,
        JobState::Claimed,
        JobState::Released,
        JobState::Refunded,
        JobState::Returned,
    ];
    for (byte, state) in states.into_iter().enumerate() {
        assert_eq!(
            Record::decode(&record(byte as u8, None)).unwrap().state,
            state
        );
    }
    assert!(Record::decode(&record(6, None)).is_none());
    let full = record(1, Some([4; 32]));
    assert!(Record::decode(&full[..full.len() - 1]).is_none());
    // A record from before payout addresses is not taken for one.
    let old: Vec<u8> = (0..OLD_RECORD.len() / 2)
        .map(|i| u8::from_str_radix(&OLD_RECORD[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    assert!(Record::decode(&old).is_none());
}

#[test]
fn finds_the_record_in_the_programs_shard() {
    let account = serde_json::json!({
        "nonce": "0",
        "shards": [
            { "program": "0000000000000000000000000000000000000000000000000000000000000000", "data": "00" },
            { "program": PROGRAM_ID_HEX, "data": hex(&record(2, Some([4; 32]))) }
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
