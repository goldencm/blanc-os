 .text
    .code64

    .global syscall
syscall:
    push rax
    push rdi
    push rsi
    push rdx
    push rcx
    push r8
    push r9
    push r10
    push r11

    mov rax, rdi
    mov rdi, rsi
    mov rsi, rdx
    mov rdx, rcx
    int 0x80

    pop r11
    pop r10
    pop r9
    pop r8
    pop rcx
    pop rdx
    pop rsi
    pop rdi
    add rsp, 8  /* RAX is used to return a value. */
    ret

    .global exit_syscall
exit_syscall:
    mov rax, rdi
    int 0x80