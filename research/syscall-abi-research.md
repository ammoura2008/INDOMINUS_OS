# Syscall ABI Design Patterns for Custom x86_64 Operating Systems

Research compiled for Indominux Rex OS design.

---

## 1. Linux Syscall ABI (x86_64)

### Register Layout

```
On SYSCALL entry:
  RAX = syscall number
  RDI = arg0
  RSI = arg1
  RDX = arg2
  R10 = arg3  (NOT RCX — SYSCALL clobbers RCX with user RIP)
  R8  = arg4
  R9  = arg5

  RCX = overwritten (holds user return RIP)
  R11 = overwritten (holds saved user RFLAGS)

On return:
  RAX = result (negative = -errno on error)
```

### Key Design Decisions

1. **R10 instead of RCX for arg3**: The `syscall` instruction saves user RIP into RCX and RFLAGS into R11. Since RCX is clobbered, Linux uses R10 (caller-saved in SysV ABI, so no extra save/restore needed in syscall wrappers).

2. **Error reporting**: Negative return values. On success, RAX contains the return value (0 or positive). On failure, RAX contains `-errno` (a negative integer). Glibc wrappers then set the global `errno` variable. This is simpler than the traditional `errno` approach — the kernel never touches userspace memory just to report an error.

3. **Up to 6 arguments in registers**: No argument stack frame needed for most syscalls. More complex syscalls use pointer-to-struct arguments.

4. **Syscall number allocation**: Sequentially numbered, starting from 0 on each architecture. Numbers are assigned in `include/uapi/asm-generic/unistd.h` (generic) and architecture-specific tables. Numbers are **never reused** — removed syscalls keep their numbers forever.

5. **Entry overhead**: The `syscall` instruction itself costs ~10-20 cycles on modern x86_64 CPUs. Full round-trip (entry + minimal handler + exit) is ~40-80 cycles. With KPTI (Kernel Page Table Isolation) enabled, CR3 switch adds ~50-100 cycles. Total realistic overhead: ~100-200 ns per syscall.

### vDSO (Virtual Dynamic Shared Object)

The vDSO is a small ELF shared library the kernel maps into every process at `execve()`. It provides userspace implementations of frequently-called syscalls to avoid ring transitions entirely.

**How it works:**
- Kernel builds a small ELF image at boot, maps it read-only into every process
- Location randomized by ASLR, passed via `AT_SYSINFO_EHDR` auxiliary vector entry
- Exports functions like `__vdso_clock_gettime`, `__vdso_gettimeofday`, `__vdso_getcpu`
- Uses a shared `[vvar]` page (read-only in userspace, read-write in kernel) with a seqlock protocol
- The kernel timer interrupt writes time data to vvar; vDSO code reads it without ring transitions

**Accelerated functions:**
| Function | Mechanism |
|---|---|
| `clock_gettime` | Reads TSC, applies mult/shift from vdso_data |
| `gettimeofday` | Same, plus timezone info |
| `getcpu` | Uses `RDPID` or `RDTSCP` instruction |
| `time` | Coarse resolution, reads pre-computed basetime |

**vsyscall (legacy, x86_64 only):**
- Fixed address `0xffffffffff600000` — ROP gadget risk
- Default: `vsyscall=emulate` (traps to kernel, emulates)
- All modern software uses vDSO exclusively
- Security concern: fixed address enables predictable ROP targets

---

## 2. FreeBSD Syscall ABI

### Register Layout (x86_64)

```
On SYSCALL entry:
  RAX = syscall number
  RDI = arg0
  RSI = arg1
  RDX = arg2
  R10 = arg3
  R8  = arg4
  R9  = arg5

On return:
  RAX = result
  CF (carry flag) = 1 on error, 0 on success
  RAX = positive errno when CF=1
```

### Key Differences from Linux

1. **Error reporting via carry flag**: Instead of returning negative errno, FreeBSD sets the carry flag (CF) on error and stores the positive errno in RAX. This is more like the traditional BSD convention and avoids the need for negation.

2. **Stack-based arguments (32-bit legacy)**: On i386, FreeBSD uses the UNIX convention (arguments on stack) rather than Linux's register convention. On x86_64, both use registers.

3. **syscalls.master**: Centralized definition file that auto-generates syscall stubs, headers, and man pages. Each entry specifies the syscall number, type (STD/OBSOL/UNIMPL), and prototype.

4. **Linux emulation**: FreeBSD has a "Linuxulator" that translates Linux syscall ABI to FreeBSD ABI, including register-to-stack argument copying for i386 Linux binaries.

**Advantages of FreeBSD approach:**
- Carry flag error reporting is cleaner for assembly (no negation)
- Centralized syscalls.master reduces boilerplate
- Stronger type checking from auto-generated stubs

---

## 3. Windows Syscall ABI (x86_64)

### Register Layout

```
ntdll stub pattern:
  mov r10, rcx    ; preserve arg0 (syscall clobbers RCX)
  mov eax, SSN    ; load System Service Number
  syscall
  ret

Kernel reads args in standard Win64 calling convention:
  RCX = arg0 (via R10 in userspace)
  RDX = arg1
  R8  = arg2
  R9  = arg3
  + stack args for arg4+

Return:
  RAX = NTSTATUS (negative = error, 0 = success)
```

### Key Differences from Linux

1. **Two-layer API**: Win32 API (`kernel32.dll`) → Native API (`ntdll.dll`, `Nt*`/`Zw*` stubs) → kernel. The documented Win32 API is a stable backward-compatible abstraction. Direct syscalls into `ntdll` are unsupported and fragile.

2. **No stable syscall numbers**: SSNs (System Service Numbers) change between Windows versions and even patch levels. Hardcoding them is explicitly discouraged. They must be resolved at runtime by reading the `ntdll` stub bytes or via PEB/LDR introspection.

3. **Previous mode distinction**: `Nt*` calls from user mode set previous mode to `UserMode` (requires full pointer validation). `Zw*` calls from kernel set previous mode to `KernelMode` (skips user pointer probing). This is the kernel-vs-driver privilege model.

4. **ProbeForRead/ProbeForWrite**: Windows validates user pointers by confirming they lie entirely within user address range before any access. Data is then copied into kernel memory to prevent TOCTOU (time-of-check/time-of-use) races.

5. **Shadow space**: The Win64 ABI requires callers to reserve 32 bytes of shadow space for callee to spill register arguments. No red zone exists.

**Lessons for a new OS:**
- The Nt/Zw previous-mode pattern is elegant for kernel-internal syscalls
- User pointer probing before access is critical for security
- SSN instability is a cautionary tale — commit to stable numbers

---

## 4. xv6 Syscall ABI

### Simple Educational Model

```
User space:
  - Arguments placed in registers (a0-a5 on RISC-V, or on stack for x86)
  - Syscall number in a7 (RISC-V) or eax (x86)
  - `ecall` (RISC-V) or `int $0x80` (x86) traps to kernel

Kernel:
  syscall() reads number from trapframe, dispatches via function pointer table
  Returns value written to a0 in trapframe (RISC-V) or eax (x86)
```

### Error Handling
- **Negative return values** for errors (like Linux)
- Unknown syscall: returns `-1` and prints error message
- No errno — errors are directly the return value

### Pointer Validation
xv6 uses a simple approach — no hardware SMAP/SMEP:

```c
// fetchint: validate pointer is within process address space
int fetchint(uint addr, int *ip) {
    struct proc *curproc = myproc();
    if (addr >= curproc->sz || addr + 4 > curproc->sz)
        return -1;
    *ip = *(int*)(addr);
    return 0;
}

// argptr: validate pointer + size
int argptr(int n, char **pp, int size) {
    int i;
    if (argint(n, &i) < 0) return -1;
    if (size < 0 || (uint)i >= curproc->sz || (uint)i + size > curproc->sz)
        return -1;
    *pp = (char*)i;
    return 0;
}

// argstr: validate null-terminated string
int argstr(int n, char **pp) {
    int addr;
    if (argint(n, &addr) < 0) return -1;
    return fetchstr(addr, pp);
}
```

**Key insight**: xv6 validates that pointers are within `[0, curproc->sz)` but does NOT use copy_from_user/copy_to_user. It directly dereferences userspace pointers. This is insecure in production but simple for education. A real kernel must use safe memory access primitives.

---

## 5. Redox OS Syscall ABI

### Message-Based Design (Rust-native)

Redox uses a **scheme-based** message-passing architecture:

```
User program
  → relibc/redox-rt (POSIX compatibility layer)
    → syscalls (open, read, write, mmap)
      → kernel
        → converts to SQE (Submission Queue Entry)
          → userspace daemon (scheme provider) reads SQE
            → processes request
            → writes CQE (Completion Queue Entry) back
              → kernel converts CQE to syscall return value
```

### Key Design Points

1. **Tiny kernel syscall set**: Only a few syscalls — message passing, interrupt registration, memory mapping. No file-specific syscalls in the kernel.

2. **Schemes are userspace daemons**: File systems, device drivers, networking — all implemented as userspace services that receive SQE/CQE messages from the kernel.

3. **Bulk syscalls**: Chain multiple syscalls without leaving/re-entering kernel space. Pass an array of syscall packages, execute serially. Reduces context switch overhead.

4. **Unix-compatible ABI**: Despite the message-passing core, Redox maintains a Unix-compatible syscall ABI for compatibility with relibc.

5. **Error handling**: Uses `Result<T, Error>` Rust types internally. Error codes in the upper 4096 values of descriptor numbers (leaving user-visible descriptors in 0..usize::MAX-4096).

### Advantages of Message-Based Approach
- Fewer syscalls = smaller trusted computing base
- Services can crash/restart independently
- Natural capability-based access control (file descriptors = message channels)
- Composability: schemes can chain to other schemes

---

## 6. seL4 Syscall ABI

### Capability-Based Architecture

seL4 has a fundamentally different model — **everything is a capability invocation**:

```
User space executes `syscall` with:
  RDX = seL4 syscall number (Send, Recv, Call, Yield, etc.)
  RDI = capability destination
  RSI = message info (length, label)
  R10, R8, R9, R15 = message registers (MR0-MR3)

Kernel:
  1. Identifies the seL4 operation (Send/Recv/Call)
  2. Looks up the capability
  3. Calls decodeInvocation() to determine specific operation
  4. Each capability type has its own invocation decoder
  5. Two-tiered dispatch: syscall number → capability type → invocation method
```

### Key Differences from Traditional Syscalls

1. **No read/write/open syscalls**: These don't exist. You invoke methods on capabilities (file capabilities, device capabilities, etc.).

2. **IPC is the syscall model**: `Send`, `Recv`, `Call`, `Reply` are the primary operations. Everything else is an IPC message to a service.

3. **Capability validation**: The kernel validates that you hold a valid capability with the right permissions before any operation. No pointer validation needed — you can only operate on objects you have capabilities for.

4. **Formal verification**: seL4 is formally verified — the kernel's correctness (including its syscall ABI) is mathematically proven.

5. **Syscall number in RDX** (not RAX): This is a deliberate divergence from Linux. On x86_64 seL4, the syscall number goes in RDX.

### Security Advantages
- **Principle of least privilege**: Capabilities grant exactly the access needed
- **No ambient authority**: No concept of "root" or process-level permissions
- **Denial by default**: You can only do what your capabilities allow
- **Compositional security**: Security properties compose from individual capabilities

---

## 7. Error Handling Patterns

### Pattern Comparison

| Pattern | Return Type | Error Convention | Used By |
|---|---|---|---|
| Negative errno | `isize` / `int` | Negative = error | Linux, xv6, Windows |
| Carry flag | `RAX` + `CF` | CF=1 means RAX is errno | FreeBSD |
| errno global | `int` + global | Return -1, set `errno` | POSIX/C tradition |
| Result type | `Result<T, E>` | `Ok(T)` or `Err(E)` | Rust, seL4 (internally) |

### For a Rust Kernel — Recommendation: Result Type

```rust
// Kernel-internal representation
pub enum Error {
    EPERM, ENOENT, ESRCH, EINTR, EIO, ...
}

pub type Result<T = (), E = Error> = core::result::Result<T, E>;

// Syscall boundary translation
// Internal kernel code uses Result<T, Error>
// Syscall handlers translate to user-facing convention
```

**Why Result is best for a Rust kernel:**

1. **Type safety**: Compiler enforces error handling. No forgotten errno checks.
2. **No global state**: No `errno` global variable — errors are values.
3. **Composability**: `?` operator, `map`, `and_then`, `unwrap_or_else` etc.
4. **Rich error types**: Can carry context, source location, error chains.
5. **FFI boundary**: At the userspace-kernel boundary, translate to a convention (negative return for C users, or Result-like for Rust users).

**Syscall return convention for userspace:**
- For C/POSIX compatibility: return negative errno in RAX (Linux style)
- For Rust users: could expose a richer Result ABI, but this adds complexity
- **Pragmatic choice**: Use Linux-style negative errno at the ABI boundary for maximum compatibility, use Result internally

---

## 8. Pointer Validation

### The Problem
User-provided pointers must be validated before the kernel dereferences them. A malicious userspace program could pass a kernel address, unmapped address, or a valid userspace address that changes between validation and use (TOCTOU).

### Linux Approach: copy_from_user / copy_to_user

```c
// Never dereference user pointers directly
// Always use these safe primitives
unsigned long copy_from_user(void *to, const void __user *from, unsigned long n);
unsigned long copy_to_user(void __user *to, const void *from, unsigned long n);

// Internally:
// 1. Validates address range (access_ok)
// 2. Disables SMAP (stac instruction)
// 3. Performs memcpy
// 4. Re-enables SMAP (clac instruction)
// 5. Checks for faults
```

### SMAP/SMEP Hardware Support

- **SMAP (Supervisor Mode Access Prevention)**: Prevents kernel from accidentally reading/writing userspace memory. Enabled via CR4 bit 21.
- **SMEP (Supervisor Mode Execution Prevention)**: Prevents kernel from executing userspace code pages. Enabled via CR4 bit 20.
- **How to temporarily disable SMAP**: `stac` (Set AC flag) before copy, `clac` (Clear AC flag) after copy. These are only valid in ring 0.
- **Linux boot-time patching**: Uses `alternative()` macro to replace `stac`/`clac` with NOPs on CPUs without SMAP.

### Hardened Usercopy (CONFIG_HARDENED_USERCOPY)

Linux performs additional checks in `__check_object_size()`:
- Rejects wrapped addresses
- Rejects NULL/zero-alloc pointers
- Validates stack objects are within current stack frame
- Validates heap objects don't exceed allocated size
- Rejects kernel text region access

### xv6 Approach (Simple, Insecure)
Direct pointer dereference after bounds checking:
```c
if (addr >= curproc->sz || addr + size > curproc->sz)
    return -1;
// Direct dereference — no copy_to_user
*ip = *(int*)(addr);
```

### Recommendation for New OS

1. **Always use copy_from_user/copy_to_user** — never dereference user pointers directly
2. **Enable SMAP/SMEP** if targeting modern x86_64
3. **Use stac/clac** around user memory access
4. **Consider address space identifiers (ASIDs)** for faster CR3 switching
5. **Implement access_ok()** that checks the pointer is in user address range (`< USER_ADDR_MAX`)
6. **For extra security**: Pin user pages during multi-step operations to prevent TOCTOU

---

## 9. Syscall Number Allocation

### Linux Strategy

```
// Sequential, architecture-specific
// arch/x86/entry/syscalls/syscall_64.tbl:
0    common  read                 sys_read
1    common  write                sys_write
2    common  open                 sys_open
...
545  common  close_range          sys_close_range
```

**Rules:**
1. **Never reuse numbers**: Removed syscalls keep their slots forever
2. **Sequential allocation**: Numbers assigned in order, gaps reserved for removed syscalls
3. **Architecture-specific ranges**: Each arch can have up to 16 arch-specific syscalls starting at `__NR_arch_specific_syscall` (244 on generic)
4. **compat_sys variants**: 32-bit compatibility wrappers get separate entries in the table
5. **Generic + arch-specific**: Most syscalls defined in `asm-generic/unistd.h`, architectures override if needed

### Adding New Syscalls (Linux Process)

1. Define in `scripts/syscall.tbl` with "common" ABI
2. Update `__NR_syscalls` count
3. Implement `sys_xyzzy()` and optionally `compat_sys_xyzzy()`
4. Add user-space stub, man page, UAPI header entry
5. Handle 32-bit compatibility if pointers or 64-bit types are involved

### Recommendation

- **Sequential allocation** is simplest and most debuggable
- **Never remove — mark as deprecated/unused**
- **Reserve ranges** for future expansion
- Consider a **syscall multiplexer** (like Linux's `socketcall`) for related syscalls to conserve numbers early on

---

## 10. ABI Stability

### Linux's 30-Year Track Record

Linux maintains one of the strongest ABI stability guarantees in computing:

**What's stable:**
- System call interface (numbers, semantics, argument types)
- `/proc` and `/sys` (with exceptions for `/sys/kernel/debug`)
- Device interfaces (`/dev`)

**What's NOT stable:**
- Kernel internal interfaces
- Debug interfaces
- Implementation details accidentally exposed

**How Linux maintains stability:**
1. **Never break userspace** — Linus's #1 rule
2. **New syscalls for new features** — never modify existing ones
3. **Compatibility layers** — `compat_sys_*` for 32-bit on 64-bit
4. **Versioned vDSO symbols** — can update signatures without breaking old binaries
5. **Glibc translation** — library wraps new kernel ABIs for old applications

**Historical examples of ABI evolution:**
- `truncate` → `truncate64` (for large file support)
- `stat` → `stat64` → `newstat` (three generations, all still work)
- `select` → `pselect` → `ppoll` → `epoll` (progressive enhancement)

### Recommendation for New OS

1. **Commit to ABI stability early** — it's easier to maintain than to retrofit
2. **Version your syscall table** — include an ABI version number
3. **Use feature negotiation** — let userspace discover available syscalls
4. **Never remove syscalls** — deprecate and redirect to stubs
5. **Keep a compatibility shim** — `sys_compat_*` for old binaries

---

## 11. Performance Considerations

### Syscall Entry/Exit Costs

| Method | Cycles (approx) | Notes |
|---|---|---|
| `syscall`/`sysret` | 40-80 | Modern x86_64, no KPTI |
| `syscall`/`sysret` + KPTI | 100-200 | CR3 switch + TLB flush |
| `int $0x80`/`iret` | 400-1700 | Legacy, full interrupt handling |
| vDSO (no ring transition) | 1-5 | Memory read only |
| Normal function call | 1-3 | For comparison |

**Benchmark reference**: On Ryzen 7 3700X, `syscall`+`sysret` round-trip averages ~78 cycles in a minimal benchmark. Real syscalls with KPTI cost 100-200 ns.

### Minimizing Overhead

1. **vDSO for hot-path syscalls**: clock_gettime, gettimeofday, getpid, getcpu
2. **Syscall batching** (like Redox bulk syscalls): Execute multiple operations per kernel entry
3. **Minimal register save/restore**: Linux fast path doesn't save all callee-saved regs
4. **Fast path / slow path**: Separate entry points for common cases (e.g., `read` with valid buffer vs. edge cases)
5. **Avoid unnecessary CR3 switches**: ASID (Address Space Identifiers) can avoid full TLB flush on context switch

### vDSO Design Recommendations

1. **Start with clock_gettime and getpid** — highest frequency syscalls
2. **Use seqlock protocol** for concurrent reads from vvar page
3. **Randomize vDSO address** (ASLR) — never use fixed addresses (learn from vsyscall mistake)
4. **Version symbols** — use GNU symbol versioning for future changes
5. **Graceful fallback** — if TSC is unreliable, fall back to real syscall transparently

---

## 12. Recommendations for Indominux Rex OS

### Syscall ABI Specification

```
Register layout (x86_64):
  RAX = syscall number
  RDI = arg0
  RSI = arg1
  RDX = arg2
  R10 = arg3
  R8  = arg4
  R9  = arg5

  RCX = clobbered (user RIP)
  R11 = clobbered (user RFLAGS)

Return:
  RAX = result (negative errno on error, for C compat)
  OR
  RAX = result, RDX = error code (for Rust Result-like ABI)
```

### Error Handling

- **ABI boundary**: Use Linux-style negative errno for C/POSIX compatibility
- **Kernel internal**: Use Rust `Result<T, Error>` throughout
- **Rust userspace**: Could expose a richer ABI with separate result/error registers

### Pointer Validation

- Implement `copy_from_user`/`copy_to_user` using `stac`/`clac` for SMAP
- Enable SMEP to prevent kernel from executing user code
- Consider pinning user pages for multi-step operations

### Syscall Number Allocation

- Sequential, never reuse
- Reserve first 256 for core syscalls
- Reserve 256-511 for POSIX compatibility
- Reserve 512+ for extensions

### vDSO

- Implement from day one for clock_gettime and getpid
- Use seqlock + vvar page pattern
- ASLR-randomize the mapping
- Provide fallback to real syscall

### Capability Layer (optional, future)

- Consider seL4-inspired capability model for IPC
- File descriptors as capabilities
- Fine-grained permission model

---

## References

1. Linux kernel source: `arch/x86/entry/entry_64.S` — SYSCALL entry code
2. Linux vdso(7) man page — vDSO documentation
3. FreeBSD syscalls.master — syscall definition format
4. seL4 Reference Manual v15.0 — capability-based syscall model
5. Redox OS design/syscalls.toml — Rust-native syscall design
6. xv6 book Chapter 4 — educational syscall implementation
7. Windows x64 ABI — Microsoft Learn documentation
8. Linux `include/uapi/asm-generic/unistd.h` — generic syscall numbers
9. Linux `docs/kernel.org/process/adding-syscalls.html` — syscall addition guide
10. kernel.org/doc/rustdoc/latest/kernel/error/ — Rust kernel error handling
11. kernel-internals.org/syscalls/vdso/ — vDSO deep dive
12. Stack Overflow: "How does the Linux kernel temporarily disable x86 SMAP" — SMAP details
13. blitz/kernel_entry_benchmark — SYSCALL cycle measurements
