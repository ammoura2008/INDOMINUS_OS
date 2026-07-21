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
use core::mem::MaybeUninit;

use crate::gdt::DOUBLE_FAULT_IST_INDEX;

// ─────────────────────────────────────────────────────────────────────────────
// IDT global state
// ─────────────────────────────────────────────────────────────────────────────

static mut IDT: MaybeUninit<InterruptDescriptorTable> = MaybeUninit::uninit();
static mut IDT_INITIALIZED: bool = false;

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
///
/// # IMPORTANT: PIC and virtual addresses
///
/// With PIC (position-independent code), function pointers contain physical
/// addresses after relocation. The IDT entries must contain virtual addresses
/// (the CPU jumps to virtual addresses). We convert each handler's physical
/// address to its kernel virtual address using `phys_to_kernel_virt`.
///
/// Similarly, the IDTR base must be a virtual address, because after CR3
/// switch to a user PML4 (no identity map), the physical address is unmapped.
pub fn init() {
    crate::serial::write_str("[MARK] idt::init start\n");

    // PROVE RFLAGS.IF state
    unsafe {
        let rflags: u64;
        core::arch::asm!("pushfq; pop {}", out(reg) rflags);
        crate::serial::write_str("[IDT] RFLAGS=0x");
        crate::serial::write_hex(rflags);
        crate::serial::write_str(" IF=");
        crate::serial::write_str(if rflags & (1 << 9) != 0 { "1 (ENABLED)" } else { "0 (DISABLED)" });
        crate::serial::write_nl();
    }

    // Helper: convert a handler fn pointer to a VirtAddr for the IDT
    macro_rules! handler_virt {
        ($handler:expr) => {{
            unsafe {
                let phys = $handler as *const () as u64;
                let virt = crate::memory::phys_to_kernel_virt(phys);
                x86_64::VirtAddr::new(virt)
            }
        }};
    }

    crate::serial::write_str("[MARK] idt::init creating IDT\n");
    let mut idt = InterruptDescriptorTable::new();
    crate::serial::write_str("[MARK] idt::init new() done\n");

    unsafe {
        crate::serial::write_str("[MARK] idt::init setting handlers\n");
        // ── CPU Exception Handlers ─────────────────────────────────────────

        // #BP — Breakpoint (INT3)
        idt.breakpoint.set_handler_addr(handler_virt!(breakpoint_handler));

        // #DF — Double Fault
        // Uses IST stack for safety against stack overflow.
        idt.double_fault
            .set_handler_addr(handler_virt!(double_fault_handler))
            .set_stack_index(DOUBLE_FAULT_IST_INDEX);

        // #GP — General Protection Fault
        idt.general_protection_fault.set_handler_addr(handler_virt!(general_protection_fault_handler));

        // #PF — Page Fault
        idt.page_fault.set_handler_addr(handler_virt!(page_fault_handler));

        // #SS — Stack Segment Fault (vector 12)
        idt.stack_segment_fault.set_handler_addr(handler_virt!(stack_segment_fault_handler));

        // #TS — Invalid TSS (vector 10)
        idt.invalid_tss.set_handler_addr(handler_virt!(invalid_tss_handler));

        // #DE — Division Error
        idt.divide_error.set_handler_addr(handler_virt!(divide_error_handler));

        // #UD — Invalid Opcode
        idt.invalid_opcode.set_handler_addr(handler_virt!(invalid_opcode_handler));

        crate::serial::write_str("[MARK] idt::init exception handlers done\n");

        // ── Hardware IRQ Handlers (vectors 32-47) ─────────────────────────

        // Vector 32 (PIT timer) uses a special naked handler for context switching.
        let handler_phys = crate::process::context_switch::timer_interrupt_handler as *const () as u64;
        let handler_virt_addr = crate::memory::phys_to_kernel_virt(handler_phys);
        idt[32].set_handler_addr(x86_64::VirtAddr::new(handler_virt_addr));
        idt[33].set_handler_addr(handler_virt!(irq_handler_33));
        idt[34].set_handler_addr(handler_virt!(irq_handler_34));
        idt[35].set_handler_addr(handler_virt!(irq_handler_35));
        idt[36].set_handler_addr(handler_virt!(irq_handler_36));
        idt[37].set_handler_addr(handler_virt!(irq_handler_37));
        idt[38].set_handler_addr(handler_virt!(irq_handler_38));
        idt[39].set_handler_addr(handler_virt!(irq_handler_39));
        idt[40].set_handler_addr(handler_virt!(irq_handler_40));
        idt[41].set_handler_addr(handler_virt!(irq_handler_41));
        idt[42].set_handler_addr(handler_virt!(irq_handler_42));
        idt[43].set_handler_addr(handler_virt!(irq_handler_43));
        idt[44].set_handler_addr(handler_virt!(irq_handler_44));
        idt[45].set_handler_addr(handler_virt!(irq_handler_45));
        idt[46].set_handler_addr(handler_virt!(irq_handler_46));
        idt[47].set_handler_addr(handler_virt!(irq_handler_47));
    }

    crate::serial::write_str("[MARK] idt::init handlers done\n");

    // Store IDT in static and get a 'static reference for lidt
    unsafe {
        IDT.as_mut_ptr().write(idt);
        IDT_INITIALIZED = true;
        let idt_ref = IDT.assume_init_ref();
        let idt_phys = idt_ref as *const InterruptDescriptorTable as u64;
        let idt_virt = crate::memory::phys_to_kernel_virt(idt_phys);
        crate::serial::write_str("[MARK] idt::init idt_phys=0x");
        crate::serial::write_hex(idt_phys);
        crate::serial::write_str(" idt_virt=0x");
        crate::serial::write_hex(idt_virt);
        crate::serial::write_nl();

        #[repr(C, packed)]
        struct Idtr { limit: u16, base: u64 }
        let idtr = Idtr {
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
            base: idt_virt,
        };
        core::arch::asm!("lidt [{}]", in(reg) &idtr as *const _ as u64, options(readonly, nostack));

        // Verify: read back IDTR
        let readback = Idtr { limit: 0, base: 0 };
        core::arch::asm!("sidt [{}]", in(reg) &readback as *const _ as u64, options(readonly, nostack));
        crate::serial::write_str("[IDTR] limit="); crate::serial::write_hex(readback.limit as u64);
        crate::serial::write_str(" base="); crate::serial::write_hex(readback.base); crate::serial::write_nl();
    }
    crate::serial::write_str("[MARK] idt::init done\n");
}

/// Reload the IDT into the CPU.
///
/// Call this after modifying the IDT (e.g., after registering new handlers).
pub fn reload() {
    if unsafe { IDT_INITIALIZED } {
        let idt = unsafe { IDT.assume_init_ref() };
        let idt_phys = idt as *const InterruptDescriptorTable as u64;
        let idt_virt = unsafe { crate::memory::phys_to_kernel_virt(idt_phys) };
        #[repr(C, packed)]
        struct Idtr { limit: u16, base: u64 }
        let idtr = Idtr {
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
            base: idt_virt,
        };
        unsafe {
            core::arch::asm!("lidt [{}]", in(reg) &idtr as *const _ as u64, options(readonly, nostack));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Exception handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Handle INT3 breakpoints — log and continue.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    use crate::serial::{write_str, write_hex, write_nl};
    write_str("[EXCEPTION] Breakpoint at 0x");
    write_hex(stack_frame.instruction_pointer.as_u64());
    write_nl();
}

/// Handle Division Errors — fatal.
extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    use crate::serial::{write_str, write_hex, write_nl};
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] #DE Division Error at 0x");
    write_hex(stack_frame.instruction_pointer.as_u64());
    write_str(" — HALTING\n");
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    crate::halt();
}

/// Handle Invalid Opcode — fatal.
extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    use crate::serial::{write_str, write_hex, write_nl};
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] #UD Invalid Opcode at 0x");
    write_hex(stack_frame.instruction_pointer.as_u64());
    write_str(" — HALTING\n");
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    crate::halt();
}

/// Handle Stack Segment Faults — fatal with full diagnostics.
extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    use crate::serial::{write_str, write_hex, write_nl};
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] #SS Stack Segment Fault\n");
    write_str("  Error code : 0x"); write_hex(error_code); write_nl();
    write_str("  RIP        : 0x"); write_hex(stack_frame.instruction_pointer.as_u64()); write_nl();
    write_str("  CS         : 0x"); write_hex(stack_frame.code_segment.0 as u64); write_nl();
    write_str("  RFLAGS     : 0x"); write_hex(stack_frame.cpu_flags.bits()); write_nl();
    write_str("  HALTING\n");
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    crate::halt();
}

/// Handle Invalid TSS — fatal with full diagnostics.
extern "x86-interrupt" fn invalid_tss_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    use crate::serial::{write_str, write_hex, write_nl};
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] #TS Invalid TSS\n");
    write_str("  Error code : 0x"); write_hex(error_code); write_nl();
    write_str("  RIP        : 0x"); write_hex(stack_frame.instruction_pointer.as_u64()); write_nl();
    write_str("  CS         : 0x"); write_hex(stack_frame.code_segment.0 as u64); write_nl();
    write_str("  RFLAGS     : 0x"); write_hex(stack_frame.cpu_flags.bits()); write_nl();
    write_str("  HALTING\n");
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    crate::halt();
}

/// Handle General Protection Faults — fatal with full diagnostics.
///
/// # IMPORTANT: NO format_args! after CR3 switch
///
/// With PIC (position-independent code), `format_args!` creates function
/// pointers containing **physical** addresses. After CR3 switch to a user
/// PML4 (no identity map), calling through those pointers causes a page
/// fault at the physical address (unmapped). We use write_str/write_hex
/// exclusively — they are direct function calls that work at virtual
/// addresses regardless of PIC.
/// Handle General Protection Faults — kill user processes, halt on kernel faults.
///
/// This is a naked handler (like page_fault_handler) so we can:
/// 1. Read CS.RPL from the interrupt frame to classify user vs kernel fault
/// 2. For user faults: call kill_process(), then context-switch via
///    gp_return_to_user (EOI + stack switch + iretq)
/// 3. For kernel faults: halt with diagnostics
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn general_protection_fault_handler() {
    core::arch::naked_asm!(
        // ═══ Save GP registers (same order as timer/page fault handler) ═══
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push r11",
        "push r10",
        "push r9",
        "push r8",
        "push rbp",
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push rbx",
        "push rax",

        // ═══ Read CS from the interrupt frame for classification ═══
        // Stack: [0..14]=GP regs (15 qwords), [15]=error_code, [16]=RIP, [17]=CS
        "mov rdi, rsp",          // rdi = frame pointer
        "mov rsi, [rsp + 120]",  // rsi = error_code (offset 15*8)
        "mov rdx, [rsp + 136]",  // rdx = CS (offset 17*8)
        "call {gp_classify}",
        // rax = 0 for kernel fault (halt), 1 for user fault (kill + context switch)

        "cmp rax, 0",
        "je .gp_kernel_fault",

        // ═══ User fault: kill process and context switch ═══
        "call {kill_process}",
        // rax = new process's stack pointer
        "mov rdi, rax",
        "call {gp_return_to_user}",
        // Never returns here

        // ═══ Kernel fault: print diagnostics and halt ═══
        ".gp_kernel_fault:",
        "call {gp_print_diag}",
        "call {halt}",

        gp_classify = sym gp_classify,
        kill_process = sym crate::process::context_switch::kill_process,
        gp_return_to_user = sym gp_return_to_user,
        gp_print_diag = sym gp_print_diag,
        halt = sym crate::halt,
    );
}

/// Classify a GP fault as user-mode (kill process) or kernel-mode (halt).
///
/// Returns rax = 1 for user fault, rax = 0 for kernel fault.
#[no_mangle]
unsafe extern "C" fn gp_classify(frame: u64, error_code: u64, cs: u64) -> u64 {
    use crate::serial::{write_str, write_hex, write_nl, write_u64};

    let rpl = cs & 3;
    // Frame: [0..14]=GP regs, [15]=error_code, [16]=RIP, [17]=CS
    let rip = core::ptr::read_volatile((frame + 16 * 8) as *const u64);

    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] #GP General Protection Fault\n");
    write_str("  Error code : 0x"); write_hex(error_code); write_nl();
    write_str("  RIP        : 0x"); write_hex(rip); write_nl();
    write_str("  CS         : 0x"); write_hex(cs);
    write_str(" (RPL="); write_u64(rpl); write_str(")\n");

    if rpl == 3 {
        write_str("  USER FAULT: killing process\n");
        1
    } else {
        write_str("  KERNEL FAULT: halting\n");
        0
    }
}

/// Print additional diagnostics for kernel GP faults (before halting).
#[no_mangle]
unsafe extern "C" fn gp_print_diag() {
    use crate::serial::{write_str, write_hex, write_nl};

    let cr3_val: u64;
    let rsp_val: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3_val);
    core::arch::asm!("mov {}, rsp", out(reg) rsp_val);

    write_str("  CR3       : 0x"); write_hex(cr3_val); write_nl();
    write_str("  RSP       : 0x"); write_hex(rsp_val); write_nl();
    write_str("  HALTING\n");
    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
}

/// Return to user mode after killing a GP-faulting process.
///
/// Called from the naked GP handler with rdi = new process's stack pointer.
/// Does EOI + stack switch + iretq.
#[no_mangle]
unsafe extern "C" fn gp_return_to_user(new_rsp: u64) -> ! {
    // Send EOI to LAPIC (using upper-half mapping)
    core::ptr::write_volatile(0xFFFFFFFF_FEE0_00B0 as *mut u32, 0);

    // Switch to new process's stack and restore its context
    core::arch::asm!(
        "mov rsp, {new_rsp}",
        // Restore 15 GP registers
        "pop rax",
        "pop rbx",
        "pop rcx",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop rbp",
        "pop r8",
        "pop r9",
        "pop r10",
        "pop r11",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",
        // Return from interrupt (reads IRET frame from stack)
        "iretq",
        new_rsp = in(reg) new_rsp,
        options(noreturn),
    );
}

/// Handle Page Faults — classify and kill user processes, halt on kernel faults.
///
/// #PF has an error code. For Ring 3→Ring 0 (CPL change), the CPU pushes:
/// ```text
/// [RSP+0]  = error code (PageFaultErrorCode bitfield)
/// [RSP+8]  = RIP
/// [RSP+16] = CS
/// [RSP+24] = RFLAGS
/// [RSP+32] = RSP (user)
/// [RSP+40] = SS  (user)
/// ```
/// CR2 contains the faulting virtual address.
///
/// For Ring 0→Ring 0 (no CPL change), only 4 qwords are pushed (no RSP/SS).
///
/// This is a naked handler because we need to:
/// 1. Read CR2 and CS for classification
/// 2. Print diagnostics (on the faulting process's kernel stack)
/// 3. For user faults: call kill_process(), then context-switch via
///    page_fault_return_to_user (which does EOI + stack switch + iretq)
/// 4. For kernel faults: halt (no recovery)
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn page_fault_handler() {
    core::arch::naked_asm!(
        // ═══ Save GP registers (same order as timer handler) ═══
        // Push R15 first (highest addr) → RAX last (lowest = RSP)
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push r11",
        "push r10",
        "push r9",
        "push r8",
        "push rbp",
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push rbx",
        "push rax",

        // ═══ Read CR2 and CS for classification ═══
        "mov rdi, rsp",          // rdi = frame pointer (GP regs base)
        "mov rsi, cr2",          // rsi = faulting address
        // Read CS from the interrupt frame pushed by CPU
        // Stack: [0]=rax ... [14]=r15, [15]=error_code, [16]=RIP, [17]=CS, [18]=RFLAGS, ...
        "mov rdx, [rsp + 136]",  // rdx = CS (offset 17*8 = 136 bytes from GP frame base)
        "call {page_fault_classify}",
        // rax = 0 for kernel fault (halt), 1 for user fault (kill + context switch)

        // ═══ Check classification result ═══
        "cmp rax, 0",
        "je .kernel_fault",

        // ═══ User fault: kill process and context switch ═══
        "call {kill_process}",
        // rax = new process's stack pointer
        "mov rdi, rax",
        "call {page_fault_return_to_user}",
        // page_fault_return_to_user does EOI + stack switch + iretq (never returns here)

        // ═══ Kernel fault: halt ═══
        ".kernel_fault:",
        "call {halt}",

        page_fault_classify = sym page_fault_classify,
        kill_process = sym crate::process::context_switch::kill_process,
        page_fault_return_to_user = sym crate::process::context_switch::page_fault_return_to_user,
        halt = sym crate::halt,
    );
}

/// Classify a page fault as user-mode (kill process) or kernel-mode (halt).
///
/// Called from the naked page_fault_handler with:
/// - rdi = frame pointer (saved GP regs on stack)
/// - rsi = CR2 (faulting address)
/// - rdx = CS selector
///
/// Returns:
/// - rax = 1 → user fault (caller should kill process + context switch)
/// - rax = 0 → kernel fault (caller should halt)
///
/// Also prints diagnostic information.
#[no_mangle]
unsafe extern "C" fn page_fault_classify(frame: u64, faulting_addr: u64, cs: u64) -> u64 {
    let rpl = cs & 3;
    // Frame layout: [0..14]=GP regs (15 qwords), [15]=error_code, [16]=RIP, [17]=CS
    let rip = core::ptr::read_volatile((frame + 16 * 8) as *const u64); // RIP slot
    let error_code = core::ptr::read_volatile((frame + 15 * 8) as *const u64); // error code

    crate::serial::write_str_nl("!! PAGE FAULT !!");
    crate::serial::write_str("  CR2="); crate::serial::write_hex(faulting_addr); crate::serial::write_nl();
    crate::serial::write_str("  RIP="); crate::serial::write_hex(rip); crate::serial::write_nl();
    crate::serial::write_str("  CS=");  crate::serial::write_hex(cs);
    crate::serial::write_str(" RPL="); crate::serial::write_hex(rpl); crate::serial::write_nl();
    crate::serial::write_str("  err="); crate::serial::write_hex(error_code);
    if error_code & 1 != 0 { crate::serial::write_str(" prot_viol"); }
    if error_code & 2 != 0 { crate::serial::write_str(" write"); } else { crate::serial::write_str(" read"); }
    if error_code & 4 != 0 { crate::serial::write_str(" user"); }
    if error_code & 8 != 0 { crate::serial::write_str(" NX"); }
    if error_code & 0x10 != 0 { crate::serial::write_str(" reserved"); }
    crate::serial::write_nl();

    if rpl == 3 {
        // User-mode fault: kill the process, continue with next
        crate::serial::write_str_nl("  USER FAULT: killing process");
        1 // return value → caller will kill + context switch
    } else {
        // Kernel-mode fault: unrecoverable
        crate::serial::write_str_nl("  KERNEL FAULT: halting");
        0 // return value → caller will halt
    }
}

/// Handle Double Faults — fatal, always.
///
/// # IMPORTANT: NO format_args! after CR3 switch
///
/// With PIC, `format_args!` creates function pointers containing physical
/// addresses. After CR3 switch to a user PML4 (no identity map), calling
/// through those pointers causes a page fault. We use write_str/write_hex
/// exclusively — they are direct function calls that work at virtual
/// addresses regardless of PIC.
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    use crate::serial::{write_str, write_hex, write_nl, write_u64};

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

    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("[EXCEPTION] *** DOUBLE FAULT ***\n");
    write_str("  DF error code : 0x"); write_hex(error_code); write_nl();

    let frame_rip  = stack_frame.instruction_pointer.as_u64();
    let frame_cs   = stack_frame.code_segment.0;
    let frame_rflags = stack_frame.cpu_flags.bits();
    let frame_rsp  = stack_frame.stack_pointer.as_u64();
    let frame_ss   = stack_frame.stack_segment.0;

    write_str("  ── DF frame (CPU-pushed on IST) ──\n");
    write_str("  RIP     : 0x"); write_hex(frame_rip); write_nl();
    write_str("  CS      : 0x"); write_hex(frame_cs as u64);
    write_str("  (RPL="); write_u64((frame_cs & 3) as u64); write_str(")\n");
    write_str("  RFLAGS  : 0x"); write_hex(frame_rflags); write_nl();
    write_str("  RSP     : 0x"); write_hex(frame_rsp); write_nl();
    write_str("  SS      : 0x"); write_hex(frame_ss as u64); write_nl();
    write_str("  CR2     : 0x"); write_hex(cr2); write_nl();
    write_str("  CR3     : 0x"); write_hex(cr3_val); write_nl();
    write_str("  CS (actual) : 0x"); write_hex(cs_val as u64);
    write_str("  (RPL="); write_u64((cs_val & 3) as u64); write_str(")\n");
    write_str("  SS (actual) : 0x"); write_hex(ss_val as u64);
    write_str("  (RPL="); write_u64((ss_val & 3) as u64); write_str(")\n");

    if frame_cs & 3 == 3 {
        write_str("  DF CPL=3: RSP/SS in DF frame are from Ring 3 transition\n");
    } else {
        write_str("  DF CPL=0: RSP/SS in DF frame may be garbage (no CPL change to DF)\n");
    }

    if cr2 == 0xFFFFFFFFFFFFFFF8 {
        write_str("  CR2 = 0x...F8 = 0x0 - 8: push at RSP=0x0\n");
    } else if cr2 == 0 {
        write_str("  CR2 = 0x0: null deref or RSP was 0\n");
    }

    #[cfg(DEBUG_KERNEL)]
    {
        use crate::serial::{write_str, write_hex, write_nl, write_u64};

        let saved_rsp       = unsafe { super::process::context_switch::SAVED_RSP };
        let rsp_after_load  = unsafe { super::process::context_switch::RSP_AFTER_LOAD };
        let rsp_before      = unsafe { super::process::context_switch::RSP_BEFORE_IRETQ };
        let expected        = unsafe { super::process::context_switch::EXPECTED_RSP };
        let cs_in_frame     = unsafe { super::process::context_switch::CS_IN_FRAME };
        let rip_in_frame    = unsafe { super::process::context_switch::RIP_IN_FRAME };

        write_str("  ── Captured diagnostics ──\n");
        write_str("  SAVED_RSP (into schedule)  : 0x"); write_hex(saved_rsp); write_nl();
        write_str("  RSP_AFTER_LOAD (mov rsp,r12): 0x"); write_hex(rsp_after_load); write_nl();
        write_str("  RIP_IN_FRAME (before pops) : 0x"); write_hex(rip_in_frame); write_nl();
        write_str("  CS_IN_FRAME  (before pops) : 0x"); write_hex(cs_in_frame);
        write_str("  (RPL="); write_u64(cs_in_frame & 3); write_str(")\n");
        write_str("  RSP_BEFORE_IRETQ           : 0x"); write_hex(rsp_before); write_nl();
        write_str("  EXPECTED_RSP (after iretq) : 0x"); write_hex(expected); write_nl();

        if rsp_before != 0 {
            write_str("  ── 20 qwords at RSP_BEFORE_IRETQ=0x"); write_hex(rsp_before); write_str(" ──\n");
            for i in 0..20u64 {
                let addr = rsp_before + i * 8;
                let val = unsafe { core::ptr::read_volatile(addr as *const u64) };
                write_str("    [0x"); write_hex(addr); write_str("] = 0x"); write_hex(val);
                match i {
                    0  => write_str("  ← IRET RIP\n"),
                    1  => {
                        write_str("  ← IRET CS (RPL="); write_u64(val & 3);
                        if val & 3 == 0 { write_str(") RPL=0!\n"); }
                        else if val & 3 == 3 { write_str(") RPL=3\n"); }
                        else { write_str(")\n"); }
                    }
                    2  => write_str("  ← IRET RFLAGS\n"),
                    3  => write_str("  ← [rsp+24] = new RSP if CPL change\n"),
                    4  => write_str("  ← [rsp+32] = new SS if CPL change\n"),
                    _  => write_str("\n"),
                }
            }

            let cs_from_mem = unsafe { core::ptr::read_volatile((rsp_before + 8) as *const u64) };
            write_str("  ── CS cross-check ──\n");
            write_str("  CS_IN_FRAME (dumped before pops) : 0x"); write_hex(cs_in_frame); write_nl();
            write_str("  CS at RSP_BEFORE_IRETQ+8 (memory): 0x"); write_hex(cs_from_mem); write_nl();
            if cs_in_frame == cs_from_mem {
                write_str("  MATCH — frame was not corrupted between dump and iretq\n");
            } else {
                write_str("  MISMATCH — frame WAS corrupted between dump and iretq!\n");
            }

            if cs_in_frame & 3 == 0 {
                write_str("  CS RPL=0 → same-privilege iretq: pops RIP/CS/RFLAGS only (3*8=24)\n");
                write_str("  New RSP = RSP_BEFORE_IRETQ + 24 = 0x"); write_hex(rsp_before + 24); write_nl();
            } else if cs_in_frame & 3 == 3 {
                write_str("  CS RPL=3 → outer-privilege iretq: pops RIP/CS/RFLAGS/RSP/SS (5*8=40)\n");
                write_str("  New RSP from [rsp+24] = 0x");
                write_hex(unsafe { core::ptr::read_volatile((rsp_before + 24) as *const u64) });
                write_nl();
            } else {
                write_str("  CS RPL="); write_u64(cs_in_frame & 3); write_str(" → unexpected\n");
            }
        } else {
            write_str("  RSP_BEFORE_IRETQ = 0 → naked handler never saved it\n");
        }

        write_str("  ── GDT selectors ──\n");
        let kernel_cs = crate::gdt::kernel_code_selector();
        let user_cs   = crate::gdt::user_code_selector();
        let user_ss   = crate::gdt::user_data_selector();
        write_str("  kernel_code : 0x"); write_hex(kernel_cs.0 as u64);
        write_str(" (idx="); write_u64(kernel_cs.index() as u64);
        write_str(" RPL="); write_u64((kernel_cs.0 & 3) as u64); write_str(")\n");
        write_str("  user_code   : 0x"); write_hex(user_cs.0 as u64);
        write_str(" (idx="); write_u64(user_cs.index() as u64);
        write_str(" RPL="); write_u64((user_cs.0 & 3) as u64); write_str(")\n");
        write_str("  user_data   : 0x"); write_hex(user_ss.0 as u64);
        write_str(" (idx="); write_u64(user_ss.index() as u64);
        write_str(" RPL="); write_u64((user_ss.0 & 3) as u64); write_str(")\n");

        write_str("  ── Frame origin ──\n");
        write_str("  Timer handler: NO IST (vector 32 has no IST index set)\n");
        write_str("  IRET frame: manually constructed by setup_initial_stack_frame_kernel\n");
        write_str("  NOT a CPU-pushed interrupt frame — CS/RIP/RFLAGS written by Rust code\n");
    }

    write_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    write_str("  SYSTEM HALTED — cannot recover from double fault\n");
    crate::halt()
}
