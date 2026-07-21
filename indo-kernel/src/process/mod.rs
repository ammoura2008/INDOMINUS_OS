//! # Process Management Module
//!
//! Implements the process abstraction and round-robin scheduler.
//!
//! ## Context Switch Flow
//!
//! ```text
//! Timer fires (vector 32)
//!   → Naked handler saves registers on current stack
//!   → schedule() picks next process
//!   → Load new process's stack pointer
//!   → Restore registers from new stack
//!   → iretq resumes new process
//! ```

pub mod context_switch;
pub mod idle;
pub mod process;
pub mod scheduler;
pub mod tasks;

pub use process::{Process, ProcessState, Pid, MAX_PROCESSES, KERNEL_STACK_SIZE};
pub use scheduler::{Scheduler, SCHEDULER};

/// Initialize the process subsystem.
///
/// # Safety
/// Must be called with interrupts DISABLED. Returns with interrupts DISABLED.
/// Caller is responsible for enabling interrupts after all processes are spawned.
pub fn init() {
    crate::serial::write_str("[PROC] Initializing process subsystem...\n");

    unsafe { core::arch::asm!("cli", options(nostack, nomem)); }
    let mut sched = SCHEDULER.lock();
    sched.init();
    sched.spawn_idle(idle::idle_main as *const () as u64);
    drop(sched);
    // NOTE: interrupts remain DISABLED — caller must enable them later.

    crate::serial::write_str("[PROC] Process subsystem initialized\n");
}

/// Spawn a new kernel-mode process.
///
/// # Safety
/// Must be called with interrupts DISABLED.
pub fn spawn(entry_phys: u64) -> Option<Pid> {
    let result = SCHEDULER.lock().spawn(entry_phys);
    result
}

/// Spawn a new user-mode process from an ELF binary.
///
/// Creates a per-process PML4, loads ELF segments via the ELF loader,
/// maps a user stack page, and creates the process.
pub fn spawn_user(elf_data: &[u8], parent: Option<Pid>) -> Option<Pid> {
    use crate::memory::{self, vmm, PAGE_SIZE, USER_STACK_TOP};
    use x86_64::structures::paging::{FrameAllocator, PageTableFlags};
    use x86_64::VirtAddr;

    // 1. Get the current kernel PML4 (to copy kernel entries from)
    let kernel_pml4 = {
        let (frame, _) = x86_64::registers::control::Cr3::read();
        frame.start_address().as_u64()
    };

    // 2. Create a new PML4 with kernel entries shared
    let user_pml4 = vmm::create_user_pml4(memory::PhysAddr::new(kernel_pml4));

    // 3. Load ELF segments into the process's address space
    let elf_image = match crate::elf::load_elf(elf_data, user_pml4) {
        Ok(img) => img,
        Err(e) => {
            crate::serial::write_str("[PROC] ELF load failed: ");
            crate::serial::write_str(e.description());
            crate::serial::write_nl();
            return None;
        }
    };

    // 4. Map a user stack page below USER_STACK_TOP (4 KiB for now)
    let stack_frame = vmm::PmmFrameAllocator.allocate_frame()
        .expect("PMM: out of memory for user stack page");
    let stack_virt = VirtAddr::new(USER_STACK_TOP - PAGE_SIZE);
    let stack_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    vmm::map_page(user_pml4, stack_virt, memory::PhysAddr::new(stack_frame.start_address().as_u64()), stack_flags);

    // User RSP starts at the top of the stack region
    let user_rsp = USER_STACK_TOP;

    crate::serial::write_str("[PROC] ELF loaded: entry=");
    crate::serial::write_hex(elf_image.entry);
    crate::serial::write_str(", stack=");
    crate::serial::write_hex(stack_virt.as_u64());
    crate::serial::write_str(", pml4=");
    crate::serial::write_hex(user_pml4.as_u64());
    crate::serial::write_nl();

    // 5. Spawn via the scheduler
    let result = SCHEDULER.lock().spawn_user(elf_image.entry, user_rsp, user_pml4.as_u64(), parent);
    result
}

/// Start the scheduler. Never returns.
///
/// Enables interrupts and enters a HLT loop. The first timer interrupt
/// triggers `schedule()` which, seeing `current_pid == None`, finds the
/// first Ready task and returns its initial frame SP. The naked handler
/// then context-switches to that task via iretq — the same path used
/// for every subsequent context switch.
///
/// ## Stack transition
///
/// ```text
/// boot stack (kernel_main)  →  timer fires  →  schedule()
///   finds first Ready task  →  returns its initial frame SP
///   handler: mov rsp, r12   →  pop 15 GP  →  iretq
///   task runs on its OWN allocated kernel stack
///   boot stack is abandoned (never returned to)
/// ```
pub fn start_scheduler() -> ! {
    crate::serial::write_str("[PROC] Starting scheduler — first tick will dispatch\n");

    // Enable interrupts. The first timer IRQ will trigger the initial dispatch.
    // current_pid is None (set by init/sched.start not being called),
    // so schedule() will find the first Ready task and iretq to it.
    unsafe { core::arch::asm!("sti", options(nostack, nomem)); }

    // HLT loop — we never return to kernel_main.
    // The first timer interrupt context-switches to the first task.
    loop {
        unsafe { core::arch::asm!("hlt", options(nostack, nomem)); }
    }
}
