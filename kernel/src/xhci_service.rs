//! Непрерывная служба USB xHCI в потоке аппаратного планировщика.
//!
//! Контроллером и его DMA владеет исключительно поток службы. Второй поток
//! читает только атомарные счётчики и подаёт запрос остановки. Ядро освобождает
//! физические страницы лишь после подтверждения выхода обоих потоков.
//! Пустые опросы приостанавливают поток до аппаратного тика, не занимая CPU.

use core::sync::atomic::{AtomicU64, Ordering};

use phoenix_pcie::usb_hid::InputEvent;
use phoenix_pcie::xhci_runtime::{RuntimeLifecycle, RuntimePhase};

use crate::input_service;
use crate::serial;
use crate::xhci_mmio::{ActiveXhci, ServiceResult};

pub struct UsbServiceControl {
    lifecycle: RuntimeLifecycle,
    controller: usize,
    first_keyboard_slot: u8,
    mouse_slot: u8,
    worker_polls: AtomicU64,
    idle_polls: AtomicU64,
    supervisor_ticks: AtomicU64,
    detached: AtomicU64,
    attached: AtomicU64,
    pressed: AtomicU64,
    released: AtomicU64,
    moved: AtomicU64,
    fault: AtomicU64,
}

impl UsbServiceControl {
    /// Указатель должен оставаться действительным до возврата управления
    /// распорядителю после остановки обоих потоков. На одном процессоре
    /// только поток USB обращается к контроллеру с изменяемым доступом.
    pub fn new(controller: &mut ActiveXhci, keyboard_slot: u8, mouse_slot: u8) -> Self {
        Self {
            lifecycle: RuntimeLifecycle::new(),
            controller: controller as *mut ActiveXhci as usize,
            first_keyboard_slot: keyboard_slot,
            mouse_slot,
            worker_polls: AtomicU64::new(0),
            idle_polls: AtomicU64::new(0),
            supervisor_ticks: AtomicU64::new(0),
            detached: AtomicU64::new(0),
            attached: AtomicU64::new(0),
            pressed: AtomicU64::new(0),
            released: AtomicU64::new(0),
            moved: AtomicU64::new(0),
            fault: AtomicU64::new(0),
        }
    }

    pub fn phase(&self) -> RuntimePhase {
        self.lifecycle.phase()
    }

    pub fn report(&self) -> RuntimeReport {
        RuntimeReport {
            worker_polls: self.worker_polls.load(Ordering::Acquire),
            idle_polls: self.idle_polls.load(Ordering::Acquire),
            supervisor_ticks: self.supervisor_ticks.load(Ordering::Acquire),
            detached: self.detached.load(Ordering::Acquire),
            attached: self.attached.load(Ordering::Acquire),
            pressed: self.pressed.load(Ordering::Acquire),
            released: self.released.load(Ordering::Acquire),
            moved: self.moved.load(Ordering::Acquire),
            fault: self.fault.load(Ordering::Acquire),
        }
    }

    fn reached_test_goal(&self) -> bool {
        self.detached.load(Ordering::Acquire) > 0
            && self.attached.load(Ordering::Acquire) > 0
            && self.pressed.load(Ordering::Acquire) > 0
            && self.released.load(Ordering::Acquire) > 0
            && self.moved.load(Ordering::Acquire) > 0
            && self.worker_polls.load(Ordering::Acquire) > 4
            && self.supervisor_ticks.load(Ordering::Acquire) > 1
    }

    fn fail(&self, code: u64) {
        self.fault.store(code, Ordering::Release);
        self.lifecycle.fail();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeReport {
    pub worker_polls: u64,
    pub idle_polls: u64,
    pub supervisor_ticks: u64,
    pub detached: u64,
    pub attached: u64,
    pub pressed: u64,
    pub released: u64,
    pub moved: u64,
    pub fault: u64,
}

pub extern "C" fn service_thread(argument: usize) {
    let state = unsafe { &*(argument as *const UsbServiceControl) };
    if state.lifecycle.start().is_err() {
        state.fail(1);
        return;
    }

    // Безопасность: распорядитель удерживает ActiveXhci и не обращается
    // к нему до подтверждения остановки. Чужие потоки читают лишь атомики.
    let controller = unsafe { &mut *(state.controller as *mut ActiveXhci) };
    let mut keyboard_slot = state.first_keyboard_slot;
    serial::emergency(format_args!("[INFO] xhci scheduled ready: OK\n"));

    loop {
        if state.lifecycle.phase() == RuntimePhase::StopRequested {
            if state.lifecycle.acknowledge_stop().is_err() {
                state.fail(2);
                return;
            }
            serial::emergency(format_args!("[INFO] xhci scheduled stopped: OK\n"));
            return;
        }
        if state.lifecycle.phase() != RuntimePhase::Running {
            state.fail(3);
            return;
        }

        let step = match controller.service_once() {
            Ok(step) => step,
            Err(error) => {
                state.fail(4);
                serial::emergency(format_args!(
                    "[ERROR] xhci scheduled service failed: {error:?}\n"
                ));
                return;
            }
        };
        state.worker_polls.fetch_add(1, Ordering::Release);

        match step {
            ServiceResult::Idle | ServiceResult::Unchanged(_) => {
                state.idle_polls.fetch_add(1, Ordering::Release);
            }
            ServiceResult::Detached(binding) => {
                input_service::publish_device(binding.slot, false);
                if binding.protocol == 1 {
                    keyboard_slot = 0;
                }
                state.detached.fetch_add(1, Ordering::Release);
                serial::emergency(format_args!(
                    "[INFO] xhci scheduled detached: OK port={} slot={}\n",
                    binding.port, binding.slot
                ));
            }
            ServiceResult::Attached(binding) => {
                let configured = controller.configure_hid_for_port(binding.port);
                if let Err(error) = configured {
                    state.fail(5);
                    serial::emergency(format_args!(
                        "[ERROR] xhci scheduled configure failed: {error:?}\n"
                    ));
                    return;
                }
                if binding.protocol == 1 {
                    keyboard_slot = binding.slot;
                }
                input_service::publish_device(binding.slot, true);
                state.attached.fetch_add(1, Ordering::Release);
                serial::emergency(format_args!(
                    "[INFO] xhci scheduled attached: OK port={} slot={}\n",
                    binding.port, binding.slot
                ));
            }
            ServiceResult::Hid(InputEvent::Key(key)) => {
                if keyboard_slot == 0 {
                    state.fail(6);
                    return;
                }
                input_service::publish_hid(keyboard_slot, InputEvent::Key(key));
                if key.usage == 5 && key.pressed {
                    state.pressed.fetch_add(1, Ordering::Release);
                    serial::emergency(format_args!(
                        "[INFO] xhci scheduled key pressed: OK usage=5\n"
                    ));
                }
                if key.usage == 5 && !key.pressed {
                    state.released.fetch_add(1, Ordering::Release);
                    serial::emergency(format_args!(
                        "[INFO] xhci scheduled key released: OK usage=5\n"
                    ));
                }
            }
            ServiceResult::Hid(InputEvent::Mouse(mouse)) => {
                input_service::publish_hid(state.mouse_slot, InputEvent::Mouse(mouse));
                if mouse.delta_x == 7 && mouse.delta_y == 5 {
                    state.moved.fetch_add(1, Ordering::Release);
                    serial::emergency(format_args!(
                        "[INFO] xhci scheduled mouse moved: OK dx=7 dy=5\n"
                    ));
                }
            }
        }

        // Без непрерывного пустого опроса: x2APIC разбудит поток очередным
        // системным тиком. Контроллер не отключается и не сбрасывается.
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// Поток управления не обращается к xHCI и его DMA. Отправляет только
/// запрос остановки, когда QEMU испытал переподключение и весь HID-ввод.
pub extern "C" fn supervisor_thread(argument: usize) {
    let state = unsafe { &*(argument as *const UsbServiceControl) };
    loop {
        if state.lifecycle.phase() == RuntimePhase::Faulted {
            return;
        }
        if state.reached_test_goal() {
            if state.lifecycle.request_stop().is_err() {
                state.fail(7);
                return;
            }
            serial::emergency(format_args!(
                "[INFO] xhci scheduled stop requested: OK\n"
            ));
            return;
        }
        state.supervisor_ticks.fetch_add(1, Ordering::Release);
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}
