use lazy_static::lazy_static;
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use super::{apic, gdt};
use crate::qemu::{self, ExitCode};
use crate::serial;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = build_idt();
}

pub fn init() {
    IDT.load();
}

pub fn smoke_test_breakpoint() {
    interrupts::int3();
}

fn build_idt() -> InterruptDescriptorTable {
    let mut idt = InterruptDescriptorTable::new();

    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    idt[apic::TIMER_VECTOR].set_handler_fn(apic_timer_handler);
    idt[apic::SPURIOUS_VECTOR].set_handler_fn(apic_spurious_handler);

    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }

    idt
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial::emergency(format_args!(
        "[INFO] exception: breakpoint\n{stack_frame:#?}\n"
    ));
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("invalid opcode", &stack_frame, None);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    fatal_exception("general protection fault", &stack_frame, Some(error_code));
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    serial::emergency(format_args!(
        "[FATAL] exception: page fault; error={error_code:?}\n{stack_frame:#?}\n"
    ));
    qemu::exit(ExitCode::Failure);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    fatal_exception("double fault", &stack_frame, Some(error_code));
}

extern "x86-interrupt" fn apic_timer_handler(_stack_frame: InterruptStackFrame) {
    apic::handle_timer_interrupt();
}

extern "x86-interrupt" fn apic_spurious_handler(_stack_frame: InterruptStackFrame) {}

fn fatal_exception(name: &str, stack_frame: &InterruptStackFrame, error_code: Option<u64>) -> ! {
    match error_code {
        Some(code) => serial::emergency(format_args!(
            "[FATAL] exception: {name}; error={code:#x}\n{stack_frame:#?}\n"
        )),
        None => serial::emergency(format_args!(
            "[FATAL] exception: {name}\n{stack_frame:#?}\n"
        )),
    }

    qemu::exit(ExitCode::Failure);
}
