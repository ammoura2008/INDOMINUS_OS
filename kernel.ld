/*
 * indo-kernel/kernel.ld — INDOMINUS Kernel Linker Script
 *
 * What is a linker script?
 * ========================
 * A linker script tells the linker (ld) how to take compiled object files
 * and arrange their sections into a final binary. For a kernel, this is
 * critically important because:
 *
 *   1. We control EXACTLY where every section lands in memory
 *   2. We can place read-only data in read-only pages (security)
 *   3. We can place executable code in executable-only pages (NX enforcement)
 *   4. We can align sections to page boundaries (required for page table setup)
 *   5. We can define symbols (like __kernel_start, __bss_start) that Rust
 *      code can reference to know where sections are in memory
 *
 * Kernel virtual base address
 * ===========================
 * The kernel is linked to run at 0xFFFFFFFF80000000 (-2 GiB).
 * This is a well-established convention: all addresses at or above this
 * threshold are "kernel space". Everything below is "user space".
 *
 * During early boot (before we set up our own page tables), the CPU is
 * still using UEFI's identity map (physical == virtual). Our bootloader
 * sets up a minimal page table that maps the kernel's physical location
 * to 0xFFFFFFFF80000000 before jumping to this address.
 *
 * Section layout
 * ==============
 *   .text    — Executable kernel code (rx permissions)
 *   .rodata  — Read-only data: string literals, constant tables (r permissions)
 *   .data    — Initialized mutable data (rw permissions)
 *   .bss     — Zero-initialized uninitialized data (rw permissions, zeroed by bootloader)
 *
 * WHY this ordering?
 * - Security: separate sections allow the page table to apply different
 *   permissions (.text = rx, .rodata = r, .data/.bss = rw, never rwx)
 * - Performance: related data placed together improves cache locality
 */

/* Output format: 64-bit ELF for x86-64 */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)

/* The symbol that the ELF entry point field will point to.
 * The bootloader reads this from the ELF header and jumps to it.
 * This must match the #[no_mangle] pub extern "C" fn kernel_main() in main.rs */
ENTRY(kernel_main)

SECTIONS {
    /* ──────────────────────────────────────────────
     * Kernel virtual base address
     * All sections are placed relative to this.
     * ────────────────────────────────────────────── */
    . = 0xFFFFFFFF80000000;

    /* Export the kernel start address for use in Rust code */
    __kernel_start = .;

    /* ──────────────────────────────────────────────
     * .text — Executable code
     * ────────────────────────────────────────────── */
    .text ALIGN(4096) : {
        . = ALIGN(16);
        __text_start = .;
        *(.text._start)
        *(.text .text.*)
        . = ALIGN(16);
        __text_end = .;
    }

    /* ──────────────────────────────────────────────
     * .rodata — Read-only data
     * String literals, constant tables, etc.
     * Page-aligned so we can set r-- permissions.
     * ────────────────────────────────────────────── */
    .rodata ALIGN(4096) : {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    }

    /* ──────────────────────────────────────────────
     * .data — Initialized writable data
     * Global variables with initial values.
     * ────────────────────────────────────────────── */
    .data ALIGN(4096) : {
        __data_start = .;
        *(.data .data.*)
        __data_end = .;
    }

    /* ──────────────────────────────────────────────
     * .bss — Zero-initialized uninitialized data
     *
     * BSS stands for "Block Started by Symbol" (historical artifact).
     * These are global/static variables declared without an initial value.
     * They must be zero-initialized before the kernel runs.
     *
     * The bootloader zero-fills this region (p_memsz > p_filesz in the
     * ELF program header). It does NOT take space in the binary file.
     * ────────────────────────────────────────────── */
    .bss ALIGN(4096) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)              /* C-style uninitialized globals */
        __bss_end = .;
    }

    /* Export kernel end address */
    __kernel_end = .;

    /* ──────────────────────────────────────────────
     * Discard DWARF debug sections from release builds.
     * Keep them for dev builds (controlled by Makefile).
     *
     * Note: We do NOT discard .debug_* here — we handle that
     * with compiler flags. The kernel ELF can carry debug info
     * without it being loaded into memory (it's in non-PT_LOAD sections).
     * ────────────────────────────────────────────── */

    /* Discard sections that are never executed or needed at runtime */
    /DISCARD/ : {
        *(.comment)            /* Compiler version strings */
        *(.note.*)             /* ELF notes (not needed at runtime) */
        *(.eh_frame)           /* Exception handling frames (we don't unwind) */
        *(.eh_frame_hdr)
    }
}
