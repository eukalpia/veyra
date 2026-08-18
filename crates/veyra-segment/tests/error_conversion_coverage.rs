use std::error::Error;
use std::io;

use veyra_segment::SegmentError;

#[test]
fn segment_error_conversion_display_and_source_cover_both_families() {
    let io_error = SegmentError::from(io::Error::other("boom"));
    assert!(io_error.to_string().contains("segment I/O error"));
    assert!(io_error.source().is_some());

    let semantic = SegmentError::InvalidMagic;
    assert_eq!(semantic.to_string(), "InvalidMagic");
    assert!(semantic.source().is_none());
}
