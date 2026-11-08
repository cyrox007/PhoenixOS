//! Учёт подключённых USB-устройств для одного контроллера xHCI.
//!
//! Не владеет аппаратной памятью и не выдаёт номера слотов: их подтверждает
//! контроллер. Запись обновляется лишь после успешной аппаратной операции.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbBinding {
    pub port: u8,
    pub slot: u8,
    pub protocol: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbServiceError {
    InvalidBinding,
    DuplicatePort,
    DuplicateSlot,
    NoFreeContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbService {
    bindings: [Option<UsbBinding>; 2],
}

impl UsbService {
    pub const fn new() -> Self {
        Self {
            bindings: [None; 2],
        }
    }

    pub fn for_port(&self, port: u8) -> Option<UsbBinding> {
        self.bindings
            .iter()
            .flatten()
            .copied()
            .find(|entry| entry.port == port)
    }

    pub fn for_index(&self, index: usize) -> Option<UsbBinding> {
        self.bindings.get(index).copied().flatten()
    }

    pub fn vacant_index(&self) -> Option<usize> {
        self.bindings.iter().position(Option::is_none)
    }

    pub fn active_count(&self) -> usize {
        self.bindings.iter().flatten().count()
    }

    pub fn attach(&mut self, binding: UsbBinding) -> Result<usize, UsbServiceError> {
        if binding.port == 0 || binding.slot == 0 {
            return Err(UsbServiceError::InvalidBinding);
        }
        if self.for_port(binding.port).is_some() {
            return Err(UsbServiceError::DuplicatePort);
        }
        if self
            .bindings
            .iter()
            .flatten()
            .any(|entry| entry.slot == binding.slot)
        {
            return Err(UsbServiceError::DuplicateSlot);
        }
        let index = self.vacant_index().ok_or(UsbServiceError::NoFreeContext)?;
        self.bindings[index] = Some(binding);
        Ok(index)
    }

    /// Вызвать только ПОСЛЕ подтверждённой команды Disable Slot.
    pub fn detach(&mut self, port: u8) -> Option<UsbBinding> {
        let index = self
            .bindings
            .iter()
            .position(|entry| entry.is_some_and(|b| b.port == port))?;
        self.bindings[index].take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard() -> UsbBinding {
        UsbBinding {
            port: 5,
            slot: 1,
            protocol: 1,
        }
    }

    fn mouse() -> UsbBinding {
        UsbBinding {
            port: 6,
            slot: 2,
            protocol: 2,
        }
    }

    #[test]
    fn retains_two_devices_and_frees_only_detached_position() {
        let mut service = UsbService::new();
        assert_eq!(service.attach(keyboard()), Ok(0));
        assert_eq!(service.attach(mouse()), Ok(1));
        assert_eq!(service.active_count(), 2);
        assert_eq!(service.for_port(5), Some(keyboard()));
        assert_eq!(service.detach(5), Some(keyboard()));
        assert_eq!(service.active_count(), 1);
        assert_eq!(service.for_port(6), Some(mouse()));
        assert_eq!(
            service.attach(UsbBinding {
                port: 5,
                slot: 3,
                protocol: 1
            }),
            Ok(0)
        );
        assert_eq!(service.for_index(1), Some(mouse()));
    }

    #[test]
    fn refuses_conflicting_and_out_of_range_bindings() {
        let mut service = UsbService::new();
        assert_eq!(
            service.attach(UsbBinding {
                port: 0,
                slot: 1,
                protocol: 1
            }),
            Err(UsbServiceError::InvalidBinding)
        );
        assert_eq!(service.attach(keyboard()), Ok(0));
        assert_eq!(
            service.attach(keyboard()),
            Err(UsbServiceError::DuplicatePort)
        );
        assert_eq!(
            service.attach(UsbBinding {
                port: 6,
                slot: 1,
                protocol: 2
            }),
            Err(UsbServiceError::DuplicateSlot)
        );
        assert_eq!(service.attach(mouse()), Ok(1));
        assert_eq!(
            service.attach(UsbBinding {
                port: 7,
                slot: 3,
                protocol: 1
            }),
            Err(UsbServiceError::NoFreeContext)
        );
        assert_eq!(service.detach(7), None);
        assert_eq!(service.active_count(), 2);
    }
}
