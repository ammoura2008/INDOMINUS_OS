# INDOMINUS OS — Next Phase Recommendation

## Research Summary

### What Redox OS Did
- Built microkernel + shell + basic apps first
- Created their own filesystem (RedoxFS) — CoW, encrypted
- **Self-hosting** (building Redox ON Redox) is their #1 goal — they've been working on it 10+ years
- Current priorities: Self-hosting → Compliance → Performance → Hardware → Desktop
- Key lesson: Shell + filesystem + basic apps = minimum viable OS. Everything else is polish.

### What SerenityOS Did
- Andreas Kling (single developer) started Oct 2018
- Year 1: Kernel → GUI → Networking → IRC client → DOOM → Web browser
- Year 2: JavaScript engine → HTTPS → JPEG → IDE → games
- Year 3: Browser engine maturity → standards compliance
- Year 4: Cross-platform browser (Ladybird) → new language (Jakt)
- **Key lesson:** Visual progress keeps motivation alive. Build something you can SEE and USE quickly.

### OSDev Wiki Consensus
Bootloader → Kernel → Memory → Interrupts → Scheduler → Syscalls → **Shell** → **Storage** → **Filesystem** → Applications

### Where INDOMINUS Is Today
✅ Bootloader (UEFI)
✅ Memory (PMM + VMM + heap)
✅ Interrupts (LAPIC + IOAPIC + PIT)
✅ Scheduler (round-robin, preemptive)
✅ Syscalls (16 syscalls)
✅ User programs (ELF loader, Ring 3)
✅ Shell (basic — 4 built-in commands only)
✅ RAM filesystem
❌ Shell cannot run external commands
❌ No disk driver (AHCI/NVMe)
❌ No real filesystem (FAT32/ext2)
❌ No display (framebuffer) — serial only
❌ No mouse support
❌ No networking

---

## Recommendation: Phase 9 = Storage + Filesystem + Shell

**Why NOT just "fix the shell" first:**
A shell without storage is a dead end. You can't:
- Save files between reboots
- Load programs from disk
- Install new tools
- Build a user environment

**Why NOT "display/framebuffer" first:**
A pretty screen without content is wallpaper. You need data first.

**The right order (based on Redox + Serenity + OSDev consensus):**

### Phase 9: Storage + Filesystem + Shell (4-6 weeks)

```
Week 1-2: AHCI SATA driver
  - Detect AHCI controller via PCI
  - Map ABAR (MMIO registers)
  - Initialize HBA (host bus adapter)
  - READ_DMA_EXT, WRITE_DMA_EXT commands
  - Block device abstraction layer
  - Test: read sector 0 from QEMU disk image

Week 3-4: FAT32 filesystem
  - Read FAT32 boot sector (BPB)
  - Parse FAT table, cluster chains
  - Directory entry parsing
  - File read/write operations
  - Mount system (VFS integration)
  - Test: create/read/write files on disk

Week 4-5: Shell completion
  - Fork+exec external commands
  - I/O redirection (>, <)
  - Pipes (|)
  - Ctrl+C signal delivery
  - PATH lookup (/bin/, /)
  - Test: "ls", "echo hello > file", "cat file", "echo hi | cat"

Week 5-6: Init + stabilization
  - User-mode init (forks, execs shell, reaps orphans)
  - End-to-end testing
  - Regression test update
```

### Phase 10: Display + Console (3-4 weeks)

```
- VGA/VESA framebuffer driver
- Font renderer (8x16 bitmap font)
- Console driver (text output to screen)
- ANSI escape code support
- Scrolling buffer
- Test: kernel messages appear on screen
```

### Phase 11: Input + Process Lifecycle (2-3 weeks)

```
- PS/2 mouse driver
- Input event system
- Signal delivery (SIGINT, SIGTERM, SIGKILL)
- Job control (fg, bg)
- Test: mouse moves, Ctrl+C works, jobs managed
```

### Phase 12: Networking (3-4 weeks)

```
- E1000/Virtio-net NIC driver
- ARP, IPv4, ICMP (ping)
- TCP/UDP sockets
- DNS resolver
- Test: ping gateway, download file via HTTP
```

### Phase 13: Window Manager (4-6 weeks)

```
- Compositor
- Window management
- Input routing
- Basic apps: file manager, text editor, system monitor
```

---

## Why This Order Works

| Phase | What You Get | Why It Matters |
|-------|-------------|----------------|
| 9 | Disk + Files + Working Shell | You can SAVE things. You can RUN programs. This is an OS. |
| 10 | Screen output | You can SEE what's happening. Debugging 10x faster. |
| 11 | Mouse + Signals | You can INTERACT properly. User experience. |
| 12 | Network | You can CONNECT. Download tools, browse web. |
| 13 | Windows + Apps | You can USE it daily. Desktop experience. |

**Minimum viable daily-use OS:** Phases 9 + 10 + 11 + 12 + 13

---

## Key Insight from Research

> Redox OS spent 10+ years and still isn't self-hosting.
> SerenityOS built a working desktop in 1 year by focusing on visual progress.

**For INDOMINUS:** Focus on **Phase 9 (Storage + Filesystem + Shell)** because:
1. It's the foundation everything else builds on
2. It's testable (you can verify it works)
3. It's the "tipping point" — after this, you have a REAL OS
4. Without it, you're stuck with RAM-only test binaries

After Phase 9, you'll have:
- A shell that can run any program
- Files that survive reboots
- A real /bin/ directory with tools
- The ability to add new programs without rebuilding the kernel

That's when the fun begins.
