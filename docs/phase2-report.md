# Phase 2 Report — Interrupts & Timers

**Date**: 2026-07-17  
**Status**: COMPLETE  
**Duration**: ~1 hour  

---

## Goal
Handle hardware interrupts. Establish timing for scheduling.

## What Was Implemented

### LAPIC (Local APIC) — `interrupts/lapic.rs`
- MMIO register access at `0xFEE00000` (standard x86, QEMU Q35)
- ID register read to verify MMIO accessibility
- Task Priority Register set to 0 (accept all interrupts)
- SVR (Spurious Interrupt Vector Register) enabled with vector 39
- LVT Timer configured for periodic mode at ~100 Hz
- Divide-by-16 prescaler, initial count 746

### IO-APIC — `interrupts/ioapic.rs`
- MMIO register access at `0xFEC00000`
- ID and version registers read (version 32, 24 redirection entries)
- `set_irq()`: configures IRQ→vector routing with destination APIC ID
- `mask_irq()` / `unmask_irq()`: per-IRQ masking control
- All 24 redirection entries masked during init

### PIT (Programmable Interval Timer) — `interrupts/pit.rs`
- Channel 0 on I/O ports `0x40-0x43`
- Divisor = 1,193,182 / 100 = 11,931 → ~100 Hz tick rate
- Command byte: Channel 0, lobyte/hibyte access, rate generator mode
- `on_tick()`: called from interrupt handler, increments atomic counter
- `tick_count()`: returns total ticks since boot
- `sleep_ms()`: blocking busy-wait for approximate delays

### IRQ Dispatch — `interrupts/dispatch.rs`
- Static handler table: 16 entries (IRQ 0-15, vectors 32-47)
- `register(irq, handler)`: registers a function pointer for an IRQ
- `dispatch(vector)`: calls registered handler, sends LAPIC EOI
- Type-safe: `IrqHandler = fn()` — handlers are regular Rust functions

### IDT Updates — `idt.rs`
- 16 static `extern "x86-interrupt"` handler functions (vectors 32-47)
- Each handler calls `dispatch::dispatch(vector)` which routes to registered handler
- Exception handlers unchanged from Phase 0 (breakpoint, double fault, GPF, page fault, etc.)

### Main Integration — `main.rs`
- Phase 2 banner and initialization sequence
- Timer handler registered on IRQ0: prints tick count every 100 ticks (1/sec)
- Keyboard handler registered on IRQ1: reads and prints scancodes from port 0x60
- 3-second spin-wait to demonstrate timer accuracy
- All Phase 1 functionality preserved (PMM, VMM, heap, etc.)

## New Files
| File | Purpose |
|------|---------|
| `interrupts/mod.rs` | Module root, `init()` orchestrator |
| `interrupts/lapic.rs` | Local APIC driver |
| `interrupts/ioapic.rs` | I/O APIC driver |
| `interrupts/pit.rs` | PIT timer driver |
| `interrupts/dispatch.rs` | IRQ handler registration and dispatch |

## Modified Files
| File | Changes |
|------|---------|
| `idt.rs` | Added 16 hardware IRQ handlers, `reload()` function |
| `main.rs` | Added `mod interrupts`, Phase 2 init, timer/keyboard handlers |

## Test Results
| Test | Result |
|------|--------|
| LAPIC ID read | ✓ (0x00000000) |
| LAPIC enabled | ✓ (SVR set) |
| IO-APIC version | ✓ (version 32, 24 entries) |
| PIT configured | ✓ (11931 divisor) |
| Timer fires at 100 Hz | ✓ (303 ticks in 3 seconds) |
| Timer continues firing | ✓ (24100+ ticks, no drift) |
| No triple faults | ✓ |
| No spurious interrupts | ✓ |
| Phase 1 regression | ✓ (all heap tests pass) |

## Architecture Notes
- LAPIC and IO-APIC MMIO addresses are hardcoded for QEMU Q35. Phase 5 (ACPI) will discover these dynamically from MADT.
- Keyboard handler reads scancodes but doesn't process them. Full PS/2 driver in Phase 12.
- Timer uses PIT (legacy). Phase 25 (Power Management) will migrate to APIC timer or HPET.
- Identity map still active (needed for LAPIC/IO-APIC MMIO). Removal deferred to Phase 3.
