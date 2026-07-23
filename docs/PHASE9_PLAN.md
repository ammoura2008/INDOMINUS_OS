# Phase 9 — Userspace Foundation

**Goal:** Make the shell fully functional. A user can type commands, run programs, use pipes, and interact with the system.

**Why this first:** Everything downstream (filesystem, display, networking) requires a working shell to test and use. Without it, there's no user interface.

**Target:** Shell parses commands → forks → execs external programs → reaps children → handles I/O redirection and pipes → Ctrl+C kills foreground process.

---

## What's Broken / Missing Today

| Gap | Impact | Priority |
|-----|--------|----------|
| Shell only handles built-in commands (help/exit/echo/clear) | Cannot run any external program | CRITICAL |
| No `sys_dup2` wrapper in userspace crate | Cannot redirect stdin/stdout | HIGH |
| No signal/keyboard interrupt delivery to foreground process | Ctrl+C doesn't work | HIGH |
| Shell doesn't fork+exec external commands | Core shell functionality missing | CRITICAL |
| No PATH lookup | Shell can't find executables in /bin/ | HIGH |
| Init process is kernel-mode, doesn't use syscalls | Can't communicate with shell | MEDIUM |
| No `sys_chdir` / `sys_getcwd` | Can't change directories | MEDIUM |
| No environment variable support | Limited shell features | LOW |

---

## Deliverables

### 9.1 Shell: External Command Execution

**What:** Shell can find, fork, and exec any ELF binary from `/bin/` or `/`.

**How:**
1. Parse input line into command + args (split on spaces)
2. Try `exec("/bin/{cmd}")` — if ENOENT, try `exec("/{cmd}")`
3. If exec fails, print "command not found"
4. Shell runs in a loop, forking for each command

**Files:**
- `userspace/shell/src/lib.rs` — rewrite main loop
- `userspace/syscall/src/lib.rs` — add missing wrappers

**Test:** Type `hello` → shell forks → execs `/bin/hello` → hello prints → returns to shell prompt.

### 9.2 Shell: I/O Redirection

**What:** `ls > output.txt` writes stdout to a file. `cat < input.txt` reads from a file.

**How:**
1. Parse `>` and `<` tokens in command line
2. Before exec: `open(path, flags)` → `dup2(fd, 1)` for `>`, `dup2(fd, 0)` for `<`
3. Close original fd after dup2

**Files:**
- `userspace/shell/src/lib.rs` — add redirection parsing
- `userspace/syscall/src/lib.rs` — add `dup2`, `open` with flags

**Test:** `echo hello > test.txt` → file created with "hello". `cat < test.txt` → prints "hello".

### 9.3 Shell: Pipes

**What:** `ls | grep foo` connects stdout of `ls` to stdin of `grep`.

**How:**
1. Parse `|` token
2. `pipe()` → returns (read_fd, write_fd)
3. Fork child 1: `dup2(pipe_write, 1)` → exec `ls`
4. Fork child 2: `dup2(pipe_read, 0)` → exec `grep`
5. Close pipe fds in parent, wait for both children

**Files:**
- `userspace/shell/src/lib.rs` — add pipe parsing

**Test:** `echo hello | cat` → prints "hello" (cat reads from pipe).

### 9.4 Shell: Ctrl+C (SIGINT)

**What:** Pressing Ctrl+C kills the foreground process, not the shell.

**How:**
1. Keyboard driver detects Ctrl+C (scancode 0x1E + 0x2D)
2. Kernel sends signal to foreground process group
3. Default signal handler terminates the process
4. Shell is immune (handles SIGINT by continuing)

**Files:**
- `indo-kernel/src/keyboard.rs` — detect Ctrl+C, send signal
- `indo-kernel/src/process/scheduler.rs` — add signal delivery
- `indo-kernel/src/process/process.rs` — add signal state

**Test:** Run a long-running process → press Ctrl+C → process killed, shell prompt returns.

### 9.5 Syscall Additions

**New syscalls needed:**

| # | Name | Args | Purpose |
|---|------|------|---------|
| 14 | dup2 | oldfd, newfd | Redirect file descriptors |
| 16 | chdir | path | Change working directory |
| 17 | getcwd | buf, size | Get current working directory |

**Files:**
- `indo-kernel/src/syscall/mod.rs` — add dup2, chdir, getcwd
- `userspace/syscall/src/lib.rs` — add wrappers

### 9.6 Init Process Improvement

**What:** Init becomes a proper user-mode process that spawns the shell and reaps orphans.

**How:**
1. Init is now a user-mode ELF (not kernel-mode)
2. It forks → execs `/bin/indosh`
3. Main loop: `waitpid(-1)` to reap any orphaned zombie
4. When shell exits, init re-spawns it

**Files:**
- `userspace/init/src/lib.rs` — rewrite as user-mode init
- `indo-kernel/src/process/mod.rs` — init spawns user init from ELF

---

## Implementation Order

```
Week 1: Shell basics
  ├── 9.5: Add dup2/chdir/getcwd syscalls (kernel + userspace crate)
  ├── 9.1: Shell external command execution (fork+exec)
  └── Test: shell runs /bin/hello, /bin/init

Week 2: I/O and pipes
  ├── 9.2: Shell I/O redirection (> and <)
  ├── 9.3: Shell pipes (|)
  └── Test: echo > file, cat < file, echo | cat

Week 3: Signals and init
  ├── 9.4: Ctrl+C signal delivery
  ├── 9.6: User-mode init process
  └── Test: Ctrl+C kills foreground, init reaps orphans

Week 4: Stabilization
  ├── End-to-end testing
  ├── Fix bugs found during testing
  └── Regression test update
```

---

## Success Criteria

- [ ] Shell starts and displays prompt
- [ ] `help` shows available commands
- [ ] `hello` runs /bin/hello (external command)
- [ ] `echo hello > test.txt` creates file with content
- [ ] `cat < test.txt` prints file content
- [ ] `echo hello | cat` prints "hello" (pipe works)
- [ ] Ctrl+C kills foreground process, shell survives
- [ ] Init reaps orphaned zombies
- [ ] No page faults, no triple faults
- [ ] All existing tests still pass

---

## Future Phases (Roadmap)

| Phase | Name | Depends On | Key Deliverable |
|-------|------|------------|-----------------|
| 10 | Storage & Block Devices | Phase 5 | AHCI driver, block device abstraction |
| 11 | Filesystem | Phase 10 | FAT32 read/write, mount system |
| 12 | Display & Graphics | Phase 1 | Framebuffer, font renderer, console |
| 13 | Input System | Phase 2, 5 | PS/2 mouse, input event dispatch |
| 14 | Networking | Phase 4, 5 | E1000 driver, TCP/IP stack |
| 15 | Memory Protection | Phase 4 | CoW, mmap, demand paging |
| 16 | Process Lifecycle | Phase 9 | Signals, job control, sessions |
| 17 | Window Manager | Phase 12, 13 | Compositor, window management |
| 18 | UI Toolkit | Phase 17 | Widget library, theme engine |
| 19 | Shell & Utilities | Phase 17, 18 | Terminal, file manager, text editor |
| 20 | Package Manager | Phase 11, 14 | Package format, repository |

---

## Critical Path to Daily-Use OS

```
Phase 9 (Shell)          ← WE ARE HERE
    │
    ├──> Phase 10 (Storage) ──> Phase 11 (Filesystem)
    │                              │
    │                              ├──> Phase 20 (Package Manager)
    │                              └──> Phase 24 (Installer)
    │
    ├──> Phase 12 (Display) ──> Phase 13 (Input) ──> Phase 17 (Window Manager)
    │                                                         │
    │                                                         ├──> Phase 18 (UI Toolkit)
    │                                                         └──> Phase 19 (Shell & Utilities)
    │
    ├──> Phase 14 (Networking)
    │
    ├──> Phase 15 (Memory Protection) ──> Phase 16 (Process Lifecycle)
    │
    └──> Phase 21 (Security) ──> Phase 27 (Polish) ──> Phase 28 (Release)
```

**Minimum viable daily-use OS:** Phases 9 + 10 + 11 + 12 + 13 + 14 + 17 + 19

**Estimated effort per phase:** 1-2 weeks (solo developer, focused)
