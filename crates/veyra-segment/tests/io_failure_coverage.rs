use std::error::Error as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_segment::{Segment, SegmentError, SegmentType};
use veyra_types::{GenerationId, LogSequenceNumber};

fn path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "veyra-segment-io-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn segment() -> Segment {
    Segment::build(
        SegmentType::Availability,
        7,
        GenerationId::new(3),
        LogSequenceNumber::new(10),
        LogSequenceNumber::new(20),
        1,
        vec![1, 2, 3],
    )
    .unwrap_or_else(|_| unreachable!())
}

fn temporary_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .unwrap_or_else(|| unreachable!())
        .to_string_lossy();
    target.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

#[test]
fn preexisting_temporary_file_fails_without_leaving_stale_state() {
    let target = path("temp-collision");
    let temporary = temporary_path(&target);
    fs::write(&temporary, b"collision").unwrap_or_else(|_| unreachable!());

    let error = segment()
        .write_atomic(&target)
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(error, SegmentError::Io(_)));
    assert!(error.source().is_some());
    assert!(!temporary.exists());
    assert!(!target.exists());
}

#[test]
fn rename_failure_removes_the_new_temporary_segment() {
    let target = path("rename-target");
    fs::create_dir(&target).unwrap_or_else(|_| unreachable!());
    fs::write(target.join("keep"), b"non-empty").unwrap_or_else(|_| unreachable!());
    let temporary = temporary_path(&target);

    let error = segment()
        .write_atomic(&target)
        .err()
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(error, SegmentError::Io(_)));
    assert!(error.source().is_some());
    assert!(!temporary.exists());
    assert!(target.is_dir());

    let _ = fs::remove_file(target.join("keep"));
    let _ = fs::remove_dir(target);
}

#[test]
fn reading_a_directory_is_a_typed_io_failure() {
    let directory = path("read-directory");
    fs::create_dir(&directory).unwrap_or_else(|_| unreachable!());
    let result = Segment::read(&directory);
    assert!(matches!(result, Err(SegmentError::Io(_))));
    let _ = fs::remove_dir(directory);
}
