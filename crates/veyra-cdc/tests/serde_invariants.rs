use veyra_cdc::TransactionBatch;
use veyra_types::LogSequenceNumber;

#[test]
fn serde_rejects_commit_lsn_before_final_lsn() {
    let invalid = r#"{
        "xid": 42,
        "final_lsn": 20,
        "commit_lsn": 19,
        "end_lsn": 21,
        "changes": []
    }"#;

    assert!(serde_json::from_str::<TransactionBatch>(invalid).is_err());
}

#[test]
fn serde_rejects_end_lsn_before_commit_lsn() {
    let invalid = r#"{
        "xid": 42,
        "final_lsn": 20,
        "commit_lsn": 21,
        "end_lsn": 20,
        "changes": []
    }"#;

    assert!(serde_json::from_str::<TransactionBatch>(invalid).is_err());
}

#[test]
fn serde_accepts_an_ordered_transaction_boundary() {
    let valid = r#"{
        "xid": 42,
        "final_lsn": 20,
        "commit_lsn": 20,
        "end_lsn": 21,
        "changes": []
    }"#;

    let transaction =
        serde_json::from_str::<TransactionBatch>(valid).unwrap_or_else(|_| unreachable!());
    assert_eq!(transaction.xid(), 42);
    assert_eq!(transaction.final_lsn(), LogSequenceNumber::new(20));
    assert_eq!(transaction.commit_lsn(), LogSequenceNumber::new(20));
    assert_eq!(transaction.end_lsn(), LogSequenceNumber::new(21));
}
