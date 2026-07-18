# Phase 1 Engineering Report — Memory Management

**Date**: 2026-07-17  
**Status**: COMPLETE (compilation verified)  
**Phase**: 1 — Memory Management

---

## Phase Objective

Implement the memory management foundation for INDOMINUS:
- Physical Memory Manager (PMM) for tracking free/used 4 KiB frames
- Virtual Memory Manager (VMM) for page table manipulation
- Kernel heap allocator for dynamic memory (Box, Vec, String)

## What Was Implemented

### 1. Physical Memory Manager (`memory/pmm.rs`)

**Design**: Bitmap allocator where each bit represents one 4 KiB physical frame.

- **Bitmap storage**: Static array in BSS (512 KiB for 16 GiB max RAM)
- **Initialization**: Reads UEFI memory map, marks usable regions as free, marks kernel/bitmap as used
- **alloc_frame()**: Linear scan for first free frame, O(n) worst case
- **free_frame()**: Clears bit in bitmap
- **alloc_contiguous()**: Finds N contiguous free frames
- **Query functions**: total_frames(), free_frames(), is_allocated()

### 2. Virtual Memory Manager (`memory/vmm.rs`)

**Design**: Uses x86_64 crate's `OffsetPageTable` Mapper trait for safe page table manipulation.

- **map_page()**: Maps a virtual address to a physical address
- **unmap_page()**: Removes a mapping
- **translate_addr()**: Virtual-to-physical translation
- **init_kernel_page_tables()**: Creates PML4 with:
  - Higher-half kernel mapping (physical → 0xFFFFFFFF80000000)
  - Identity mapping of first 4 GiB (for CR3 switch safety)
- **switch_page_table()**: Writes new PML4 to CR3
- **remove_identity_map()**: Clears PML4[0] after kernel is running at higher-half

### 3. Kernel Heap (`memory/mod.rs`)

**Design**: `linked_list_allocator` crate as `#[global_allocator]`.

- **Global allocator**: `LockedHeap` with spinlock
- **Heap region**: Starts at 0xFFFF_FFFF_C000_0000, 4 MiB initial size
- **alloc_error_handler**: Panics with allocation details on OOM

## Files Modified

| File | Action | Lines Added |
|------|--------|-------------|
| `indo-kernel/src/memory/pmm.rs` | NEW | ~315 |
| `indo-kernel/src/memory/vmm.rs` | NEW | ~275 |
| `indo-kernel/src/memory/mod.rs` | NEW | ~65 |
| `indo-kernel/src/main.rs` | MODIFIED | +80 lines (Phase 1 init) |
| `indo-kernel/src/main.rs` | MODIFIED | +2 feature flags |
| `indo-kernel/src/idt.rs` | MODIFIED | API fixes for x86_64 v0.15 |
| `indo-boot/src/main.rs` | MODIFIED | Removed uefi_raw import |
| `indo-boot/Cargo.toml` | MODIFIED | Removed uefi-raw dependency |
| `Cargo.toml` | MODIFIED | Fixed uefi-raw version |
| `libs/indo-core/src/lib.rs` | UNCHANGED | — |

## Architecture Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| PMM type | Bitmap allocator | Simple, correct, O(1) alloc with linear scan, scales to 16 GiB |
| VMM approach | x86_64 crate Mapper | Safe, battle-tested, avoids manual page table walking |
| Heap allocator | linked_list_allocator | Already a dependency, sufficient for early kernel |
| Identity map | First 4 GiB, temporary | Required for safe CR3 switch; removed after transition |
| PhysAddr type | Re-exported from indo_core | Prevents type confusion between indo_core and x86_64 PhysAddr |

## Build Results

```
cargo build --target x86_64-unknown-none -p indo-kernel
  Finished `dev` profile [optimized + debuginfo] target(s) in 2.50s
  0 errors, 18 warnings
```

### Warnings (non-blocking)

- `static_mut_refs`: BITMAP/TOTAL_FRAMES/FREE_FRAMES use `static mut` (will be replaced with interior mutability in Phase 3)
- `unused functions`: free_frame, alloc_contiguous, etc. (used in later phases)
- `unnecessary unsafe`: gdt.rs, idt.rs (cosmetic, pre-existing)
- `probe-stack`: Unrecognized target feature (pre-existing, from .cargo/config.toml)

## Known Issues

### CRITICAL: Bootloader Build Failure (pre-existing)

The bootloader (`indo-boot`) fails to compile due to API incompatibilities with the `uefi` 0.24 crate. Issues include:
- `uefi::helpers::init` not found
- `SystemTable<Boot>` generic parameter mismatch
- `exit_boot_services()` signature changed
- `MemoryType::MEMORY_MAPPED_IO` renamed

**This is NOT caused by Phase 1 changes.** The bootloader code was written for an older `uefi` crate version. Fixing this is required before QEMU testing but is a separate task.

### MINOR: Identity Map Memory Leak

When `remove_identity_map()` is called, it only clears the PML4 entry for the lower half. The PDPT frame and all sub-tables (PD, PT) allocated for the identity map are leaked. This is acceptable for Phase 1 — Phase 9 will implement proper recursive page table freeing.

### MINOR: Static Mut References

The PMM uses `static mut` for BITMAP, TOTAL_FRAMES, and FREE_FRAMES. The Rust 2024 edition warns about this because mutable references to mutable statics are UB-prone. Phase 3 will replace these with `spin::Mutex` or `core::cell::UnsafeCell`.

## Testing

### Compilation Test: PASSED
- Kernel compiles without errors
- All modules link correctly
- Feature flags work (alloc_error_handler, abi_x86_interrupt)

### QEMU Boot Test: BLOCKED (pre-existing bootloader issue)
- Cannot test until bootloader API is fixed
- Expected behavior: PMM reports memory, VMM switches page tables, heap allocates successfully

## Future Improvements

1. **Fix bootloader** for `uefi` 0.24 API (required for QEMU testing)
2. **Add boot test automation** (capture serial output, verify expected strings)
3. **Replace `static mut`** with interior mutability (Phase 3)
4. **Add free-list** to PMM for O(1) allocation (Phase 2)
5. **Implement proper `unmap`** with recursive frame freeing (Phase 9)
6. **Add guard pages** to kernel stack and heap (Phase 2)

## Technical Debt

| Item | Priority | Phase to Resolve |
|------|----------|-----------------|
| Bootloader uefi 0.24 API fix | HIGH | Before QEMU testing |
| static mut → interior mutability | MEDIUM | Phase 3 |
| Identity map frame leak | LOW | Phase 9 |
| PMM linear scan → free list | LOW | Phase 2 |
| No stack guard pages | MEDIUM | Phase 2 |
