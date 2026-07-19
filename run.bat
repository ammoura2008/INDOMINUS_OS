@echo off
"C:\Program Files\qemu\qemu-system-x86_64.exe" ^
  -drive if=pflash,format=raw,readonly=on,file="C:\Program Files\qemu\share\edk2-x86_64-code.fd" ^
  -drive format=raw,file=fat:rw:"C:\Users\USER\Documents\indominux rex operating system\build\esp" ^
  -serial stdio -nographic -no-reboot -monitor none
