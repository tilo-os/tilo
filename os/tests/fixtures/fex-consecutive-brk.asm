; Regression: a syscall result must not replace the cached syscall number
; when the next instruction sequence loads the same immediate into eax.
; Assemble with NASM -f elf64; link with ld.lld -m elf_x86_64 -static.
BITS 64
global _start
section .text
_start:
    xor edi, edi
    mov esi, 4096
    mov eax, 12
    syscall
    mov rdx, rax
    lea rdi, [rax+rsi]
    mov eax, 12
    syscall
    cmp rax, rdi
    jne failed
    mov byte [rdx], 42
    xor edi, edi
    jmp done
failed:
    mov edi, 1
done:
    mov eax, 60
    syscall
    ud2
