//! # Context Switch
//!
//! The naked timer interrupt handler that performs context switching.
//!
//! ## Invariant: the CPU-consumed frame
//!
//! The IRET frame on the new task's stack is MANUALLY constructed by
//! `setup_initial_stack_frame_kernel`.  It is NOT a CPU-pushed interrupt
//! frame.  The naked handler reads it via `dump_frame_before_pop` BEFORE
//! the pops, then the 15 pops consume it, then `iretq` reads it.
//!
//! After the last pop and before iretq there are NO calls, pushes, or
//! stack modifications of any kind.

/// RSP passed into schedule() (the old task's frame base).
pub static mut SAVED_RSP: u64 = 0;

/// RSP immediately after `mov rsp, r12` (the new task's frame base).
pub static mut RSP_AFTER_LOAD: u64 = 0;

/// RSP after 15 pops, before iretq (= RSP_AFTER_LOAD + 15*8).
/// This is the address the CPU reads the IRET frame from.
pub static mut RSP_BEFORE_IRETQ: u64 = 0;

/// Expected RSP after iretq (= RSP_AFTER_LOAD + 18*8).
pub static mut EXPECTED_RSP: u64 = 0;

/// CS slot value from the IRET frame (read before pops).
pub static mut CS_IN_FRAME: u64 = 0;

/// RIP slot value from the IRET frame (read before pops).
pub static mut RIP_IN_FRAME: u64 = 0;

/// IRET frame values captured by diagnostics (for DF handler).
pub static mut IRET_RIP: u64 = 0;
pub static mut IRET_CS: u64 = 0;
pub static mut IRET_RFLAGS: u64 = 0;
pub static mut IRET_RSP_VAL: u64 = 0;
pub static mut IRET_SS: u64 = 0;

// Marker strings as static byte arrays.
#[no_mangle]
static TIMER_MSG: [u8; 15] = *b"[TIMER] entered";
#[no_mangle]
static SCHED_MSG: [u8; 24] = *b"[SCHED] calling schedule";
#[no_mangle]
static CTX_MSG: [u8; 26] = *b"[CTX] -- context switch --";
#[no_mangle]
static OLD_RSP_MSG: [u8; 14] = *b"[CTX] old_rsp=";
#[no_mangle]
static NEW_RSP_MSG: [u8; 14] = *b"[CTX] new_rsp=";
#[no_mangle]
static RETURNED_MSG: [u8; 14] = *b"[CTX] returned";

/// Helper: write_marker_raw(ptr, len) — writes raw bytes to serial.
/// write_hex(value) — writes hex to serial.
/// write_byte(byte) — writes one byte to serial.
/// All #[no_mangle] for use from naked_asm via sym.

/// The naked timer interrupt handler (vector 32).
///
/// Runs with interrupts DISABLED (interrupt gate).
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // ── [TIMER] marker ──────────────────────────────────────────────
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "lea rdi, [rip + {timer_msg}]",
        "mov rsi, {timer_len}",
        "call {write_marker}",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",

        // ── Save current process's registers ─────────────────────────────
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // ── [SCHED] marker ──────────────────────────────────────────────
        "lea rdi, [rip + {sched_msg}]",
        "mov rsi, {sched_len}",
        "call {write_marker}",

        // ── Call schedule(saved_rsp: u64) → returns new SP in RAX ────────
        "mov rdi, rsp",
        "call {schedule}",
        // RAX = new process's saved RSP

        // ── [CTX] marker: context switch ────────────────────────────────
        "push rax",
        "lea rdi, [rip + {ctx_msg}]",
        "mov rsi, {ctx_len}",
        "call {write_marker}",
        "pop rax",

        // ── [CTX] marker: old_rsp ───────────────────────────────────────
        "push rax",
        "lea rdi, [rip + {old_rsp_msg}]",
        "mov rsi, {old_rsp_len}",
        "call {write_marker}",
        "lea rdi, [rip + {saved_rsp}]",
        "mov rdi, [rdi]",
        "call {write_hex}",
        "mov rdi, 0x0A",
        "call {write_byte}",
        "pop rax",

        // ── [CTX] marker: new_rsp ───────────────────────────────────────
        "push rax",
        "lea rdi, [rip + {new_rsp_msg}]",
        "mov rsi, {new_rsp_len}",
        "call {write_marker}",
        "mov rdi, [rsp]",
        "call {write_hex}",
        "mov rdi, 0x0A",
        "call {write_byte}",
        "pop rax",

        // ── [CTX] marker: returned ──────────────────────────────────────
        "push rax",
        "lea rdi, [rip + {returned_msg}]",
        "mov rsi, {returned_len}",
        "call {write_marker}",
        "pop rax",

        // ── Save new SP into r12 BEFORE EOI ──────────────────────────────
        "mov r12, rax",

        // ── Send EOI to LAPIC ────────────────────────────────────────────
        "mov rax, 0xFEE000B0",
        "mov dword ptr [rax], 0",

        // ── Switch to new process's stack ────────────────────────────────
        "mov rsp, r12",

        // ── Restore new process's registers ──────────────────────────────
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",

        // ── Diagnostics before iretq ─────────────────────────────────
        // RSP at this point = post-pop IRET frame base (RIP slot).
        // The 15 popped GP reg slots below RSP are dead (already consumed).
        // Use slot at [RSP-16] (rbx, dead) as GDTR scratch.
        //   GDTR = 10 bytes: [RSP-16..RSP-7] → limit(2B) + base(8B)
        //   RDI = IRET frame base (RSP itself)
        //   RSI = GDTR base = [RSP-14]
        "sgdt [rsp - 16]",          // GDTR → rsp-16 (dead rbx slot area)
        "lea rdi, [rsp]",           // RDI = IRET frame base
        "mov rsi, [rsp - 14]",      // RSI = GDTR base (8 bytes at rsp-14)

        // Align RSP to 16 bytes for call.
        // frame is 8-mod-16 (stack_top is 16-aligned, frame = stack_top-40).
        // sub 8 → RSP is 16-aligned → call pushes 8 → callee gets RSP%16=8.
        "sub rsp, 8",
        "call {cpl_diag}",
        "add rsp, 8",

        // ── Return from interrupt ────────────────────────────────────────
        // NO calls, pushes, or modifications between last pop and iretq.
        "iretq",

        schedule = sym crate::process::context_switch::schedule,
        cpl_diag = sym crate::process::context_switch::print_iret_cpl_diagnostics,
        write_marker = sym crate::serial::write_marker_raw,
        write_hex = sym crate::serial::write_hex,
        write_byte = sym crate::serial::write_byte,
        timer_msg = sym TIMER_MSG,
        timer_len = const TIMER_MSG.len(),
        sched_msg = sym SCHED_MSG,
        sched_len = const SCHED_MSG.len(),
        ctx_msg = sym CTX_MSG,
        ctx_len = const CTX_MSG.len(),
        old_rsp_msg = sym OLD_RSP_MSG,
        old_rsp_len = const OLD_RSP_MSG.len(),
        new_rsp_msg = sym NEW_RSP_MSG,
        new_rsp_len = const NEW_RSP_MSG.len(),
        returned_msg = sym RETURNED_MSG,
        returned_len = const RETURNED_MSG.len(),
        saved_rsp = sym SAVED_RSP,
    );
}

/// Dump the IRET frame and GDT entries for diagnosis.
///
/// Called from naked timer handler with:
///   rdi = iret_frame (address of the 5-qword IRET frame)
///   rsi = gdt_base (GDT base address from GDTR)
///
/// # Safety
/// Must only be called from the naked timer handler with valid pointers.
#[no_mangle]
pub unsafe extern "C" fn print_iret_cpl_diagnostics(iret_frame: u64, gdt_base: u64) {
    use crate::serial::{write_str, write_hex, write_nl};

    // 1. Read current CS register
    let cs_reg: u64;
    core::arch::asm!("mov {0}, cs", out(reg) cs_reg);

    // 2. Read the exact 5 qwords the CPU will consume via iretq
    let f = [
        core::ptr::read_volatile(iret_frame as *const u64),         // [0] RIP
        core::ptr::read_volatile((iret_frame + 8) as *const u64),   // [1] CS
        core::ptr::read_volatile((iret_frame + 16) as *const u64),  // [2] RFLAGS
        core::ptr::read_volatile((iret_frame + 24) as *const u64),  // [3] RSP
        core::ptr::read_volatile((iret_frame + 32) as *const u64),  // [4] SS
    ];

    // Store in globals for DF handler
    IRET_RIP      = f[0];
    IRET_CS       = f[1];
    IRET_RFLAGS   = f[2];
    IRET_RSP_VAL  = f[3];
    IRET_SS       = f[4];

    // ── RSP and CS immediately before iretq ───────────────────────
    write_str("[CPU] RSP="); write_hex(iret_frame);
    write_str(" CS="); write_hex(cs_reg);
    write_str(" CPL="); write_hex(cs_reg & 3); write_nl();

    // ── The exact 5 qwords consumed by iretq ─────────────────────
    write_str("[F0] RIP   = 0x"); write_hex(f[0]); write_nl();
    write_str("[F1] CS    = 0x"); write_hex(f[1]);
    write_str(" RPL="); write_hex(f[1] & 3); write_nl();
    write_str("[F2] RFLAGS= 0x"); write_hex(f[2]); write_nl();
    write_str("[F3] RSP   = 0x"); write_hex(f[3]); write_nl();
    write_str("[F4] SS    = 0x"); write_hex(f[4]);
    write_str(" RPL="); write_hex(f[4] & 3); write_nl();

    // ── GDT descriptors for 0x08, 0x10, 0x28 ────────────────────
    write_str("[GDT] base=0x"); write_hex(gdt_base); write_nl();
    dump_gdt(gdt_base, 0x08);
    dump_gdt(gdt_base, 0x10);
    dump_gdt(gdt_base, 0x28);

    // ── Compare frame CS/SS vs GDT lookup ────────────────────────
    let ci = ((f[1] >> 3) & 0x1FFF) as u64;
    let si = ((f[4] >> 3) & 0x1FFF) as u64;

    write_str("[CS] sel=0x"); write_hex(f[1]); write_str(" idx="); write_hex(ci);
    dump_gdt_entry_info(gdt_base, ci);
    write_nl();

    write_str("[SS] sel=0x"); write_hex(f[4]); write_str(" idx="); write_hex(si);
    dump_gdt_entry_info(gdt_base, si);
    write_nl();

    write_str("── END ──\n");
}

/// Print one GDT entry: selector, raw lo/hi, DPL, type, present.
unsafe fn dump_gdt(gdt_base: u64, sel: u64) {
    use crate::serial::{write_str, write_hex, write_nl};
    let idx = ((sel >> 3) & 0x1FFF) as u64;
    let lo = core::ptr::read_volatile((gdt_base + idx * 8) as *const u32);
    let hi = core::ptr::read_volatile((gdt_base + idx * 8 + 4) as *const u32);
    let access = (hi >> 8) as u8;
    let seg_type = access & 0x0F;
    write_str("  [0x"); write_hex(sel);
    write_str("] lo=0x"); write_hex(lo as u64);
    write_str(" hi=0x"); write_hex(hi as u64);
    write_str(" DPL="); write_hex(((access >> 5) & 3) as u64);
    write_str(" T=0x"); write_hex(seg_type as u64);
    write_str(" P="); write_hex(((access >> 7) & 1) as u64);
    write_nl();
}

/// Print DPL/type/present for a GDT entry looked up by index.
unsafe fn dump_gdt_entry_info(gdt_base: u64, idx: u64) {
    use crate::serial::{write_str, write_hex};
    if idx >= 128 {
        write_str(" OUT_OF_BOUNDS");
        return;
    }
    let lo = core::ptr::read_volatile((gdt_base + idx * 8) as *const u32);
    let hi = core::ptr::read_volatile((gdt_base + idx * 8 + 4) as *const u32);
    let access = (hi >> 8) as u8;
    write_str(" DPL="); write_hex(((access >> 5) & 3) as u64);
    write_str(" T=0x"); write_hex((access & 0x0F) as u64);
    write_str(" P="); write_hex(((access >> 7) & 1) as u64);
}

/// Inspect the new task's frame AFTER `mov rsp, r12`, BEFORE any pops.
///
/// This is the only diagnostic call between stack switch and pops.
/// It runs before the 15 pops, so it does not consume the IRET frame.
#[no_mangle]
pub unsafe extern "C" fn dump_frame_before_pop(rsp: u64) {
    RSP_AFTER_LOAD = rsp;
    RSP_BEFORE_IRETQ = rsp + 15 * 8;
    EXPECTED_RSP = rsp + 18 * 8;

    // Read the IRET frame slots (RIP, CS, RFLAGS) before pops consume them.
    let iret_rip   = core::ptr::read_volatile((rsp + 15 * 8) as *const u64);
    let iret_cs    = core::ptr::read_volatile((rsp + 16 * 8) as *const u64);
    let iret_rflags = core::ptr::read_volatile((rsp + 17 * 8) as *const u64);

    RIP_IN_FRAME = iret_rip;
    CS_IN_FRAME  = iret_cs;

    crate::serial::write_str("[FRAME] -- New task frame at ");
    crate::serial::write_hex(rsp);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] RSP after load  = ");
    crate::serial::write_hex(rsp);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] RIP slot addr  = ");
    crate::serial::write_hex(rsp + 15 * 8);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] CS  slot addr  = ");
    crate::serial::write_hex(rsp + 16 * 8);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] RIP slot value = ");
    crate::serial::write_hex(iret_rip);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] CS  slot value = ");
    crate::serial::write_hex(iret_cs);
    crate::serial::write_str(" (RPL=");
    crate::serial::write_u64(iret_cs & 3);
    crate::serial::write_str(")");
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] RFLAGS slot   = ");
    crate::serial::write_hex(iret_rflags);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] RSP before iretq = ");
    crate::serial::write_hex(rsp + 15 * 8);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] Expected RSP after iretq = ");
    crate::serial::write_hex(rsp + 18 * 8);
    crate::serial::write_nl();

    crate::serial::write_str("[FRAME] Expected RSP & 0xFFF = ");
    crate::serial::write_hex((rsp + 18 * 8) & 0xFFF);
    crate::serial::write_nl();
}

/// The Rust-side schedule function called from the naked timer handler.
#[no_mangle]
pub unsafe extern "C" fn schedule(saved_rsp: u64) -> u64 {
    use super::scheduler::SCHEDULER;
    use crate::memory::vmm;

    SAVED_RSP = saved_rsp;

    crate::interrupts::pit::on_tick();

    let mut sched = SCHEDULER.lock();

    // ═══════════════════════════════════════════════════════════════════
    // FIRST DISPATCH: no current process yet.
    // ═══════════════════════════════════════════════════════════════════
    if sched.current_pid().is_none() {
        if let Some(first_pid) = sched.find_next_ready(0) {
            let sp = sched.dispatch_first(first_pid);

            crate::serial::write_str("[SCHED] FIRST DISPATCH: PID=");
            crate::serial::write_u64(first_pid);
            crate::serial::write_str(" sp=");
            crate::serial::write_hex(sp);
            crate::serial::write_str(" saved_rsp(old)=");
            crate::serial::write_hex(saved_rsp);
            crate::serial::write_nl();

            EXPECTED_RSP = sp + 18 * 8;

            return sp;
        }
        return saved_rsp;
    }

    // ═══════════════════════════════════════════════════════════════════
    // NORMAL PATH
    // ═══════════════════════════════════════════════════════════════════
    sched.save_current_sp(saved_rsp);

    let old_pid = sched.current_pid().unwrap_or(99);
    let new_sp = sched.on_tick();
    let new_pid = sched.current_pid().unwrap_or(99);

    crate::serial::write_str("[SCHED] current_pid=");
    crate::serial::write_u64(old_pid);
    crate::serial::write_str(" next_pid=");
    crate::serial::write_u64(new_pid);
    crate::serial::write_nl();

    if let Some(new_proc) = sched.current_process() {
        let new_pml4 = new_proc.pml4_phys;

        let (current_cr3, _) = x86_64::registers::control::Cr3::read();
        if current_cr3.start_address().as_u64() != new_pml4 {
            vmm::switch_page_table(crate::memory::PhysAddr::new(new_pml4));
        }

        let rsp0 = new_proc.kernel_stack_base + super::process::KERNEL_STACK_SIZE as u64;
        crate::gdt::set_tss_rsp0(rsp0);
        crate::syscall::set_kernel_rsp(rsp0);
    }

    let current_pid = sched.current_pid().unwrap_or(99);

    if new_sp == 0 {
        crate::serial::write_str("[SCHED] FATAL: new_sp=0 for PID=");
        crate::serial::write_u64(current_pid);
        crate::serial::write_nl();
        sched.dump_table();
        return 0;
    }

    new_sp
}
