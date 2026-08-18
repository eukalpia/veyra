use veyra_cdc::TransactionBatch;

#[test]
fn serde_rejects_commit_lsn_before_begin_lsn() {
    let invalid = r#"{
        "xid": 42,
        "begin_lsn": 20,
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
        "begin_lsn": 20,
        "commit_lsn": 21,
        "end_lsn": 20,
        "changes": []
    }"#;

    assert!(serde_json::from_str::<TransactionBatch>(invalid).is_err());
}
