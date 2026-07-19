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

/// The naked timer interrupt handler (vector 32).
///
/// Runs with interrupts DISABLED (interrupt gate).
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
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

        // ── Call schedule(saved_rsp: u64) → returns new SP in RAX ────────
        "mov rdi, rsp",
        "call {schedule}",
        // RAX = new process's saved RSP

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

        // ── Return from interrupt ────────────────────────────────────────
        // NO calls, pushes, or modifications between last pop and iretq.
        "iretq",

        schedule = sym crate::process::context_switch::schedule,
    );
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

    crate::serial::write_str("[FRAME] ── New task frame at ");
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

    let new_sp = sched.on_tick();

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
