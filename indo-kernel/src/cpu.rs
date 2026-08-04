//! CPU feature detection via CPUID.
//!
//! Provides runtime detection of x86-64 features needed by the kernel:
//! - NX (No Execute) — EFER.NXE bit, enables page-level execute protection
//! - SMEP (Supervisor Mode Execution Prevention) — CR4 bit 20
//! - SMAP (Supervisor Mode Access Prevention) — CR4 bit 21
//! - Other features logged for diagnostics

use crate::sync_cell::SyncUnsafeCell;

/// Cached CPU feature flags, detected once at boot.
pub struct CpuFeatures {
    pub nx: bool,
    pub smep: bool,
    pub smap: bool,
    pub pat: bool,
    pub apic: bool,
    pub sep: bool,
    pub syscall: bool,
    pub mmx: bool,
    pub sse: bool,
    pub sse2: bool,
    pub sse3: bool,
    pub ssse3: bool,
    pub sse4_1: bool,
    pub sse4_2: bool,
    pub avx: bool,
    pub avx2: bool,
    pub fsgsbase: bool,
    pub tsc_deadline: bool,
    pub rdtscp: bool,
    pub invariant_tsc: bool,
    pub maxphyaddr: u8,
}

/// Global cached features. Detected once, read many times.
/// Wrapped in SyncUnsafeCell to avoid undefined behavior from shared references
/// to mutable statics (Rust 2024 edition).
static CPU_FEATURES: SyncUnsafeCell<CpuFeatures> = SyncUnsafeCell::new(CpuFeatures {
    nx: false, smep: false, smap: false, pat: false, apic: false,
    sep: false, syscall: false, mmx: false, sse: false, sse2: false,
    sse3: false, ssse3: false, sse4_1: false, sse4_2: false,
    avx: false, avx2: false, fsgsbase: false, tsc_deadline: false,
    rdtscp: false, invariant_tsc: false, maxphyaddr: 36,
});

/// Run CPUID with the given leaf and sub-leaf, return (EAX, EBX, ECX, EDX).
#[inline]
fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let mut eax = leaf;
    let mut ecx = subleaf;
    let mut result = [0u32; 4];
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov [{ptr}], eax",
            "mov [{ptr} + 4], ebx",
            "mov [{ptr} + 8], ecx",
            "mov [{ptr} + 12], edx",
            "pop rbx",
            inout("eax") eax,
            inout("ecx") ecx,
            ptr = in(reg) result.as_mut_ptr(),
            out("edx") _,
        );
    }
    (result[0], result[1], result[2], result[3])
}

/// Detect and cache all CPU features. Call once during boot.
pub fn detect() {
    let mut f = CpuFeatures {
        nx: false, smep: false, smap: false, pat: false, apic: false,
        sep: false, syscall: false, mmx: false, sse: false, sse2: false,
        sse3: false, ssse3: false, sse4_1: false, sse4_2: false,
        avx: false, avx2: false, fsgsbase: false, tsc_deadline: false,
        rdtscp: false, invariant_tsc: false, maxphyaddr: 36,
    };

    // ── CPUID leaf 0x01: Basic feature flags ───────────────────────────
    let (_eax, ebx, ecx, edx) = cpuid(0x01, 0);

    // Max CPUID leaf
    let (max_leaf, _, _, _) = cpuid(0x00, 0);

    // EDX flags
    f.mmx   = (edx >> 23) & 1 == 1;
    f.sse   = (edx >> 25) & 1 == 1;
    f.sse2  = (edx >> 26) & 1 == 1;
    f.apic  = (edx >> 9) & 1 == 1;
    f.sep   = (edx >> 11) & 1 == 1;
    f.pat   = (edx >> 16) & 1 == 1;
    f.syscall = f.sep;  // AMD64: SEP = syscall/sysret support

    // ECX flags
    f.sse3  = (ecx >> 0) & 1 == 1;
    f.ssse3 = (ecx >> 9) & 1 == 1;
    f.sse4_1 = (ecx >> 19) & 1 == 1;
    f.sse4_2 = (ecx >> 20) & 1 == 1;
    f.avx   = (ecx >> 28) & 1 == 1;
    f.tsc_deadline = (ecx >> 24) & 1 == 1;

    // EBX flags (leaf 0x01)
    f.fsgsbase = (ebx >> 0) & 1 == 1;

    // Max physical address bits — from CPUID.80000008H:EAX[7:0]
    let (eax_8, _, _, _) = cpuid(0x80000008, 0);
    f.maxphyaddr = (eax_8 & 0xFF) as u8;

    // ── CPUID leaf 0x80000001: Extended feature flags ──────────────────
    let (_, _, _, edx_ext) = cpuid(0x80000001, 0);

    f.nx     = (edx_ext >> 20) & 1 == 1;  // No Execute bit
    f.rdtscp = (edx_ext >> 27) & 1 == 1;

    // ── CPUID leaf 0x80000007: Advanced Power Management ───────────────
    let (_, _, _, edx_pow) = cpuid(0x80000007, 0);
    f.invariant_tsc = (edx_pow >> 8) & 1 == 1;

    // ── CPUID leaf 0x07, sub-leaf 0: Structured extended features ──────
    if max_leaf >= 0x07 {
        let (_, ebx_07, _, _) = cpuid(0x07, 0);
        f.smep = (ebx_07 >> 7) & 1 == 1;    // EBX bit 7
        f.smap = (ebx_07 >> 20) & 1 == 1;   // EBX bit 20
        f.avx2 = (ebx_07 >> 5) & 1 == 1;    // EBX bit 5
    }

    unsafe {
        CPU_FEATURES.get().write(f);
    }
}

/// Get a reference to the detected CPU features.
///
/// # Safety
/// Must call `detect()` before reading.
pub fn features() -> &'static CpuFeatures {
    unsafe { &*CPU_FEATURES.get() }
}

/// Enable SMEP (CR4 bit 20), SMAP (CR4 bit 21), and SSE/FPU for user-mode.
///
/// SMEP: Prevents kernel from executing code in user-mode pages.
/// SMAP: Prevents kernel from accessing user-mode data (unless EFLAGS.AC=1).
/// SSE:  CR4.OSFXSR/OSXMMEXCPT for user-mode SIMD, CR0.EM=0, CR0.TS=0.
pub fn enable_smep_smap() {
    let f = features();
    let mut cr4: u64;

    unsafe {
        core::arch::asm!("mov {0}, cr4", out(reg) cr4);
    }

    if f.smep {
        cr4 |= 1 << 20; // CR4.SMEP
    }
    if f.smap {
        cr4 |= 1 << 21; // CR4.SMAP
    }
    if f.sse {
        cr4 |= 1 << 9;  // CR4.OSFXSR — enable FXSAVE/FXRSTOR and SSE
        cr4 |= 1 << 10; // CR4.OSXMMEXCPT — enable SIMD FP exceptions
    }

    unsafe {
        core::arch::asm!("mov cr4, {0}", in(reg) cr4);
    }
}

/// Enable FPU and SSE hardware.
///
/// Clears CR0.EM (bit 2) to enable x87 FPU.
/// Sets CR0.MP (bit 1) to monitor coprocessor.
/// Executes FNINIT to reset x87 state.
/// Sets MXCSR to mask all FP exceptions (0x1F80).
pub fn enable_fpu() {
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {0}, cr0", out(reg) cr0);
        cr0 &= !(1 << 2); // Clear CR0.EM (enable FPU)
        cr0 |= 1 << 1;    // Set CR0.MP (monitor coprocessor)
        core::arch::asm!("mov cr0, {0}", in(reg) cr0);

        // Reset x87 FPU state
        core::arch::asm!("fninit");

        // Set MXCSR to default (mask all FP exceptions: 0x1F80)
        let mxcsr: u32 = 0x1F80;
        core::arch::asm!("ldmxcsr [{ptr}]", ptr = in(reg) &mxcsr as *const u32);
    }
    crate::serial::write_str_nl("[CPU] FPU/SSE enabled");
}

/// Print detected CPU features to serial.
pub fn print_features() {
    let f = features();

    crate::serial::write_str_nl("[CPU] Detected features:");

    crate::serial::write_str("  NX (No Execute):      ");
    crate::serial::write_str_nl(if f.nx { "YES" } else { "NO" });

    crate::serial::write_str("  SMEP:                  ");
    crate::serial::write_str_nl(if f.smep { "YES" } else { "NO" });

    crate::serial::write_str("  SMAP:                  ");
    crate::serial::write_str_nl(if f.smap { "YES" } else { "NO" });

    crate::serial::write_str("  PAT:                   ");
    crate::serial::write_str_nl(if f.pat { "YES" } else { "NO" });

    crate::serial::write_str("  APIC:                  ");
    crate::serial::write_str_nl(if f.apic { "YES" } else { "NO" });

    crate::serial::write_str("  Syscall/Sysret:        ");
    crate::serial::write_str_nl(if f.syscall { "YES" } else { "NO" });

    crate::serial::write_str("  FSGSBASE:              ");
    crate::serial::write_str_nl(if f.fsgsbase { "YES" } else { "NO" });

    crate::serial::write_str("  SSE:                   ");
    crate::serial::write_str_nl(if f.sse { "YES" } else { "NO" });

    crate::serial::write_str("  SSE2:                  ");
    crate::serial::write_str_nl(if f.sse2 { "YES" } else { "NO" });

    crate::serial::write_str("  AVX:                   ");
    crate::serial::write_str_nl(if f.avx { "YES" } else { "NO" });

    crate::serial::write_str("  AVX2:                  ");
    crate::serial::write_str_nl(if f.avx2 { "YES" } else { "NO" });

    crate::serial::write_str("  TSC Deadline Timer:    ");
    crate::serial::write_str_nl(if f.tsc_deadline { "YES" } else { "NO" });

    crate::serial::write_str("  Invariant TSC:         ");
    crate::serial::write_str_nl(if f.invariant_tsc { "YES" } else { "NO" });

    crate::serial::write_str("  Max PHY addr bits:     ");
    crate::serial::write_hex(f.maxphyaddr as u64);
    crate::serial::write_nl();
}
