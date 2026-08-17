use std::error::Error;
use std::io;
use std::path::Path;

use veyra_segment::{Segment, SegmentError, SegmentType};
use veyra_types::{GenerationId, LogSequenceNumber};

fn segment() -> Segment {
    Segment::build(
        SegmentType::Availability,
        0,
        GenerationId::new(1),
        LogSequenceNumber::new(1),
        LogSequenceNumber::new(2),
        1,
        vec![1],
    )
    .unwrap_or_else(|_| unreachable!())
}

#[test]
fn io_conversion_display_and_source_preserve_the_original_error() {
    let error: SegmentError = io::Error::new(io::ErrorKind::PermissionDenied, "denied").into();
    assert!(error.to_string().contains("segment I/O error"));
    assert!(error.to_string().contains("denied"));
    assert!(error.source().is_some());

    let semantic = SegmentError::ChecksumMismatch;
    assert_eq!(semantic.to_string(), "ChecksumMismatch");
    assert!(semantic.source().is_none());
}

#[test]
fn missing_file_name_and_missing_file_fail_through_typed_boundaries() -> Result<(), Box<dyn Error>>
{
    assert!(matches!(
        segment().write_atomic(Path::new("/")),
        Err(SegmentError::MissingFileName)
    ));

    let missing = std::env::temp_dir().join(format!(
        "veyra-definitely-missing-{}-{}.segment",
        std::process::id(),
        u128::MAX
    ));
    let Err(error) = Segment::read(&missing) else {
        return Err("missing file unexpectedly decoded".into());
    };
    assert!(matches!(error, SegmentError::Io(_)));
    assert!(error.source().is_some());
    Ok(())
}

#[test]
fn every_semantic_error_variant_has_a_stable_non_io_surface() {
    let variants = [
        SegmentError::InvalidMagic,
        SegmentError::UnsupportedVersion(2),
        SegmentError::UnknownSegmentType(99),
        SegmentError::TruncatedHeader(7),
        SegmentError::MalformedHeader,
        SegmentError::LsnRangeReversed,
        SegmentError::PayloadTooLarge(1),
        SegmentError::LengthOverflow,
        SegmentError::InvalidPayloadOffset(5),
        SegmentError::LengthMismatch {
            expected: 10,
            actual: 9,
        },
        SegmentError::ChecksumMismatch,
        SegmentError::InternalHeaderSize(3),
        SegmentError::MissingFileName,
    ];

    for error in variants {
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }
}
