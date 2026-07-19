//! # Interrupt Descriptor Table (IDT)
//!
//! ## What is the IDT?
//!
//! The IDT is a table of 256 entries (gate descriptors) that the CPU indexes
//! by interrupt vector number to find the handler for each interrupt or exception.
//!
//! ## Interrupts vs Exceptions — the difference
//!
//! **Exceptions** are synchronous — they happen BECAUSE of something the current
//! instruction did:
//! - #DE (0): Division by zero
//! - #PF (14): Page fault — access to unmapped/protected memory
//! - #GP (13): General Protection Fault — privilege violation, bad segment, etc.
//! - #DF (8): Double Fault — exception while handling an exception
//!
//! **Interrupts** are asynchronous — they happen between instructions because of
//! external hardware events:
//! - IRQ0 (32): Programmable Interval Timer tick
//! - IRQ1 (33): PS/2 Keyboard press/release
//! - IRQ8 (40): Real-Time Clock
//!
//! WHY 32 for the first IRQ? Vectors 0–31 are reserved by Intel for exceptions.
//! The APIC/PIC is configured to deliver hardware interrupts starting at 32.
//!
//! ## What happens when an interrupt fires?
//!
//! 1. CPU checks IDT for the handler at vector number N
//! 2. CPU pushes an interrupt frame onto the current (or IST) stack:
//!    - RIP (instruction pointer at time of interrupt)
//!    - CS (code segment selector)
//!    - RFLAGS (CPU flags)
//!    - RSP (stack pointer) [only if privilege level changed]
//!    - SS (stack segment) [only if privilege level changed]
//!    - Error code [for some exceptions only]
//! 3. CPU jumps to the handler
//! 4. Handler executes
//!    - Hardware IRQs: calls `dispatch::dispatch(vector)`, which calls
//!      the registered handler and sends EOI to LAPIC
//! 5. `iretq` restores the frame and resumes execution

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use spin::Once;

use crate::gdt::DOUBLE_FAULT_IST_INDEX;

// ─────────────────────────────────────────────────────────────────────────────
// IDT global state
// ─────────────────────────────────────────────────────────────────────────────

static IDT: Once<InterruptDescriptorTable> = Once::new();

// ─────────────────────────────────────────────────────────────────────────────
// Hardware IRQ handlers (vectors 32-47)
// ─────────────────────────────────────────────────────────────────────────────

// Generate static handler functions for each hardware IRQ vector.
// Each function calls `dispatch::dispatch` which routes to the registered handler.
// The x86_64 crate requires each IDT entry to reference a distinct function.

macro_rules! irq_handler {
    ($name:ident, $vector:expr) => {
        extern "x86-interrupt" fn $name(_stack_frame: InterruptStackFrame) {
            unsafe {
                crate::interrupts::dispatch::dispatch($vector);
            }
        }
    };
}

irq_handler!(irq_handler_32, 32);
irq_handler!(irq_handler_33, 33);
irq_handler!(irq_handler_34, 34);
irq_handler!(irq_handler_35, 35);
irq_handler!(irq_handler_36, 36);
irq_handler!(irq_handler_37, 37);
irq_handler!(irq_handler_38, 38);
irq_handler!(irq_handler_39, 39);
irq_handler!(irq_handler_40, 40);
irq_handler!(irq_handler_41, 41);
irq_handler!(irq_handler_42, 42);
irq_handler!(irq_handler_43, 43);
irq_handler!(irq_handler_44, 44);
irq_handler!(irq_handler_45, 45);
irq_handler!(irq_handler_46, 46);
irq_handler!(irq_handler_47, 47);

// ─────────────────────────────────────────────────────────────────────────────
// Initialization
// ─────────────────────────────────────────────────────────────────────────────

/// Initialize and load the IDT.
///
/// Must be called AFTER `gdt::init()` — the double-fault handler uses an IST
/// entry that requires the TSS to be loaded, which `gdt::init()` does.
pub fn init() {
    let idt = IDT.call_once(|| {
        let mut idt = InterruptDescriptorTable::new();

        // ── CPU Exception Handlers ─────────────────────────────────────────

        // #BP — Breakpoint (INT3)
        idt.breakpoint.set_handler_fn(breakpoint_handler);

        // #DF — Double Fault
        // Uses IST stack for safety against stack overflow.
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(DOUBLE_FAULT_IST_INDEX);
        }

        // #GP — General Protection Fault
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);

        // #PF — Page Fault
        idt.page_fault.set_handler_fn(page_fault_handler);

        // #SS — Stack Segment Fault (vector 12)
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);

        // #TS — Invalid TSS (vector 10)
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);

        // #DE — Division Error
        idt.divide_error.set_handler_fn(divide_error_handler);

        // #UD — Invalid Opcode
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);

        // ── Hardware IRQ Handlers (vectors 32-47) ─────────────────────────
        // Each vector gets its own static handler function that dispatches
        // to the registered IRQ handler.

        // Vector 32 (PIT timer) uses a special naked handler for context switching.
        // This handler saves/restores all registers manually and calls the scheduler.
        // Convert the physical address (PIC relocation) to virtual for the VMM page tables.
        unsafe {
            let handler_phys = crate::process::context_switch::timer_interrupt_handler as *const () as u64;
            let handler_virt = crate::memory::phys_to_kernel_virt(handler_phys);
            idt[32].set_handler_addr(x86_64::VirtAddr::new(handler_virt));
        }
        idt[33].set_handler_fn(irq_handler_33);
        idt[34].set_handler_fn(irq_handler_34);
        idt[35].set_handler_fn(irq_handler_35);
        idt[36].set_handler_fn(irq_handler_36);
        idt[37].set_handler_fn(irq_handler_37);
        idt[38].set_handler_fn(irq_handler_38);
        idt[39].set_handler_fn(irq_handler_39);
        idt[40].set_handler_fn(irq_handler_40);
        idt[41].set_handler_fn(irq_handler_41);
        idt[42].set_handler_fn(irq_handler_42);
        idt[43].set_handler_fn(irq_handler_43);
        idt[44].set_handler_fn(irq_handler_44);
        idt[45].set_handler_fn(irq_handler_45);
        idt[46].set_handler_fn(irq_handler_46);
        idt[47].set_handler_fn(irq_handler_47);

        idt
    });

    // Load the IDT into the CPU via the `lidt` instruction.
    idt.load();
}

/// Reload the IDT into the CPU.
///
/// Call this after modifying the IDT (e.g., after registering new handlers).
pub fn reload() {
    if let Some(idt) = IDT.get() {
        idt.load();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Exception handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Handle INT3 breakpoints — log and continue.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::kprintln!(
        "[EXCEPTION] Breakpoint at {:#x}",
        stack_frame.instruction_pointer.as_u64()
    );
}

/// Handle Division Errors — fatal.
extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    crate::kprintln!(
        "[EXCEPTION] #DE Division Error at {:#x} — HALTING",
        stack_frame.instruction_pointer.as_u64()
    );
    crate::halt();
}

/// Handle Invalid Opcode — fatal.
extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    crate::kprintln!(
        "[EXCEPTION] #UD Invalid Opcode at {:#x} — HALTING",
        stack_frame.instruction_pointer.as_u64()
    );
    crate::halt();
}

/// Handle Stack Segment Faults — fatal with full diagnostics.
///
/// #SS has an error code. For Ring 0→Ring 0 (no CPL change), the CPU pushes:
/// ```text
/// [RSP+0]  = error code
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// ```
/// Common causes: bad SS selector, stack not present, stack limit violation.
extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("[EXCEPTION] #SS Stack Segment Fault");
    crate::kprintln!("  Error code : {:#x}", error_code);
    crate::kprintln!("  RIP        : {:#x}", stack_frame.instruction_pointer.as_u64());
    crate::kprintln!("  CS         : {:#x}", stack_frame.code_segment.0);
    crate::kprintln!("  RFLAGS     : {:#x}", stack_frame.cpu_flags.bits());
    crate::kprintln!("  HALTING");
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::halt();
}

/// Handle Invalid TSS — fatal with full diagnostics.
///
/// #TS has an error code. For Ring 0→Ring 0 (no CPL change), the CPU pushes:
/// ```text
/// [RSP+0]  = error code
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// ```
/// Common causes: bad TSS selector, TSS not present, bad segment in TSS.
extern "x86-interrupt" fn invalid_tss_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("[EXCEPTION] #TS Invalid TSS");
    crate::kprintln!("  Error code : {:#x}", error_code);
    crate::kprintln!("  RIP        : {:#x}", stack_frame.instruction_pointer.as_u64());
    crate::kprintln!("  CS         : {:#x}", stack_frame.code_segment.0);
    crate::kprintln!("  RFLAGS     : {:#x}", stack_frame.cpu_flags.bits());
    crate::kprintln!("  HALTING");
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::halt();
}

/// Handle General Protection Faults — fatal with full diagnostics.
///
/// #GP has an error code. For Ring 0→Ring 0 (no CPL change), the CPU pushes:
/// ```text
/// [RSP+0]  = error code
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// ```
/// The `stack_frame` reference points at [RSP+0] (the error code slot).
/// `stack_frame.instruction_pointer` reads [RSP+8] (correct).
/// `stack_frame.code_segment` reads [RSP+16] (correct).
/// `stack_frame.cpu_flags` reads [RSP+24] (correct).
/// `stack_frame.stack_pointer` and `stack_frame.stack_segment` are
/// invalid (garbage) when there is no CPL change.
extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let captured_rsp = unsafe { crate::CAPTURED_RSP };
    let cr3_val: u64;
    let cs_val: u16;
    let ss_val: u16;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3_val);
        core::arch::asm!("mov {}, cs", out(reg) cs_val);
        core::arch::asm!("mov {}, ss", out(reg) ss_val);
    }

    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("[EXCEPTION] #GP General Protection Fault");
    crate::kprintln!("  Error code : {:#x}", error_code);
    crate::kprintln!("  RIP (from CPU frame) : {:#x}", stack_frame.instruction_pointer.as_u64());
    crate::kprintln!("  CS  (from CPU frame) : {:#x}", stack_frame.code_segment.0);
    crate::kprintln!("  RFLAGS (CPU frame)   : {:#x}", stack_frame.cpu_flags.bits());
    crate::kprintln!("  CR3 (actual)          : {:#x}", cr3_val);
    crate::kprintln!("  CS  (actual selector) : {:#x}  (RPL={})", cs_val, cs_val & 3);
    crate::kprintln!("  SS  (actual selector) : {:#x}  (RPL={})", ss_val, ss_val & 3);
    crate::kprintln!("  CAPTURED_RSP (before iretq): {:#x}", captured_rsp);

    if captured_rsp != 0 {
        crate::kprintln!("  ── IRET frame at CAPTURED_RSP ──");
        for i in 0..3u64 {
            let addr = captured_rsp + i * 8;
            let val = unsafe { core::ptr::read_volatile(addr as *const u64) };
            let label = match i {
                0 => "  ← IRET RIP",
                1 => "  ← IRET CS",
                2 => "  ← IRET RFLAGS",
                _ => "",
            };
            crate::kprintln!("    [{:#x}] = {:#018x}{}", addr, val, label);
        }
        let cur_rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) cur_rsp); }
        crate::kprintln!("  Current RSP in GP handler: {:#x}", cur_rsp);
    }

    crate::kprintln!("  HALTING");
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::halt();
}

/// Handle Page Faults — fatal with full diagnostics.
///
/// #PF has an error code. For Ring 0→Ring 0 (no CPL change), the CPU pushes:
/// ```text
/// [RSP+0]  = error code (PageFaultErrorCode bitfield)
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// ```
/// CR2 contains the faulting virtual address.
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let faulting_addr = x86_64::registers::control::Cr2::read()
        .map(|v| v.as_u64())
        .unwrap_or(0);

    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("[EXCEPTION] #PF Page Fault");
    crate::kprintln!("  CR2 (faulting addr) : {:#x}", faulting_addr);
    crate::kprintln!("  Error flags         : {:?}", error_code);
    crate::kprintln!("  RIP                 : {:#x}", stack_frame.instruction_pointer.as_u64());
    crate::kprintln!("  CS                  : {:#x}", stack_frame.code_segment.0);
    crate::kprintln!("  RFLAGS              : {:#x}", stack_frame.cpu_flags.bits());
    crate::kprintln!("  HALTING");
    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::halt();
}

/// Handle Double Faults — fatal, always.
///
/// ## What the CPU provides
///
/// The `stack_frame` parameter is the x86_64 crate's `InterruptStackFrame`,
/// a reference to the frame the CPU pushed on the IST stack. We read it
/// DIRECTLY rather than computing IST offsets.
///
/// For Ring 0 → Ring 0 (no CPL change), the CPU pushes 4 qwords:
/// ```text
/// [RSP+0]  = error code (0 for DF)
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// ```
/// RSP and SS are NOT pushed (no CPL change).
///
/// For Ring 3 → Ring 0, RSP and SS ARE pushed (6 qwords total).
///
/// ## What if First RIP = 0?
///
/// Intel SDM Vol.3 §6.12.1.2: "If the double-fault detection mechanism is
/// activated because of a failure to deliver the exception, the processor
/// pushes a saved instruction pointer of 0."
///
/// This means the FIRST exception could not be delivered to its handler.
/// The CPU could not push the first exception's frame (e.g., the stack
/// was invalid, or the IDT entry was bad).
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    let cr2 = x86_64::registers::control::Cr2::read()
        .map(|v| v.as_u64())
        .unwrap_or(0);

    let cr3_val: u64;
    let cs_val: u16;
    let ss_val: u16;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3_val);
        core::arch::asm!("mov {}, cs", out(reg) cs_val);
        core::arch::asm!("mov {}, ss", out(reg) ss_val);
    }

    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("[EXCEPTION] *** DOUBLE FAULT ***");
    crate::kprintln!("  DF error code : {:#x}", error_code);

    // ── DF frame from CPU (on IST) ─────────────────────────────────────
    let frame_rip  = stack_frame.instruction_pointer.as_u64();
    let frame_cs   = stack_frame.code_segment.0;
    let frame_rflags = stack_frame.cpu_flags.bits();
    let frame_rsp  = stack_frame.stack_pointer.as_u64();
    let frame_ss   = stack_frame.stack_segment.0;

    crate::kprintln!("  ── DF frame (CPU-pushed on IST) ──");
    crate::kprintln!("  RIP     : {:#x}", frame_rip);
    crate::kprintln!("  CS      : {:#x}  (RPL={})", frame_cs, frame_cs & 3);
    crate::kprintln!("  RFLAGS  : {:#x}", frame_rflags);
    crate::kprintln!("  RSP     : {:#x}", frame_rsp);
    crate::kprintln!("  SS      : {:#x}", frame_ss);
    crate::kprintln!("  CR2     : {:#x}", cr2);
    crate::kprintln!("  CR3     : {:#x}", cr3_val);
    crate::kprintln!("  CS (actual) : {:#x}  (RPL={})", cs_val, cs_val & 3);
    crate::kprintln!("  SS (actual) : {:#x}  (RPL={})", ss_val, ss_val & 3);

    if frame_cs & 3 == 3 {
        crate::kprintln!("  DF CPL=3: RSP/SS in DF frame are from Ring 3 transition");
    } else {
        crate::kprintln!("  DF CPL=0: RSP/SS in DF frame may be garbage (no CPL change to DF)");
    }

    // ── CR2 analysis ───────────────────────────────────────────────────
    if cr2 == 0xFFFFFFFFFFFFFFF8 {
        crate::kprintln!("  CR2 = 0x...F8 = 0x0 - 8: push at RSP=0x0");
    } else if cr2 == 0 {
        crate::kprintln!("  CR2 = 0x0: null deref or RSP was 0");
    }

    // ════════════════════════════════════════════════════════════════════
    // Diagnostics captured by dump_frame_before_pop (before pops).
    // ════════════════════════════════════════════════════════════════════
    let saved_rsp       = unsafe { super::process::context_switch::SAVED_RSP };
    let rsp_after_load  = unsafe { super::process::context_switch::RSP_AFTER_LOAD };
    let rsp_before      = unsafe { super::process::context_switch::RSP_BEFORE_IRETQ };
    let expected        = unsafe { super::process::context_switch::EXPECTED_RSP };
    let cs_in_frame     = unsafe { super::process::context_switch::CS_IN_FRAME };
    let rip_in_frame    = unsafe { super::process::context_switch::RIP_IN_FRAME };

    crate::kprintln!("  ── Captured diagnostics ──");
    crate::kprintln!("  SAVED_RSP (into schedule)  : {:#x}", saved_rsp);
    crate::kprintln!("  RSP_AFTER_LOAD (mov rsp,r12): {:#x}", rsp_after_load);
    crate::kprintln!("  RIP_IN_FRAME (before pops) : {:#x}", rip_in_frame);
    crate::kprintln!("  CS_IN_FRAME  (before pops) : {:#x}  (RPL={})", cs_in_frame, cs_in_frame & 3);
    crate::kprintln!("  RSP_BEFORE_IRETQ           : {:#x}", rsp_before);
    crate::kprintln!("  EXPECTED_RSP (after iretq) : {:#x}", expected);

    // ════════════════════════════════════════════════════════════════════
    // Dump 20 qwords at the RSP the naked handler saved right before
    // iretq.  This is the ACTUAL memory the CPU consumed.
    // ════════════════════════════════════════════════════════════════════
    if rsp_before != 0 {
        crate::kprintln!("  ── 20 qwords at RSP_BEFORE_IRETQ={:#x} ──", rsp_before);
        for i in 0..20u64 {
            let addr = rsp_before + i * 8;
            let val = unsafe { core::ptr::read_volatile(addr as *const u64) };
            crate::kprintln!("    [{:#x}] = {:#x}{}", addr, val,
                match i {
                    0  => "  ← IRET RIP",
                    1  => if val & 3 == 0 { "  ← IRET CS RPL=0" }
                          else if val & 3 == 3 { "  ← IRET CS RPL=3 !!!" }
                          else { "  ← IRET CS" },
                    2  => "  ← IRET RFLAGS",
                    3  => "  ← [rsp+24] = new RSP if CPL change",
                    4  => "  ← [rsp+32] = new SS if CPL change",
                    _  => "",
                }
            );
        }

        // Cross-check: CS from frame dump vs CS from memory
        let cs_from_mem = unsafe { core::ptr::read_volatile((rsp_before + 8) as *const u64) };
        crate::kprintln!("  ── CS cross-check ──");
        crate::kprintln!("  CS_IN_FRAME (dumped before pops) : {:#x}", cs_in_frame);
        crate::kprintln!("  CS at RSP_BEFORE_IRETQ+8 (memory): {:#x}", cs_from_mem);
        if cs_in_frame == cs_from_mem {
            crate::kprintln!("  MATCH — frame was not corrupted between dump and iretq");
        } else {
            crate::kprintln!("  MISMATCH — frame WAS corrupted between dump and iretq!");
        }

        if cs_in_frame & 3 == 0 {
            crate::kprintln!("  CS RPL=0 → same-privilege iretq: pops RIP/CS/RFLAGS only (3×8=24)");
            crate::kprintln!("  New RSP = RSP_BEFORE_IRETQ + 24 = {:#x}", rsp_before + 24);
        } else if cs_in_frame & 3 == 3 {
            crate::kprintln!("  CS RPL=3 → outer-privilege iretq: pops RIP/CS/RFLAGS/RSP/SS (5×8=40)");
            crate::kprintln!("  New RSP from [rsp+24] = {:#x}",
                unsafe { core::ptr::read_volatile((rsp_before + 24) as *const u64) });
        } else {
            crate::kprintln!("  CS RPL={} → unexpected", cs_in_frame & 3);
        }
    } else {
        crate::kprintln!("  RSP_BEFORE_IRETQ = 0 → naked handler never saved it");
    }

    // ════════════════════════════════════════════════════════════════════
    // GDT selector values
    // ════════════════════════════════════════════════════════════════════
    crate::kprintln!("  ── GDT selectors ──");
    let kernel_cs = crate::gdt::kernel_code_selector();
    let user_cs   = crate::gdt::user_code_selector();
    let user_ss   = crate::gdt::user_data_selector();
    crate::kprintln!("  kernel_code : {:#06x} (idx={}, RPL={})", kernel_cs.0, kernel_cs.index(), kernel_cs.0 & 3);
    crate::kprintln!("  user_code   : {:#06x} (idx={}, RPL={})", user_cs.0, user_cs.index(), user_cs.0 & 3);
    crate::kprintln!("  user_data   : {:#06x} (idx={}, RPL={})", user_ss.0, user_ss.index(), user_ss.0 & 3);

    // ════════════════════════════════════════════════════════════════════
    // Frame origin
    // ════════════════════════════════════════════════════════════════════
    crate::kprintln!("  ── Frame origin ──");
    crate::kprintln!("  Timer handler: NO IST (vector 32 has no IST index set)");
    crate::kprintln!("  IRET frame: manually constructed by setup_initial_stack_frame_kernel");
    crate::kprintln!("  NOT a CPU-pushed interrupt frame — CS/RIP/RFLAGS written by Rust code");

    // ════════════════════════════════════════════════════════════════════
    // Naked task RSP: what the CPU actually delivered
    // ════════════════════════════════════════════════════════════════════
    crate::kprintln!("  ── Naked task RSP (CPU-delivered) ──");
    crate::kprintln!("  (task globals removed for minimal test)");

    crate::kprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    crate::kprintln!("  SYSTEM HALTED — cannot recover from double fault");
    crate::halt()
}
