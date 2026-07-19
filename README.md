# Indo Kernel — Experimental x86_64 Operating System

i am new to github , but this is a project i made after i got hacked , i found out tht windows security is trash and it takes a lot of space , linux on the other hand needs a lot to create a folder and just makes eveything harder so i am trying to make an operating system that can have the security of linux and the effeciency of windows
I am primarily a Python developer, with only a surface-level understanding of Rust and Assembly when this project started. This project is not the work of an experienced OS engineer; BUT I AM TRYING TO DO MY BEST

The goal is  to replace Linux or Windows on mycomputer and give it for free to those ho have the saame problems or see that my work can suit him better. i know this is very hard ti achive but as a part of the community i will try to make sure the biggest number of people is happy with this os

Indominus Kernel is an experimental x86_64 operating system kernel written from scratch in Rust and Assembly. the name indominus is from the movie jurassic world ...The project has evolved from a bare-metal kernel into a small multitasking operating system capable of running isolated user programs. and i am continuing to update it

---

# Current Status

The kernel currently successfully supports:

✅ UEFI bootloader integration  
✅ 64-bit x86_64 kernel initialization  
✅ GDT and TSS setup  
✅ IDT and exception handling foundation  
✅ Physical Memory Manager (PMM)  
✅ Virtual Memory Manager (VMM)  
✅ Kernel heap allocator  
✅ High-half kernel mapping  
✅ LAPIC / IO-APIC initialization  
✅ PIT timer interrupts  
✅ Preemptive round-robin scheduling  
✅ SYSCALL/SYSRET user-kernel transitions  
✅ ELF64 user program loading  
✅ Ring 3 user execution  
✅ Multiple user processes  
✅ Process lifecycle management  
✅ Zombie process handling  
✅ Basic syscall interface (`write`, `exit`, `yield`, `getpid`)  

---

# Current Development Phase

The kernel has completed the first major multitasking milestone.

The system can now:

1. Boot into a 64-bit kernel environment.
2. Initialize memory and interrupts.
3. Create kernel tasks.
4. Load user-space ELF programs.
5. Transition between Ring 0 and Ring 3.
6. Handle multiple processes.
7. Execute syscalls safely.
8. Terminate user processes while keeping the kernel alive.

The current focus is moving from "running programs" to "protecting the system from programs."

---

# Next Phase: Memory Protection

The next development stage is implementing a complete page fault handling system.

Goals:

- Detect invalid memory access.
- Separate user faults from kernel faults.
- Terminate faulty user programs safely.
- Improve process isolation.
- Build the foundation for more advanced virtual memory features.

Future plans include:

- Better virtual memory management.
- Filesystem support.
- Device drivers.
- More complete user-space environment.
- Security-focused architecture experiments.
- AI-assisted system tools.

---
