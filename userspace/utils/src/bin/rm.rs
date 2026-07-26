#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

fn get_arg_ptr(argc: u64, argv: u64, idx: usize) -> *const u8 {
    if idx >= argc as usize { return core::ptr::null(); }
    let args = unsafe { core::slice::from_raw_parts(argv as *const u64, argc as usize) };
    args[idx] as *const u8
}

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    if argc < 2 { sys::write(2, b"rm: missing operand\n"); sys::exit(1); }
    let ptr = get_arg_ptr(argc, argv, 1);
    let ret = sys::unlink(ptr as u64);
    if sys::is_error(ret) { sys::write(2, b"rm: failed\n"); sys::exit(1); }
    sys::exit(0);
}
