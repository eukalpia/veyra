#![no_main]

use libfuzzer_sys::fuzz_target;
use veyra_cdc::{BatchLimits, decode_transaction_batch};

fuzz_target!(|data: &[u8]| {
    let limits = BatchLimits {
        max_items: 4_096,
        max_item_bytes: 1 << 20,
        max_prefix_bytes: 4_096,
        max_encoded_bytes: 2 << 20,
    };
    let _ = decode_transaction_batch(data, limits);
});
