pub mod apic;
pub mod context;
pub mod exceptions;
mod gdt;
pub mod syscall;

pub fn init() {
    gdt::init();
    exceptions::init();
}

pub fn user_privilege_stack_ready() -> bool {
    gdt::user_privilege_stack_ready()
}
