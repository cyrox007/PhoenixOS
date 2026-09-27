pub mod apic;
pub mod context;
pub mod exceptions;
pub mod syscall;
mod gdt;

pub fn init() {
    gdt::init();
    exceptions::init();
}
