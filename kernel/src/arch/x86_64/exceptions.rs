use core::arch::global_asm;

use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use super::{apic, gdt};
use crate::qemu::{self, ExitCode};
use crate::serial;

#[unsafe(export_name = "phoenix_timer_registers")]
static mut TIMER_REGISTER_SNAPSHOT: apic::TimerGeneralRegisters =
    apic::TimerGeneralRegisters::EMPTY;

#[unsafe(export_name = "phoenix_timer_capture_marker")]
static mut TIMER_CAPTURE_MARKER: u64 = 0;

global_asm!(
    r#"
    .global phoenix_apic_timer_entry
    .type phoenix_apic_timer_entry,@function
phoenix_apic_timer_entry:
    mov [rip + phoenix_timer_registers + 0], rax
    mov [rip + phoenix_timer_registers + 8], rbx
    mov [rip + phoenix_timer_registers + 16], rcx
    mov [rip + phoenix_timer_registers + 24], rdx
    mov [rip + phoenix_timer_registers + 32], rsi
    mov [rip + phoenix_timer_registers + 40], rdi
    mov [rip + phoenix_timer_registers + 48], rbp
    mov [rip + phoenix_timer_registers + 56], r8
    mov [rip + phoenix_timer_registers + 64], r9
    mov [rip + phoenix_timer_registers + 72], r10
    mov [rip + phoenix_timer_registers + 80], r11
    mov [rip + phoenix_timer_registers + 88], r12
    mov [rip + phoenix_timer_registers + 96], r13
    mov [rip + phoenix_timer_registers + 104], r14
    mov [rip + phoenix_timer_registers + 112], r15
    mov qword ptr [rip + phoenix_timer_capture_marker], 0x54494d52
    cld
    jmp phoenix_apic_timer_rust_handler
    .size phoenix_apic_timer_entry, .-phoenix_apic_timer_entry
"#
);

unsafe extern "C" {
    fn phoenix_apic_timer_entry();
}

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
    idt[apic::SPURIOUS_VECTOR].set_handler_fn(apic_spurious_handler);

    unsafe {
        idt[apic::TIMER_VECTOR]
            .set_handler_addr(VirtAddr::new(phoenix_apic_timer_entry as *const () as u64));
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

#[unsafe(export_name = "phoenix_apic_timer_rust_handler")]
extern "x86-interrupt" fn apic_timer_handler(stack_frame: InterruptStackFrame) {
    let general_registers =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TIMER_REGISTER_SNAPSHOT)) };
    let register_capture_marker =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(TIMER_CAPTURE_MARKER)) };
    let context = apic::TimerInterruptContext {
        instruction_pointer: stack_frame.instruction_pointer.as_u64(),
        stack_pointer: stack_frame.stack_pointer.as_u64(),
        cpu_flags: stack_frame.cpu_flags.bits(),
        general_registers,
        register_capture_marker,
    };

    apic::handle_timer_interrupt(context);
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
