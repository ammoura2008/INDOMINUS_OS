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
#[cfg(DEBUG_KERNEL)]
pub static mut SAVED_RSP: u64 = 0;

/// RSP immediately after `mov rsp, r12` (the new task's frame base).
#[cfg(DEBUG_KERNEL)]
pub static mut RSP_AFTER_LOAD: u64 = 0;

/// RSP after 15 pops, before iretq (= RSP_AFTER_LOAD + 15*8).
/// This is the address the CPU reads the IRET frame from.
#[cfg(DEBUG_KERNEL)]
pub static mut RSP_BEFORE_IRETQ: u64 = 0;

/// Expected RSP after iretq (= RSP_AFTER_LOAD + 18*8).
#[cfg(DEBUG_KERNEL)]
pub static mut EXPECTED_RSP: u64 = 0;

/// CS slot value from the IRET frame (read before pops).
#[cfg(DEBUG_KERNEL)]
pub static mut CS_IN_FRAME: u64 = 0;

/// RIP slot value from the IRET frame (read before pops).
#[cfg(DEBUG_KERNEL)]
pub static mut RIP_IN_FRAME: u64 = 0;

/// IRET frame values captured by diagnostics (for DF handler).
#[cfg(DEBUG_KERNEL)]
pub static mut IRET_RIP: u64 = 0;
#[cfg(DEBUG_KERNEL)]
pub static mut IRET_CS: u64 = 0;
#[cfg(DEBUG_KERNEL)]
pub static mut IRET_RFLAGS: u64 = 0;
#[cfg(DEBUG_KERNEL)]
pub static mut IRET_RSP_VAL: u64 = 0;
#[cfg(DEBUG_KERNEL)]
pub static mut IRET_SS: u64 = 0;



/// Deferred CR3 value for the first dispatch.
///
/// When the first dispatch happens, the timer handler is still running on the
/// UEFI boot stack (lower-half physical address). Switching CR3 here would
/// unmap the boot stack (user PML4 lacks the identity map) → page fault → DF.
///
/// Instead, schedule() stores the target PML4 here and returns. The naked
/// handler reads this AFTER `mov rsp, r12` (when RSP is on the new process's
/// upper-half kernel stack), then switches CR3 safely.
///
/// Zero means "no deferred switch pending".
#[no_mangle]
pub static DEFERRED_CR3: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Pointer to the current process's FPU/SSE state buffer (512 bytes, 16-byte aligned).
/// Updated by schedule() on every context switch.
/// The naked handler uses this for FXSAVE (outgoing) and FXRSTOR (incoming).
#[no_mangle]
pub static CURRENT_FPU_STATE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Deferred process cleanup slots.
///
/// `schedule_force` runs on the dying process's kernel stack. Calling
/// `free_kernel_stack` on that stack corrupts the locals and return address.
/// Instead, we stash dead resources here and free them at the next timer tick
/// when we are safely on a different process's stack.
const MAX_DEFERRED: usize = 8;
use core::sync::atomic::{AtomicUsize, Ordering};
static DEFERRED_COUNT: AtomicUsize = AtomicUsize::new(0);
static DEFERRED_SLOTS: spin::Mutex<[(u64, u64, bool); MAX_DEFERRED]> =
    spin::Mutex::new([(0, 0, false); MAX_DEFERRED]);

/// Enqueue dead-process resources for deferred freeing.
///
/// # Safety
/// Must only be called from `schedule_force` with interrupts disabled.
#[allow(dead_code)]
pub unsafe fn defer_free(kstack: u64, pml4: u64, is_user: bool) {
    let count = DEFERRED_COUNT.load(Ordering::Relaxed);
    if count < MAX_DEFERRED {
        let mut slots = DEFERRED_SLOTS.lock();
        slots[count] = (kstack, pml4, is_user);
        DEFERRED_COUNT.store(count + 1, Ordering::Relaxed);
    } else {
        // Overflow: all deferred slots full.  We CANNOT free synchronously
        // because we're still on the dying process's kernel stack — freeing
        // it would corrupt locals and the return address → triple fault.
        // Instead, panic to halt the system. This should never happen in
        // normal operation (MAX_DEFERRED=4 is enough for all exit paths).
        crate::serial::write_str("[SCHED] PANIC: deferred slots full, cannot free resources safely\n");
        crate::serial::write_str("  kstack=0x");
        crate::serial::write_hex(kstack);
        crate::serial::write_str(" pml4=0x");
        crate::serial::write_hex(pml4);
        crate::serial::write_nl();
        loop { unsafe { core::arch::asm!("hlt"); } }
    }
}

/// Drain all deferred-freelist slots.  Must be called from a safe context
/// (e.g. the timer handler after context-switching to a different process).
pub fn drain_deferred_free() {
    use crate::memory::vmm;
    let count = DEFERRED_COUNT.swap(0, Ordering::Relaxed);
    if count == 0 {
        return;
    }
    let slots = {
        let mut s = DEFERRED_SLOTS.lock();
        let taken = *s;
        for slot in s.iter_mut() {
            *slot = (0, 0, false);
        }
        taken
    };
    for i in 0..count {
        let (kstack, pml4, is_user) = slots[i];

        // Switch to kernel PML4 for alloc/dealloc (PIC pointers need identity map).
        let kernel_pml4 = crate::memory::kernel_pml4_phys();
        let (saved_cr3, _) = x86_64::registers::control::Cr3::read();
        if saved_cr3.start_address().as_u64() != kernel_pml4 {
            unsafe { vmm::switch_page_table(crate::memory::PhysAddr::new(kernel_pml4)); }
        }

        // Free user address space if this was a user process.
        // safe: we're on the kernel PML4 with identity map active.
        if is_user && pml4 != 0 {
            unsafe { vmm::free_user_address_space(crate::memory::PhysAddr::new(pml4)); }
        }

        if kstack != 0 {
            super::process::free_kernel_stack(kstack);
        }

        // Restore original CR3.
        if saved_cr3.start_address().as_u64() != kernel_pml4 {
            unsafe { vmm::switch_page_table(crate::memory::PhysAddr::new(
                saved_cr3.start_address().as_u64(),
            )); }
        }
    }
}

/// The naked timer interrupt handler (vector 32).
///
/// Runs with interrupts DISABLED (interrupt gate).
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn timer_interrupt_handler() {
    core::arch::naked_asm!(
        // ── Save FPU/SSE state of outgoing process ────────────────────────
        // FXSAVE saves 512 bytes to the buffer pointed by CURRENT_FPU_STATE.
        // Must happen before GP register pushes (FXSAVE doesn't touch GP regs).
        // Skipped if CURRENT_FPU_STATE is 0 (first dispatch, no outgoing process).
        "lea rax, [rip + {current_fpu_state}]",
        "mov rax, [rax]",
        "test rax, rax",
        "jz .Lno_fpu_save",
        "fxsave [rax]",
        ".Lno_fpu_save:",

        // ── Save current process's registers ─────────────────────────────
        // Canonical frame: push R15 first (highest addr) → RAX last (lowest = RSP)
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

        // ── Call schedule(saved_rsp: u64) → returns new SP in RAX ────────
        "mov rdi, rsp",
        "call {schedule}",
        // RAX = new process's saved RSP

        // ── Save new SP into r12 BEFORE EOI ──────────────────────────────
        "mov r12, rax",

        // ── Send EOI to LAPIC ────────────────────────────────────────────
        "mov rax, 0xFFFFFFFFFEE000B0",
        "mov dword ptr [rax], 0",

        // ── Switch to new process's stack ────────────────────────────────
        "mov rsp, r12",

        // ── Deferred CR3 switch (first dispatch only) ─────────────────
        "lea rbx, [rip + {deferred_cr3}]",
        "mov rax, [rbx]",
        "test rax, rax",
        "jz .Lno_deferred_cr3",
        "mov cr3, rax",
        "mov qword ptr [rbx], 0",
        ".Lno_deferred_cr3:",

        // ── Restore new process's registers ──────────────────────────────
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

        // ── Restore FPU/SSE state of incoming process ────────────────────
        // Push/pop RAX around FXRSTOR because loading the FPU state pointer
        // clobbers RAX, which holds the user's restored register value.
        "push rax",
        "lea rax, [rip + {current_fpu_state}]",
        "mov rax, [rax]",
        "test rax, rax",
        "jz .Lno_fpu_restore",
        "fxrstor [rax]",
        ".Lno_fpu_restore:",
        "pop rax",

        // ── Return from interrupt ────────────────────────────────────────
        "iretq",

        schedule = sym crate::process::context_switch::schedule,
        deferred_cr3 = sym DEFERRED_CR3,
        current_fpu_state = sym CURRENT_FPU_STATE,
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
#[cfg(DEBUG_KERNEL)]
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

#[cfg(not(DEBUG_KERNEL))]
#[no_mangle]
pub unsafe extern "C" fn print_iret_cpl_diagnostics(_iret_frame: u64, _gdt_base: u64) {}

/// Print one GDT entry: selector, raw lo/hi, DPL, type, present.
#[cfg(DEBUG_KERNEL)]
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
#[cfg(DEBUG_KERNEL)]
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
#[cfg(DEBUG_KERNEL)]
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

#[cfg(not(DEBUG_KERNEL))]
#[no_mangle]
pub unsafe extern "C" fn dump_frame_before_pop(_rsp: u64) {}

/// Dump the 5-qword IRET frame right before iretq.
///
/// Called from the naked timer handler with:
///   rdi = RSP pointing at the IRET frame [RIP, CS, RFLAGS, RSP, SS]
///
/// Must use write_str/write_hex (NOT kprintln!) — we may be running with
/// a user PML4 (no identity map), so format_args! function pointers
/// (physical addresses) would page-fault.
#[cfg(DEBUG_KERNEL)]
#[no_mangle]
pub unsafe extern "C" fn dump_iret_before_iretq(rsp: u64) {
    use crate::serial::{write_str, write_hex, write_nl, write_u64};
    let iret_rip    = core::ptr::read_volatile(rsp as *const u64);
    let iret_cs     = core::ptr::read_volatile((rsp + 8) as *const u64);
    let iret_rflags = core::ptr::read_volatile((rsp + 16) as *const u64);
    let iret_rsp    = core::ptr::read_volatile((rsp + 24) as *const u64);
    let iret_ss     = core::ptr::read_volatile((rsp + 32) as *const u64);
    write_str("[IRET-FRAME] RIP=0x"); write_hex(iret_rip);
    write_str(" CS=0x"); write_hex(iret_cs);
    write_str("(RPL="); write_u64(iret_cs & 3); write_str(")");
    write_str(" RFLAGS=0x"); write_hex(iret_rflags);
    write_str(" RSP=0x"); write_hex(iret_rsp);
    write_str(" SS=0x"); write_hex(iret_ss);
    write_str("(RPL="); write_u64(iret_ss & 3); write_str(")");
    write_nl();
}

#[cfg(not(DEBUG_KERNEL))]
#[no_mangle]
pub unsafe extern "C" fn dump_iret_before_iretq(_rsp: u64) {}

/// The Rust-side schedule function called from the naked timer handler.
#[no_mangle]
pub unsafe extern "C" fn schedule(saved_rsp: u64) -> u64 {
    use super::scheduler::SCHEDULER;
    use crate::memory::vmm;

    // ── Drain deferred frees from the previous schedule_force() ─────────
    // schedule_force() stashes dead process resources here instead of
    // freeing them inline (freeing the stack we're executing on corrupts
    // locals and the return address).  Now we're safely on the timer
    // handler's kernel stack, so it's safe to free.
    crate::process::context_switch::drain_deferred_free();

    // Check if any interrupt handler (serial RX, keyboard IRQ) flagged a
    // pending wake.  If so, call keyboard_wake() now — we're in the timer
    // handler which is a safe context for acquiring the SCHEDULER lock.
    if crate::process::KEYBOARD_WAKE_PENDING.swap(false, core::sync::atomic::Ordering::Acquire) {
        crate::process::keyboard_wake();
    }

    #[cfg(DEBUG_KERNEL)]
    { SAVED_RSP = saved_rsp; }

    crate::interrupts::pit::on_tick();

    let mut sched = SCHEDULER.lock();

    // ═══════════════════════════════════════════════════════════════════
    // FIRST DISPATCH: no current process yet.
    // ═══════════════════════════════════════════════════════════════════
    if sched.current_pid().is_none() {
        if let Some(first_pid) = sched.find_next_ready(0) {
            let sp = sched.dispatch_first(first_pid);

            // FIX: First dispatch must NOT switch CR3 here. The timer handler
            // is running on the UEFI boot stack (lower-half physical address).
            // User PML4s lack the identity map, so switching CR3 here would
            // unmap the boot stack → page fault → double fault.
            //
            // Instead, store the target PML4 in DEFERRED_CR3. The naked handler
            // reads this AFTER `mov rsp, r12` (when RSP is on the new process's
            // upper-half kernel stack) and switches CR3 there safely.
            if let Some(new_proc) = sched.current_process() {
                let new_pml4 = new_proc.pml4_phys;
                let (current_cr3, _) = x86_64::registers::control::Cr3::read();
                if current_cr3.start_address().as_u64() != new_pml4 {
                    DEFERRED_CR3.store(new_pml4, core::sync::atomic::Ordering::Relaxed);
                }
        let rsp0 = new_proc.kernel_stack_base + super::process::KERNEL_STACK_SIZE as u64;
        crate::gdt::set_tss_rsp0(rsp0);
        crate::syscall::set_kernel_rsp(rsp0);

        // Update FPU state pointer for the incoming process.
        // The naked handler reads this for FXRSTOR after GP register restore.
        CURRENT_FPU_STATE.store(&new_proc.fpu_state as *const _ as u64, core::sync::atomic::Ordering::Relaxed);
            }

            #[cfg(DEBUG_KERNEL)]
            {
                crate::serial::write_str("[SCHED] FIRST DISPATCH: PID=");
                crate::serial::write_u64(first_pid);
                crate::serial::write_str(" sp=");
                crate::serial::write_hex(sp);
                crate::serial::write_str(" saved_rsp(old)=");
                crate::serial::write_hex(saved_rsp);
                crate::serial::write_nl();
            }

            #[cfg(DEBUG_KERNEL)]
            { EXPECTED_RSP = sp + 18 * 8; }

            return sp;
        }
        return saved_rsp;
    }

    // ═══════════════════════════════════════════════════════════════════
    // NORMAL PATH
    // ═══════════════════════════════════════════════════════════════════
    let _old_pid_for_diag = sched.current_pid();
    let _old_sp_for_diag = saved_rsp;

    // ── DIAGNOSTIC: dump IRET frame of process BEING SAVED (preempted) ──
    // The IRET frame is at saved_rsp + 15*8 (after 15 GP register pushes)
    #[cfg(DEBUG_KERNEL)]
    {
        let iret_addr = saved_rsp + 15 * 8;
        let iret_rip   = core::ptr::read_volatile(iret_addr as *const u64);
        let iret_cs    = core::ptr::read_volatile((iret_addr + 8) as *const u64);
        let iret_rflags = core::ptr::read_volatile((iret_addr + 16) as *const u64);
        let iret_rsp   = core::ptr::read_volatile((iret_addr + 24) as *const u64);
        let iret_ss    = core::ptr::read_volatile((iret_addr + 32) as *const u64);
        crate::serial::write_str("[SAVE] pid=");
        crate::serial::write_u64(old_pid_for_diag.unwrap_or(99));
        crate::serial::write_str(" sp=");
        crate::serial::write_hex(saved_rsp);
        crate::serial::write_str(" IRET: RIP="); crate::serial::write_hex(iret_rip);
        crate::serial::write_str(" CS="); crate::serial::write_hex(iret_cs);
        crate::serial::write_str(" RFLAGS="); crate::serial::write_hex(iret_rflags);
        crate::serial::write_str(" RSP="); crate::serial::write_hex(iret_rsp);
        crate::serial::write_str(" SS="); crate::serial::write_hex(iret_ss);
        crate::serial::write_nl();
    }

    sched.save_current_sp(saved_rsp);

    let _old_pid = sched.current_pid().unwrap_or(99);
    let new_sp = sched.on_tick();
    let _new_pid = sched.current_pid().unwrap_or(99);

    #[cfg(DEBUG_KERNEL)]
    {
        crate::serial::write_str("[SCHED] current_pid=");
        crate::serial::write_u64(old_pid);
        crate::serial::write_str(" next_pid=");
        crate::serial::write_u64(new_pid);
        crate::serial::write_nl();
    }

    if let Some(new_proc) = sched.current_process() {
        // Use saved_cr3 if available (the CR3 that was active when the process
        // was last preempted).  Falls back to pml4_phys for first dispatch.
        let target_pml4 = if new_proc.saved_cr3 != 0 {
            new_proc.saved_cr3
        } else {
            new_proc.pml4_phys
        };

        let (current_cr3, _) = x86_64::registers::control::Cr3::read();
        if current_cr3.start_address().as_u64() != target_pml4 {
            vmm::switch_page_table(crate::memory::PhysAddr::new(target_pml4));
        }

        let rsp0 = new_proc.kernel_stack_base + super::process::KERNEL_STACK_SIZE as u64;
        crate::gdt::set_tss_rsp0(rsp0);
        crate::syscall::set_kernel_rsp(rsp0);

        // Update FPU state pointer for the incoming process.
        // The naked handler uses this for FXRSTOR after schedule() returns.
        CURRENT_FPU_STATE.store(&new_proc.fpu_state as *const _ as u64, core::sync::atomic::Ordering::Relaxed);
    }

    // ── DIAGNOSTIC: dump IRET frame of process BEING RESTORED (resumed) ──
    // The IRET frame is at new_sp + 15*8 (after 15 GP register pushes)
    #[cfg(DEBUG_KERNEL)]
    {
        let iret_addr = new_sp + 15 * 8;
        let iret_rip   = core::ptr::read_volatile(iret_addr as *const u64);
        let iret_cs    = core::ptr::read_volatile((iret_addr + 8) as *const u64);
        let iret_rflags = core::ptr::read_volatile((iret_addr + 16) as *const u64);
        let iret_rsp   = core::ptr::read_volatile((iret_addr + 24) as *const u64);
        let iret_ss    = core::ptr::read_volatile((iret_addr + 32) as *const u64);
        crate::serial::write_str("[RESTORE] pid=");
        crate::serial::write_u64(new_pid);
        crate::serial::write_str(" sp=");
        crate::serial::write_hex(new_sp);
        crate::serial::write_str(" IRET: RIP="); crate::serial::write_hex(iret_rip);
        crate::serial::write_str(" CS="); crate::serial::write_hex(iret_cs);
        crate::serial::write_str(" RFLAGS="); crate::serial::write_hex(iret_rflags);
        crate::serial::write_str(" RSP="); crate::serial::write_hex(iret_rsp);
        crate::serial::write_str(" SS="); crate::serial::write_hex(iret_ss);
        crate::serial::write_nl();
    }

    let current_pid = sched.current_pid().unwrap_or(99);

    if new_sp == 0 {
        crate::serial::write_str("[SCHED] FATAL: new_sp=0 in schedule() for PID=");
        crate::serial::write_u64(current_pid);
        crate::serial::write_nl();
        sched.dump_table();
        // Fallback: return idle process SP to prevent triple fault
        let idle_sp = sched.processes()[sched.idle_pid() as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0);
        if idle_sp != 0 {
            return idle_sp;
        }
        // Last resort: halt
        crate::serial::write_str("[SCHED] HALT: no idle SP available\n");
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    new_sp
}

/// Force-switch scheduler — called from syscall_entry force_switch path.
///
/// Unlike schedule() which goes through on_tick() (quantum-gated), this
/// function ALWAYS performs a context switch. Used by sys_exit and sys_yield
/// where the current process must yield immediately regardless of quantum.
#[no_mangle]
pub unsafe extern "C" fn schedule_force(saved_rsp: u64) -> u64 {
    use super::scheduler::SCHEDULER;
    use crate::memory::vmm;

    let mut sched = SCHEDULER.lock();

    // ── DIAGNOSTIC: dump IRET frame of process BEING SAVED (force-switch) ──
    #[cfg(DEBUG_KERNEL)]
    {
        let iret_addr = saved_rsp + 15 * 8;
        let iret_rip   = core::ptr::read_volatile(iret_addr as *const u64);
        let iret_cs    = core::ptr::read_volatile((iret_addr + 8) as *const u64);
        let iret_rflags = core::ptr::read_volatile((iret_addr + 16) as *const u64);
        let iret_rsp   = core::ptr::read_volatile((iret_addr + 24) as *const u64);
        let iret_ss    = core::ptr::read_volatile((iret_addr + 32) as *const u64);
        crate::serial::write_str("[FORCE-SAVE] pid=");
        crate::serial::write_u64(sched.current_pid().unwrap_or(99));
        crate::serial::write_str(" sp=");
        crate::serial::write_hex(saved_rsp);
        crate::serial::write_str(" IRET: RIP="); crate::serial::write_hex(iret_rip);
        crate::serial::write_str(" CS="); crate::serial::write_hex(iret_cs);
        crate::serial::write_str(" RFLAGS="); crate::serial::write_hex(iret_rflags);
        crate::serial::write_str(" RSP="); crate::serial::write_hex(iret_rsp);
        crate::serial::write_str(" SS="); crate::serial::write_hex(iret_ss);
        crate::serial::write_nl();
    }

    // Save current process's SP (for sys_yield; ignored by sys_exit).
    sched.save_current_sp(saved_rsp);

    // If the current process is a Zombie (from sys_exit), save its resources
    // so we can free them AFTER switching to the new process's stack.
    // The stack and page tables must not be freed while we're still executing on them.
    let (dead_kstack, dead_pml4, dead_is_user) = if let Some(old_proc) = sched.current_process() {
        if old_proc.state == super::process::ProcessState::Zombie {
            (old_proc.kernel_stack_base, old_proc.pml4_phys, old_proc.is_user)
        } else {
            (0, 0, false)
        }
    } else {
        (0, 0, false)
    };

    let old_pid = sched.current_pid().unwrap_or(99);

    // ── Checkpoint F: before switch_next_force ────────────────────────
    crate::serial::ddbg(b'F');

    // Force switch: always call switch_next() regardless of quantum.
    let new_sp = sched.switch_next_force();
    let _new_pid = sched.current_pid().unwrap_or(99);

    // ── Checkpoint S: after switch_next_force ─────────────────────────
    crate::serial::ddbg(b'S');

    #[cfg(DEBUG_KERNEL)]
    {
        crate::serial::write_str("[FORCE] old=");
        crate::serial::write_u64(old_pid);
        crate::serial::write_str(" new=");
        crate::serial::write_u64(new_pid);
        crate::serial::write_str(" sp=");
        crate::serial::write_hex(new_sp);
        crate::serial::write_nl();

        // Dump the new task's IRET frame (at new_sp + 15*8)
        if new_sp != 0 {
            let iret_ptr = (new_sp + 15 * 8) as *const u64;
            let iret_rip   = core::ptr::read_volatile(iret_ptr);
            let iret_cs    = core::ptr::read_volatile(iret_ptr.add(1));
            let iret_rsp   = core::ptr::read_volatile(iret_ptr.add(3));
            let iret_ss    = core::ptr::read_volatile(iret_ptr.add(4));
            crate::serial::write_str("[FORCE] IRET RIP="); crate::serial::write_hex(iret_rip);
            crate::serial::write_str(" CS="); crate::serial::write_hex(iret_cs);
            crate::serial::write_str(" RSP="); crate::serial::write_hex(iret_rsp);
            crate::serial::write_str(" SS="); crate::serial::write_hex(iret_ss);
            crate::serial::write_nl();
        }
    }

    // ── Checkpoint D: IRET frame dumped ──────────────────────────────
    crate::serial::ddbg(b'D');

    if let Some(new_proc) = sched.current_process() {
        let target_pml4 = if new_proc.saved_cr3 != 0 {
            new_proc.saved_cr3
        } else {
            new_proc.pml4_phys
        };

        let (current_cr3, _) = x86_64::registers::control::Cr3::read();
        if current_cr3.start_address().as_u64() != target_pml4 {
            vmm::switch_page_table(crate::memory::PhysAddr::new(target_pml4));
        }

        // ── Checkpoint C: CR3 switched ───────────────────────────────
        crate::serial::ddbg(b'C');

        let rsp0 = new_proc.kernel_stack_base + super::process::KERNEL_STACK_SIZE as u64;
        crate::gdt::set_tss_rsp0(rsp0);
        crate::syscall::set_kernel_rsp(rsp0);

        // Update FPU state pointer for the incoming process.
        CURRENT_FPU_STATE.store(&new_proc.fpu_state as *const _ as u64, core::sync::atomic::Ordering::Relaxed);

        // ── Checkpoint T: TSS/RSP0 set ───────────────────────────────
        crate::serial::ddbg(b'T');
    }

    // ── Checkpoint R: about to return ────────────────────────────────
    crate::serial::ddbg(b'R');

    // ── Deferred cleanup ──────────────────────────────────────────────────
    // We are STILL executing on the dead process's kernel stack.  Calling
    // free_kernel_stack here would deallocate the very memory we're using,
    // corrupting local variables (`new_sp`, `saved_cr3`) and the function's
    // return address → garbage SP → page fault on iretq.
    //
    // Instead, stash the dead resources in a static deferred-free list.
    // drain_deferred_free() runs at the start of the next timer tick
    // (schedule()), when we are safely on a different process's kernel stack.
    if dead_kstack != 0 || dead_pml4 != 0 {
        unsafe { defer_free(dead_kstack, dead_pml4, dead_is_user); }
        if let Some(ref mut old_proc) = sched.processes_mut()[old_pid as usize] {
            old_proc.kernel_stack_base = 0;
            old_proc.pml4_phys = 0;
        }
    }

    // No CR3 switch needed here — we stay on the new process's PML4
    // (already switched above). drain_deferred_free() handles its own
    // kernel-PML4 switch when it runs.

    if new_sp == 0 {
        crate::serial::write_str("[SCHED] FATAL: new_sp=0 in force_switch\n");
        sched.dump_table();
        // Fallback: return idle process SP to prevent triple fault
        let idle_sp = sched.processes()[sched.idle_pid() as usize]
            .as_ref()
            .map(|p| p.stack_pointer)
            .unwrap_or(0);
        if idle_sp != 0 {
            return idle_sp;
        }
        crate::serial::write_str("[SCHED] HALT: no idle SP available in force_switch\n");
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    new_sp
}

/// Kill the current process and return the next process's stack pointer.
///
/// Called from the page fault handler when a user process faults.
/// Marks the current process as Zombie and performs CR3 switch + TSS update
/// for the next process.
#[no_mangle]
pub unsafe extern "C" fn kill_process() -> u64 {
    use super::scheduler::SCHEDULER;
    use crate::memory::vmm;

    let mut sched = SCHEDULER.lock();

    // Save the dying process's resources BEFORE switching away.
    let (dead_kstack, dead_pml4, dead_is_user, dead_pid) = {
        let proc = sched.current_process();
        (
            proc.map(|p| p.kernel_stack_base).unwrap_or(0),
            proc.map(|p| p.pml4_phys).unwrap_or(0),
            proc.map(|p| p.is_user).unwrap_or(false),
            sched.current_pid().unwrap_or(0),
        )
    };

    let new_sp = sched.kill_process();

    if let Some(new_proc) = sched.current_process() {
        let target_pml4 = if new_proc.saved_cr3 != 0 {
            new_proc.saved_cr3
        } else {
            new_proc.pml4_phys
        };

        let (current_cr3, _) = x86_64::registers::control::Cr3::read();
        if current_cr3.start_address().as_u64() != target_pml4 {
            vmm::switch_page_table(crate::memory::PhysAddr::new(target_pml4));
        }

        let rsp0 = new_proc.kernel_stack_base + super::process::KERNEL_STACK_SIZE as u64;
        crate::gdt::set_tss_rsp0(rsp0);
        crate::syscall::set_kernel_rsp(rsp0);

        // Update FPU state pointer for the incoming process.
        CURRENT_FPU_STATE.store(&new_proc.fpu_state as *const _ as u64, core::sync::atomic::Ordering::Relaxed);
    }

    // Now safe to free the dead process's resources — we've switched CR3.
    //
    // CRITICAL: free_kernel_stack calls dealloc() which uses PIC function
    // pointers (linked_list_allocator internals). With PIC, those pointers
    // contain physical addresses. Indirect calls through them require the
    // identity map (kernel PML4). A user PML4 lacks the identity map.
    //
    // We are STILL on the dead process's kernel stack here (page_fault_return_to_user
    // will switch stacks via `mov rsp, rdi`).  Freeing the stack now would corrupt
    // locals and the return path.  Defer to the next timer tick.
    if dead_kstack != 0 || dead_pml4 != 0 {
        unsafe { defer_free(dead_kstack, dead_pml4, dead_is_user); }
        if let Some(ref mut old_proc) = sched.processes_mut()[dead_pid as usize] {
            old_proc.kernel_stack_base = 0;
            old_proc.pml4_phys = 0;
        }
    }

    new_sp
}

/// Naked page fault return — called from the page fault handler after kill_process().
///
/// Handles the stack switch, EOI, and iretq back to the next process.
///
/// # Stack layout of the new process (built by setup_initial_stack_frame_user):
/// ```text
/// [RSP+0]  = GP: rax (15 qwords total)
/// [RSP+8]  = GP: rbx
/// ...      (15 GP registers)
/// [RSP+120] = IRET RIP            ← iretq reads from here
/// [RSP+128] = IRET CS
/// [RSP+136] = IRET RFLAGS
/// [RSP+144] = IRET RSP            (user RSP)
/// [RSP+152] = IRET SS             (user SS)
/// ```
///
/// NOTE: The error code from the page fault is on the OLD (faulting) process's
/// stack, not the new process's stack. After `mov rsp, r12` to the new stack,
/// there is no error code to skip — RSP already points at the 15 GP registers.
#[unsafe(naked)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn page_fault_return_to_user() {
    core::arch::naked_asm!(
        // RDI = new_sp (from kill_process)
        "mov r12, rdi",

        // Safety: if r12 == 0, halt to prevent triple fault
        "test r12, r12",
        "jnz 1f",
        "cli",
        ".loop:",
        "hlt",
        "jmp .loop",
        "1:",

        // Send EOI to LAPIC (upper-half virtual address)
        "mov rax, 0xFFFFFFFFFEE000B0",
        "mov dword ptr [rax], 0",

        // Switch to new process's stack
        "mov rsp, r12",

        // Restore GP registers from new process's frame
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

        // Restore FPU/SSE state of incoming process.
        // Push/pop RAX around FXRSTOR because loading the FPU state pointer
        // clobbers RAX, which holds the user's restored register value.
        "push rax",
        "lea rax, [rip + {current_fpu_state}]",
        "mov rax, [rax]",
        "test rax, rax",
        "jz .Lpf_no_fpu_restore",
        "fxrstor [rax]",
        ".Lpf_no_fpu_restore:",
        "pop rax",

        // RSP now points to IRET frame: RIP, CS, RFLAGS, RSP, SS
        // Do NOT pop these — iretq reads from [RSP] directly.
        "iretq",

        current_fpu_state = sym CURRENT_FPU_STATE,
    );
}
