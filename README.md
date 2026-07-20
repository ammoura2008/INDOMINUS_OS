# Indo Kernel — Experimental x86_64 Operating System

i am new to github , but this is a project i made after i got hacked , i found out tht windows security is trash and it takes a lot of space , linux on the other hand needs a lot to create a folder and just makes eveything harder so i am trying to make an operating system that can have the security of linux and the effeciency of windows
I am primarily a Python developer, with only a surface-level understanding of Rust and Assembly when this project started. This project is not the work of an experienced OS engineer; BUT I AM TRYING TO DO MY BEST

The goal is  to replace Linux or Windows on mycomputer and give it for free to those ho have the saame problems or see that my work can suit him better. i know this is very hard ti achive but as a part of the community i will try to make sure the biggest number of people is happy with this os

Indominus Kernel is an experimental x86_64 operating system kernel written from scratch in Rust and Assembly. the name indominus is from the movie jurassic world ...The project has evolved from a bare-metal kernel into a small multitasking operating system capable of running isolated user programs. and i am continuing to update it

---

Current Vision

INDOMINUS OS focuses on three core principles:

🛡️ Security First

The operating system is designed around strong isolation:

User programs run separately from the kernel
Ring 3 user-space execution
Page fault isolation
NX (No Execute) memory protection
Future application sandboxing
Future memory protection improvements

The goal is to make unsafe behavior fail safely instead of compromising the entire system.

⚡ Lightweight Performance

INDOMINUS OS aims to remain small and efficient:

Minimal kernel footprint
Custom memory management
No unnecessary background services
Direct hardware interaction
Controlled resource usage

Current release kernel size:

~126 KB release ELF kernel
🧩 Native User Experience

Instead of relying on many external extensions, future versions aim to integrate useful features directly into the operating system:

Native application isolation
Intelligent window management
Built-in recovery/versioning systems
Lightweight customization engine
Efficient system tools
Current Kernel Status
Boot & Hardware

✅ UEFI boot integration
✅ x86_64 kernel
✅ GDT initialization
✅ TSS setup
✅ IDT interrupt handling
✅ LAPIC initialization
✅ PIT timer interrupts

Memory Management

✅ Physical Memory Manager (PMM)
✅ Virtual Memory Manager (VMM)
✅ High-half kernel mapping
✅ Kernel heap allocator
✅ Page table management foundation
✅ Memory frame zeroing
✅ NX memory protection support

Processes & Execution

✅ Process structures
✅ Kernel task creation
✅ Round-robin scheduler
✅ User-space Ring 3 execution
✅ ELF64 loader prototype
✅ SYSCALL/SYSRET setup
✅ User process isolation

Fault Handling & Security

✅ User page fault detection
✅ User process termination on invalid memory access
✅ Kernel fault separation
✅ Page fault diagnostics
✅ NX protection for non-executable memory

Development Philosophy

INDOMINUS OS follows a simple rule:
Every feature must first have a stable foundation.
The project prioritizes:

Correctness
Security
Stability
Performance
User experience

New features are added only after the underlying architecture can support them safely.
Roadmap Ideas
Future exploration:

Application sandboxing
Copy-on-write memory
Demand paging
Better filesystem design
Device abstraction
Native desktop environment
Lightweight security model
AI-assisted system management

THIS README WAS MDE BY CHATGPT BUT REVIEWED BY OMAR MOUAKHAR
