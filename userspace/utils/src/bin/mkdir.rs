#![no_std]
#![no_main]

use indo_syscall as sys;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { sys::exit(1); }

fn get_arg(argc: u64, argv: u64, idx: usize) -> &'static str {
    if idx >= argc as usize { return ""; }
    let args = unsafe { core::slice::from_raw_parts(argv as *const u64, argc as usize) };
    let ptr = args[idx] as *const u8;
    let mut len = 0;
    while len < 4096 && unsafe { *ptr.add(len) } != 0 { len += 1; }
    unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr, len)) }
}

#[no_mangle]
pub extern "C" fn _start(argc: u64, argv: u64) -> ! {
    if argc < 2 { sys::write(2, b"mkdir: missing operand\n"); sys::exit(1); }
    let path = get_arg(argc, argv, 1);
    let ret = sys::mkdir(path);
    if sys::is_error(ret) { sys::write(2, b"mkdir: failed\n"); sys::exit(1); }
    sys::exit(0);
}
