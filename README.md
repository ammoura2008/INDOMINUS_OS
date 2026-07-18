i am new to github , but this is a project i made after i got hacked , i found out tht windows security is trash and it takes a lot of space , linux on the other hand needs a lot to create a folder and just makes eveything harder
so i am trying to make an operating system that can have the security of linux and the effeciency of windows , the project is mostly AI tbh but i am trying to interfer since i am a python devooper mainly and have a surface level understanding of rust and assembly 
"""""""""""""""""""""
# Indo Kernel — Experimental x86_64 Operating System

An experimental **64-bit x86_64 operating system kernel written in Rust**, built from scratch to explore low-level OS development concepts including memory management, interrupts, process scheduling, ELF loading, and user-space execution.

This project is currently in the transition from a bare-metal kernel into a small multitasking OS with user programs.

## Current Status

The kernel successfully boots and initializes:

✅ Bootloader integration  
✅ GDT setup  
✅ IDT setup  
✅ Physical Memory Manager (PMM)  
✅ Virtual Memory Manager (VMM)  
✅ Kernel heap allocator  
✅ LAPIC / IO-APIC initialization  
✅ PIT timer interrupts  
✅ SYSCALL/SYSRET MSR setup  
✅ High-half kernel mapping  
✅ ELF64 loader prototype  
✅ Process structures  
✅ Kernel task creation  

The current blocker is **stable kernel context switching**.
""""""""""""""""""""""
