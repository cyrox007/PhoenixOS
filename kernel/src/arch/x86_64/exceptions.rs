use core::arch::global_asm;
use core::mem::size_of;

use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use super::{apic, gdt};
use crate::qemu::{self, ExitCode};
use crate::serial;

#[repr(C)]
struct TimerInterruptStackFrame {
    general_registers: apic::TimerGeneralRegisters,
}

#[repr(C)]
struct PreparedKernelTimerFrame {
    general_registers: apic::TimerGeneralRegisters,
    instruction_pointer: u64,
    code_segment: u64,
    cpu_flags: u64,
}

impl PreparedKernelTimerFrame {
    fn new(instruction_pointer: u64) -> Self {
        Self {
            general_registers: apic::TimerGeneralRegisters::EMPTY,
            instruction_pointer,
            code_segment: u64::from(gdt::kernel_code_selector_raw()),
            cpu_flags: (1 << 1) | (1 << 9),
        }
    }

    fn stack_frame_address(&self) -> u64 {
        self as *const Self as u64
    }
}

impl TimerInterruptStackFrame {
    fn hardware_frame_address(&self) -> u64 {
        self as *const Self as u64 + size_of::<Self>() as u64
    }

    fn hardware_word(&self, index: usize) -> u64 {
        let address = self.hardware_frame_address() + (index * size_of::<u64>()) as u64;
        unsafe { (address as *const u64).read() }
    }

    fn instruction_pointer(&self) -> u64 {
        self.hardware_word(0)
    }

    fn code_segment(&self) -> u64 {
        self.hardware_word(1)
    }

    fn cpu_flags(&self) -> u64 {
        self.hardware_word(2)
    }

    fn interrupted_stack_pointer(&self) -> u64 {
        if self.code_segment() & 0b11 != 0 {
            return self.hardware_word(3);
        }

        self.hardware_frame_address() + 3 * size_of::<u64>() as u64
    }
}

global_asm!(
    r#"
    .global phoenix_apic_timer_entry
    .type phoenix_apic_timer_entry,@function
phoenix_apic_timer_entry:
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi
    push rdx
    push rcx
    push rbx
    push rax

    cld
    mov rdi, rsp
    and rsp, -16
    call phoenix_apic_timer_rust_handler
    mov rsp, rax

    pop rax
    pop rbx
    pop rcx
    pop rdx
    pop rsi
    pop rdi
    pop rbp
    pop r8
    pop r9
    pop r10
    pop r11
    pop r12
    pop r13
    pop r14
    pop r15
    iretq
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

pub fn prepared_kernel_timer_frame_self_test() -> bool {
    const TEST_INSTRUCTION_POINTER: u64 = 0xffff_8000_1234_5678;

    let frame = PreparedKernelTimerFrame::new(TEST_INSTRUCTION_POINTER);
    let base = frame.stack_frame_address();
    let registers = &frame.general_registers as *const apic::TimerGeneralRegisters as u64;
    let instruction_pointer = &frame.instruction_pointer as *const u64 as u64;
    let code_segment = &frame.code_segment as *const u64 as u64;
    let cpu_flags = &frame.cpu_flags as *const u64 as u64;

    size_of::<PreparedKernelTimerFrame>()
        == size_of::<apic::TimerGeneralRegisters>() + 3 * size_of::<u64>()
        && registers == base
        && instruction_pointer == base + size_of::<apic::TimerGeneralRegisters>() as u64
        && code_segment == instruction_pointer + size_of::<u64>() as u64
        && cpu_flags == code_segment + size_of::<u64>() as u64
        && frame.instruction_pointer == TEST_INSTRUCTION_POINTER
        && frame.code_segment & 0b11 == 0
        && frame.cpu_flags & (1 << 1) != 0
        && frame.cpu_flags & (1 << 9) != 0
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
extern "C" fn apic_timer_handler(frame: *mut TimerInterruptStackFrame) -> u64 {
    let frame = unsafe { &mut *frame };
    let context = apic::TimerInterruptContext {
        instruction_pointer: frame.instruction_pointer(),
        stack_pointer: frame.interrupted_stack_pointer(),
        cpu_flags: frame.cpu_flags(),
        general_registers: frame.general_registers,
        register_capture_marker: apic::TIMER_REGISTER_CAPTURE_MARKER,
        stack_frame_address: frame as *mut TimerInterruptStackFrame as u64,
    };

    apic::handle_timer_interrupt(context)
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
