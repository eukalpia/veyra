use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pgwire_replication::ReplicationConfig;
use veyra_cdc::{LiveReplicationError, TransactionBatch, run_pgwire};

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-transport-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

#[tokio::test]
async fn refused_local_connection_preserves_transport_error_context() {
    let journal_path = path("journal");
    let checkpoint_path = path("checkpoint");
    let config = ReplicationConfig::new(
        "127.0.0.1",
        "veyra",
        "invalid",
        "veyra",
        "veyra_test_slot",
        "veyra_test_publication",
    )
    .with_port(1);
    let mut apply = |_batch: &TransactionBatch| -> Result<(), std::convert::Infallible> { Ok(()) };

    let result = run_pgwire(config, &journal_path, &checkpoint_path, &mut apply).await;
    let Err(error) = result else {
        let _ = std::fs::remove_file(journal_path);
        let _ = std::fs::remove_file(checkpoint_path);
        return;
    };
    assert!(matches!(error, LiveReplicationError::Transport(_)));
    assert!(error.to_string().contains("transport:"));

    let _ = std::fs::remove_file(journal_path);
    let _ = std::fs::remove_file(checkpoint_path);
}
