#![no_main]

use libfuzzer_sys::fuzz_target;
use veyra_types::{LogSequenceNumber, ProjectionProgress};

fn read_u64(input: &[u8], offset: usize) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&input[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 {
        return;
    }

    let received = LogSequenceNumber::new(read_u64(data, 0));
    let durable = LogSequenceNumber::new(read_u64(data, 8));
    let applied = LogSequenceNumber::new(read_u64(data, 16));
    let published = LogSequenceNumber::new(read_u64(data, 24));

    let _ = ProjectionProgress::try_new(received, durable, applied, published);
});
