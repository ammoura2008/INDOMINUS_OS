//! # Address Space Layout Randomization (ASLR)
//!
//! Provides a simple PRNG and randomization functions for user-space memory
//! layout. Each process gets randomized stack, mmap, and heap bases to
//! defeat predictable memory layout attacks.

use core::sync::atomic::{AtomicU64, Ordering};

/// Global PRNG state, seeded from PIT tick count during boot.
static mut PRNG_STATE: u64 = 0;

/// Initialize the PRNG with a seed (called once during boot from PIT ticks).
pub fn init(seed: u64) {
    unsafe { PRNG_STATE = seed; }
}

/// Xorshift64 PRNG — fast, non-cryptographic, good enough for ASLR.
fn next_u64() -> u64 {
    unsafe {
        let mut x = PRNG_STATE;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        PRNG_STATE = x;
        x
    }
}

/// Get a random value in [0, max).
fn rand_range(max: u64) -> u64 {
    if max == 0 { return 0; }
    next_u64() % max
}

/// Random offset in pages for ASLR.
/// Returns a random offset between 0 and (num_slots * PAGE_SIZE), page-aligned.
fn random_page_offset(num_slots: u64) -> u64 {
    rand_range(num_slots) * 4096
}

/// Get a randomized user stack top.
///
/// The stack base is randomized by up to STACK_ASLR_SLOTS pages below
/// the nominal USER_STACK_TOP. This defeats predictable stack locations
/// for buffer overflow exploits.
pub fn randomize_stack_base() -> u64 {
    const STACK_ASLR_SLOTS: u64 = 16; // up to 64 KiB randomization
    let offset = random_page_offset(STACK_ASLR_SLOTS);
    super::memory::USER_STACK_TOP - offset
}

/// Get a randomized mmap base address.
///
/// The mmap region starts at a random offset above USER_MMAP_BASE,
/// randomized by up to MMAP_ASLR_SLOTS pages.
pub fn randomize_mmap_base() -> u64 {
    const MMAP_ASLR_SLOTS: u64 = 32; // up to 128 KiB randomization
    let offset = random_page_offset(MMAP_ASLR_SLOTS);
    super::memory::USER_MMAP_BASE + offset
}

/// Get a randomized heap base address.
///
/// The heap starts at a random offset above USER_HEAP_BASE,
/// randomized by up to HEAP_ASLR_SLOTS pages.
pub fn randomize_heap_base() -> u64 {
    const HEAP_ASLR_SLOTS: u64 = 64; // up to 256 KiB randomization
    let offset = random_page_offset(HEAP_ASLR_SLOTS);
    super::memory::USER_HEAP_BASE + offset
}
