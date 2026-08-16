use std::error::Error as _;
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use veyra_segment::{Segment, SegmentError, SegmentType};
use veyra_types::{GenerationId, LogSequenceNumber};

fn path(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    std::env::temp_dir().join(format!("veyra-segment-coverage-{label}-{}-{nanos}.bin", std::process::id()))
}
fn segment(kind: SegmentType) -> Segment {
    Segment::build(kind, 7, GenerationId::new(3), LogSequenceNumber::new(10),
        LogSequenceNumber::new(20), 2, vec![1, 2, 3]).unwrap_or_else(|_| unreachable!())
}

#[test]
fn every_segment_type_round_trips() {
    for kind in [SegmentType::Availability, SegmentType::Property, SegmentType::RoomType,
        SegmentType::Pricing, SegmentType::Rules] {
        let original = segment(kind);
        let encoded = original.encode().unwrap_or_else(|_| unreachable!());
        let decoded = Segment::decode(&encoded).unwrap_or_else(|_| unreachable!());
        assert_eq!(decoded.header().segment_type, kind);
        assert_eq!(decoded.payload(), &[1, 2, 3]);
    }
}

#[test]
fn header_and_payload_corruption_fail_closed() {
    assert!(matches!(Segment::decode(&[0; 3]), Err(SegmentError::TruncatedHeader(3))));
    let original = segment(SegmentType::Availability).encode().unwrap_or_else(|_| unreachable!());

    let mut bytes = original.clone(); bytes[0] = b'X';
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::InvalidMagic)));
    let mut bytes = original.clone(); bytes[4..6].copy_from_slice(&99_u16.to_le_bytes());
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::UnsupportedVersion(99))));
    let mut bytes = original.clone(); bytes[6..8].copy_from_slice(&99_u16.to_le_bytes());
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::UnknownSegmentType(99))));
    let mut bytes = original.clone(); bytes[24..32].copy_from_slice(&30_u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&20_u64.to_le_bytes());
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::LsnRangeReversed)));
    let mut bytes = original.clone(); bytes[48..56].copy_from_slice(&79_u64.to_le_bytes());
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::InvalidPayloadOffset(79))));
    let mut bytes = original.clone(); bytes[56..64].copy_from_slice(&(512_u64 * 1024 * 1024 + 1).to_le_bytes());
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::PayloadTooLarge(_))));
    let mut bytes = original.clone(); bytes.pop();
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::LengthMismatch { .. })));
    let mut bytes = original.clone(); bytes.push(0);
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::LengthMismatch { .. })));
    let mut bytes = original; let last = bytes.len() - 1; bytes[last] ^= 0xff;
    assert!(matches!(Segment::decode(&bytes), Err(SegmentError::ChecksumMismatch)));
}

#[test]
fn file_io_atomicity_and_oversize_guard_are_exercised() {
    let target = path("atomic");
    let original = segment(SegmentType::Pricing);
    original.write_atomic(&target).unwrap_or_else(|_| unreachable!());
    assert_eq!(Segment::read(&target).unwrap_or_else(|_| unreachable!()), original);

    let missing = path("missing");
    let error = Segment::read(&missing).err().unwrap_or_else(|| unreachable!());
    assert!(matches!(error, SegmentError::Io(_)));
    assert!(error.source().is_some());

    let oversized = path("oversized");
    let file = OpenOptions::new().create(true).truncate(true).write(true).open(&oversized)
        .unwrap_or_else(|_| unreachable!());
    file.set_len(512_u64 * 1024 * 1024 + 81).unwrap_or_else(|_| unreachable!());
    drop(file);
    assert!(matches!(Segment::read(&oversized), Err(SegmentError::PayloadTooLarge(_))));

    assert!(matches!(original.write_atomic(Path::new("/")), Err(SegmentError::MissingFileName)));
    let impossible_parent = path("no-parent").join("missing").join("segment.bin");
    assert!(matches!(original.write_atomic(&impossible_parent), Err(SegmentError::Io(_))));

    let _ = fs::remove_file(target); let _ = fs::remove_file(oversized);
}

#[test]
fn error_display_source_and_build_validation_are_covered() {
    assert!(matches!(Segment::build(SegmentType::Rules, 0, GenerationId::new(1),
        LogSequenceNumber::new(2), LogSequenceNumber::new(1), 0, vec![]),
        Err(SegmentError::LsnRangeReversed)));
    for error in [
        SegmentError::InvalidMagic, SegmentError::UnsupportedVersion(2), SegmentError::UnknownSegmentType(9),
        SegmentError::TruncatedHeader(3), SegmentError::MalformedHeader, SegmentError::LsnRangeReversed,
        SegmentError::PayloadTooLarge(4), SegmentError::LengthOverflow, SegmentError::InvalidPayloadOffset(5),
        SegmentError::LengthMismatch { expected: 1, actual: 2 }, SegmentError::ChecksumMismatch,
        SegmentError::InternalHeaderSize(3), SegmentError::MissingFileName,
    ] {
        assert!(!error.to_string().is_empty()); assert!(error.source().is_none());
    }
    let io_error = SegmentError::from(std::io::Error::other("segment"));
    assert!(io_error.source().is_some()); assert!(io_error.to_string().contains("segment"));
}
