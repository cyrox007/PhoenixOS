use core::arch::global_asm;

const SAVED_REGISTER_COUNT: usize = 6;
const FRAME_WORD_COUNT: usize = SAVED_REGISTER_COUNT + 1;
const FRAME_SIZE: usize = FRAME_WORD_COUNT * size_of::<u64>();
const STACK_ALIGNMENT: usize = 16;

global_asm!(
    r#"
    .global phoenix_context_switch
    .type phoenix_context_switch,@function
phoenix_context_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
    .size phoenix_context_switch, .-phoenix_context_switch

    .global phoenix_thread_trampoline
    .type phoenix_thread_trampoline,@function
phoenix_thread_trampoline:
    mov rdi, r13
    call r12
    ud2
    .size phoenix_thread_trampoline, .-phoenix_thread_trampoline
"#
);

unsafe extern "C" {
    fn phoenix_context_switch(old_rsp: *mut u64, new_rsp: u64);
    fn phoenix_thread_trampoline();
}

pub type ThreadEntry = extern "C" fn(usize) -> !;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackError {
    TooSmall,
    AddressOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    rsp: u64,
}

impl Context {
    pub const fn empty() -> Self {
        Self { rsp: 0 }
    }

    pub fn for_stack(
        stack: &mut [u8],
        entry: ThreadEntry,
        argument: usize,
    ) -> Result<Self, StackError> {
        let base = stack.as_mut_ptr() as usize;
        let top = base
            .checked_add(stack.len())
            .ok_or(StackError::AddressOverflow)?;
        let aligned_top = top & !(STACK_ALIGNMENT - 1);

        if aligned_top < base || aligned_top - base < FRAME_SIZE {
            return Err(StackError::TooSmall);
        }

        let rsp = aligned_top - FRAME_SIZE;
        let frame = rsp as *mut u64;

        unsafe {
            frame.add(0).write(0);
            frame.add(1).write(0);
            frame.add(2).write(argument as u64);
            frame.add(3).write(entry as usize as u64);
            frame.add(4).write(0);
            frame.add(5).write(0);
            frame
                .add(6)
                .write(phoenix_thread_trampoline as usize as u64);
        }

        Ok(Self { rsp: rsp as u64 })
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::empty()
    }
}

/// Переключает выполнение с текущего контекста на другой.
///
/// # Безопасность
///
/// Оба контекста должны принадлежать текущему процессору. Новый указатель стека
/// должен быть подготовлен конструктором контекста либо ранее сохранён этой же
/// функцией. Память стека нового контекста обязана оставаться доступной.
pub unsafe fn switch(from: &mut Context, to: &Context) {
    unsafe {
        phoenix_context_switch(&mut from.rsp, to.rsp);
    }
}
