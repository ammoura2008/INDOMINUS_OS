#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(1);
}

/// Regression test for KERNEL_GS_BASE corruption.
///
/// Spawns two processes, each doing a tight loop of getpid() syscalls.
/// Each syscall enters kernel mode via `swapgs`, accesses GS:0 (per-CPU
/// data), and returns. Timer interrupts fire during these loops, causing
/// context switches. If KERNEL_GS_BASE is corrupted (0 instead of the
/// per-CPU kernel address), the gs:0 access will fault at address 0.
///
/// Both processes must complete without crashing.
#[no_mangle]
pub extern "C" fn _start(_argc: u64, _argv: u64) -> ! {
    let iterations: u64 = 2000;
    let mut ok = true;

    for i in 0..iterations {
        let pid = sys::getpid();
        if pid == 0 {
            ok = false;
            break;
        }

        // Yield every 100 iterations to force context switches
        if i % 100 == 0 {
            sys::yield_now();
        }
    }

    if ok {
        sys::exit(0);
    } else {
        sys::exit(1);
    }
}
