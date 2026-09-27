use core::ptr;

use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const DOUBLE_FAULT_STACK_SIZE: usize = 4096 * 5;
const USER_PRIVILEGE_STACK_SIZE: usize = 64 * 1024;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.privilege_stack_table[0] = user_privilege_stack_end();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_stack_end();
        tss
    };
    static ref GDT: (GlobalDescriptorTable, Selectors) = build_gdt();
}

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

#[allow(dead_code)]
#[repr(align(16))]
struct DoubleFaultStack([u8; DOUBLE_FAULT_STACK_SIZE]);

#[allow(dead_code)]
#[repr(align(16))]
struct UserPrivilegeStack([u8; USER_PRIVILEGE_STACK_SIZE]);

static mut DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; DOUBLE_FAULT_STACK_SIZE]);
static mut USER_PRIVILEGE_STACK: UserPrivilegeStack =
    UserPrivilegeStack([0; USER_PRIVILEGE_STACK_SIZE]);

pub fn init() {
    GDT.0.load();

    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(GDT.1.data_selector);
        DS::set_reg(GDT.1.data_selector);
        ES::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}

pub fn kernel_code_selector_raw() -> u16 {
    GDT.1.code_selector.0
}

pub fn user_code_selector_raw() -> u16 {
    GDT.1.user_code_selector.0
}

pub fn user_data_selector_raw() -> u16 {
    GDT.1.user_data_selector.0
}

pub fn user_privilege_stack_ready() -> bool {
    let start = VirtAddr::from_ptr(ptr::addr_of!(USER_PRIVILEGE_STACK)).as_u64();
    let end = TSS.privilege_stack_table[0].as_u64();

    end.saturating_sub(start) == USER_PRIVILEGE_STACK_SIZE as u64 && end & 0xf == 0
}

fn build_gdt() -> (GlobalDescriptorTable, Selectors) {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let data_selector = gdt.append(Descriptor::kernel_data_segment());
    let user_data_selector = gdt.append(Descriptor::user_data_segment());
    let user_code_selector = gdt.append(Descriptor::user_code_segment());
    let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));

    (
        gdt,
        Selectors {
            code_selector,
            data_selector,
            user_code_selector,
            user_data_selector,
            tss_selector,
        },
    )
}

fn double_fault_stack_end() -> VirtAddr {
    let start = VirtAddr::from_ptr(ptr::addr_of!(DOUBLE_FAULT_STACK));
    start + DOUBLE_FAULT_STACK_SIZE as u64
}

fn user_privilege_stack_end() -> VirtAddr {
    let start = VirtAddr::from_ptr(ptr::addr_of!(USER_PRIVILEGE_STACK));
    start + USER_PRIVILEGE_STACK_SIZE as u64
}
