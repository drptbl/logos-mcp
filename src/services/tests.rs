use super::*;
use serde_json::json;

const JOB: &str = "0123456789abcdef0123456789abcdef";

fn text(v: serde_json::Value) -> String {
    v.to_string()
}

fn msg(id: &str, body: serde_json::Value) -> String {
    let mut v = json!({ "v": "1.0", "job": JOB, "id": format!("{id:0>32}") });
    v.as_object_mut()
        .unwrap()
        .extend(body.as_object().unwrap().clone());
    text(v)
}

fn request() -> String {
    msg(
        "1",
        json!({ "type": "request", "service": "my-app/summarize/v1", "input": { "text": "summarise this" } }),
    )
}

fn quote(expires_at: u64) -> String {
    msg(
        "2",
        json!({
            "type": "quote",
            "price_lepta": "1000000",
            "pay_to": "Public/7Lrmxj4ZQDAeqmfzR5W27yb2WC7hyxrzP2V9nUXDx5Hd",
            "network": "logos.dev",
            "expires_at": expires_at,
            "deliver_within_s": 600
        }),
    )
}

fn seen(from_self: bool, text: &str) -> Seen<'_> {
    Seen {
        from_self,
        text,
        timestamp_ms: 1,
    }
}

#[test]
fn reads_a_message_and_writes_it_back_the_same() {
    let m = parse(&request()).unwrap();
    assert_eq!(m.job, JOB);
    assert_eq!(
        m.body,
        Body::Request {
            service: "my-app/summarize/v1".into(),
            input: Payload::Text("summarise this".into()),
        }
    );
    assert_eq!(parse(&encode(&m)).unwrap(), m);
}

#[test]
fn ignores_what_is_not_a_v1_protocol_message() {
    for other in [
        "hello there".to_string(),
        "{}".to_string(),
        request().replace("\"1.0\"", "\"2.0\""),
        request().replace("\"request\"", "\"haggle\""),
        request().replace(JOB, "not-hex"),
        msg("1", json!({ "type": "quote", "price_lepta": "12x" })),
    ] {
        assert_eq!(parse(&other), None, "{other}");
    }
    // A newer minor version only adds fields, which are ignored.
    let newer = request()
        .replace("\"1.0\"", "\"1.3\"")
        .replace("\"type\"", "\"extra\":1,\"type\"");
    assert!(parse(&newer).is_some());
}

#[test]
fn new_messages_carry_the_version_and_fresh_ids() {
    let a = Message::new(
        JOB,
        Body::Cancel {
            reason: String::new(),
        },
    );
    let b = Message::new(
        JOB,
        Body::Cancel {
            reason: String::new(),
        },
    );
    assert_eq!(a.v, VERSION);
    assert_eq!(a.id.len(), 32);
    assert_ne!(a.id, b.id);
    assert_eq!(new_job_id().len(), 32);
    assert_ne!(new_job_id(), new_job_id());
}

#[test]
fn the_quote_hash_is_sha256_of_the_text_as_received() {
    assert_eq!(
        quote_hash("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_job_runs_from_request_to_delivery() {
    let q = quote(10_000);
    let paid = msg(
        "3",
        json!({ "type": "paid", "quote": quote_hash(&q), "tx_hash": "abc" }),
    );
    let result = msg(
        "4",
        json!({ "type": "result", "output": { "cid": "zDvZ" } }),
    );
    // The buyer's view: it sent the request and the payment.
    let jobs = replay([
        seen(true, &request()),
        seen(false, &q),
        seen(true, &paid),
        seen(false, &result),
    ]);
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job.role, Role::Buyer);
    assert_eq!(job.state, State::Delivered);
    assert_eq!(job.quote.as_ref().unwrap().hash, quote_hash(&q));
    assert_eq!(job.tx_hash.as_deref(), Some("abc"));
    assert_eq!(job.output, Some(Payload::Cid("zDvZ".into())));

    // The provider sees the same conversation from the other side.
    let jobs = replay([
        seen(false, &request()),
        seen(true, &q),
        seen(false, &paid),
        seen(true, &result),
    ]);
    assert_eq!(jobs[0].role, Role::Provider);
    assert_eq!(jobs[0].state, State::Delivered);
}

#[test]
fn messages_from_the_wrong_side_or_out_of_order_are_ignored() {
    let q = quote(10_000);
    // A buyer can't quote its own request, and a result before a quote is ignored.
    let jobs = replay([
        seen(true, &request()),
        seen(true, &q),
        seen(
            false,
            &msg(
                "4",
                json!({ "type": "result", "output": { "text": "early" } }),
            ),
        ),
    ]);
    assert_eq!(jobs[0].state, State::Requested);
    assert_eq!(jobs[0].output, None);
}

#[test]
fn a_payment_for_another_quote_is_ignored() {
    let q = quote(10_000);
    let jobs = replay([
        seen(true, &request()),
        seen(false, &q),
        seen(
            true,
            &msg(
                "3",
                json!({ "type": "paid", "quote": quote_hash("something else"), "tx_hash": "x" }),
            ),
        ),
    ]);
    assert_eq!(jobs[0].state, State::Quoted);
}

#[test]
fn repeats_and_messages_after_the_end_are_ignored() {
    let reject = msg("5", json!({ "type": "reject", "reason": "busy" }));
    let jobs = replay([
        seen(false, &request()),
        seen(false, &request()),
        seen(true, &reject),
        seen(true, &quote(10_000)),
    ]);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, State::Rejected);
    assert_eq!(jobs[0].reason.as_deref(), Some("busy"));
    assert!(jobs[0].quote.is_none());
}

#[test]
fn a_provider_may_deliver_on_payment_alone() {
    // The `paid` message is only a hint: the balance on pay_to is the proof.
    let jobs = replay([
        seen(false, &request()),
        seen(true, &quote(10_000)),
        seen(
            true,
            &msg(
                "4",
                json!({ "type": "result", "output": { "text": "done" } }),
            ),
        ),
    ]);
    assert_eq!(jobs[0].state, State::Delivered);
}

#[test]
fn an_unpaid_quote_expires() {
    let jobs = replay([seen(true, &request()), seen(false, &quote(5_000))]);
    let job = &jobs[0];
    assert_eq!(job.state_at(4_999, false), State::Quoted);
    assert_eq!(job.state_at(5_000, false), State::Expired);
    // A payment that landed in time is honoured even after expiry.
    assert_eq!(job.state_at(9_000, true), State::Paid);
}

#[test]
fn a_provider_counts_a_payment_only_once_it_lands() {
    let q = quote(10_000);
    let claimed = msg(
        "3",
        json!({ "type": "paid", "quote": quote_hash(&q), "tx_hash": "made-up" }),
    );
    let jobs = replay([
        seen(false, &request()),
        seen(true, &q),
        seen(false, &claimed),
    ]);
    let job = &jobs[0];
    assert_eq!(job.state, State::Paid, "the message says so");
    assert_eq!(job.state_at(5_000, false), State::Quoted);
    assert_eq!(job.state_at(10_000, false), State::Expired);
    assert_eq!(job.state_at(10_000, true), State::Paid);
}

fn escrow_quote(expires_at: u64) -> String {
    msg(
        "2",
        json!({
            "type": "quote",
            "price_lepta": "1000000",
            "pay_to": "Public/7Lrmxj4ZQDAeqmfzR5W27yb2WC7hyxrzP2V9nUXDx5Hd",
            "network": "logos.dev",
            "expires_at": expires_at,
            "deliver_within_s": 600,
            "escrow": {
                "program": "CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G",
                "provider": "Public/4n2DCJMYveciCLAvfcR1nWvMFbr7f8zSc7KGaG7YMpLK",
                "deadline": expires_at + 600_000
            }
        }),
    )
}

#[test]
fn a_quote_may_offer_escrow_and_a_payment_names_its_buyer() {
    assert_eq!(VERSION, "1.1");
    let q = escrow_quote(10_000);
    let paid = msg(
        "3",
        json!({
            "type": "paid", "quote": quote_hash(&q), "tx_hash": "t1",
            "escrow_buyer": "Public/DKZmzyVG8sccQKQPGaFoZr8wi6rMGgMwJiB77YCPZjsQ"
        }),
    );
    let jobs = replay([seen(true, &request()), seen(false, &q), seen(true, &paid)]);
    let job = &jobs[0];
    let escrow = job.quote.as_ref().unwrap().escrow.as_ref().unwrap();
    assert_eq!(
        escrow.program,
        "CAZ8AvrdAeZ4rQjHqhAucqSLqrMK8TFQ7Z8b2jg5Sb4G"
    );
    assert_eq!(escrow.deadline, 610_000);
    assert_eq!(
        job.escrow_buyer.as_deref(),
        Some("Public/DKZmzyVG8sccQKQPGaFoZr8wi6rMGgMwJiB77YCPZjsQ")
    );
    // A v1.0 quote has none, and still reads.
    let plain = replay([seen(true, &request()), seen(false, &quote(10_000))]);
    assert!(plain[0].quote.as_ref().unwrap().escrow.is_none());
}

#[test]
fn a_malformed_escrow_offer_spoils_the_quote() {
    let bad = escrow_quote(10_000).replace("\"Public/4n2D", "\"4n2D");
    assert_eq!(parse(&bad), None);
}
