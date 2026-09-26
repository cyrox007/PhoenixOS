pub mod exceptions;
mod gdt;

pub fn init() {
    gdt::init();
    exceptions::init();
}
