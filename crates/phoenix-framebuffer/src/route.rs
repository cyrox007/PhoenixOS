//! Выбор владельца графического вывода во время загрузки и отказа.
//!
//! Модуль не пишет видеопамять: допуск к операции проверяется отдельно
//! от драйвера, а прежний буфер GOP не освобождается при передаче экрана.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayOwner {
    /// Начальная заставка, пока штатный графический драйвер не запущен.
    FirmwareBoot,
    /// Штатный вывод через подтверждённый видеоконтроллер.
    Native,
    /// Аварийный вывод после отказа или остановки видеоконтроллера.
    FirmwareRecovery,
}

impl DisplayOwner {
    pub const fn name(self) -> &'static str {
        match self {
            Self::FirmwareBoot => "uefi-boot",
            Self::Native => "virtio-gpu",
            Self::FirmwareRecovery => "uefi-emergency",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwarePurpose {
    Boot,
    Emergency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayRouteError {
    NativeNotReady,
    InvalidTransition,
    FirmwareAccessDenied,
    NativeAccessDenied,
}

/// Состояние передачи графики: после подтверждения работы устройства
/// обычное рисование на GOP запрещено даже при сохранённом отображении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRoute {
    owner: DisplayOwner,
}

impl DisplayRoute {
    pub const fn new() -> Self {
        Self {
            owner: DisplayOwner::FirmwareBoot,
        }
    }

    pub const fn owner(self) -> DisplayOwner {
        self.owner
    }

    pub const fn firmware_access(self, purpose: FirmwarePurpose) -> Result<(), DisplayRouteError> {
        match (self.owner, purpose) {
            (DisplayOwner::FirmwareBoot, FirmwarePurpose::Boot)
            | (DisplayOwner::FirmwareRecovery, FirmwarePurpose::Emergency) => Ok(()),
            _ => Err(DisplayRouteError::FirmwareAccessDenied),
        }
    }

    pub const fn native_access(self) -> Result<(), DisplayRouteError> {
        match self.owner {
            DisplayOwner::Native => Ok(()),
            _ => Err(DisplayRouteError::NativeAccessDenied),
        }
    }

    /// Разрешается только после подтверждённых команд переноса и показа
    /// изображения, а также проверки живого ресурса видеопамяти.
    pub fn claim_native(&mut self, ready: bool) -> Result<(), DisplayRouteError> {
        if self.owner != DisplayOwner::FirmwareBoot {
            return Err(DisplayRouteError::InvalidTransition);
        }
        if !ready {
            return Err(DisplayRouteError::NativeNotReady);
        }
        self.owner = DisplayOwner::Native;
        Ok(())
    }

    /// Переключение на запасной буфер при ошибке запуска, отказе драйвера
    /// или его подтверждённой остановке. Повторный захват без новой
    /// процедуры инициализации запрещён.
    pub fn recover(&mut self) {
        self.owner = DisplayOwner::FirmwareRecovery;
    }
}

impl Default for DisplayRoute {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_boot_banner_can_write_firmware_before_handoff() {
        let route = DisplayRoute::new();
        assert_eq!(route.owner(), DisplayOwner::FirmwareBoot);
        assert_eq!(route.firmware_access(FirmwarePurpose::Boot), Ok(()));
        assert_eq!(
            route.firmware_access(FirmwarePurpose::Emergency),
            Err(DisplayRouteError::FirmwareAccessDenied)
        );
        assert_eq!(
            route.native_access(),
            Err(DisplayRouteError::NativeAccessDenied)
        );
    }

    #[test]
    fn native_requires_confirmed_ready_device() {
        let mut route = DisplayRoute::new();
        assert_eq!(
            route.claim_native(false),
            Err(DisplayRouteError::NativeNotReady)
        );
        assert_eq!(route.owner(), DisplayOwner::FirmwareBoot);
        route.claim_native(true).unwrap();
        assert_eq!(route.owner(), DisplayOwner::Native);
        assert_eq!(route.native_access(), Ok(()));
        for purpose in [FirmwarePurpose::Boot, FirmwarePurpose::Emergency] {
            assert_eq!(
                route.firmware_access(purpose),
                Err(DisplayRouteError::FirmwareAccessDenied)
            );
        }
        assert_eq!(
            route.claim_native(true),
            Err(DisplayRouteError::InvalidTransition)
        );
    }

    #[test]
    fn recovery_only_permits_emergency_firmware() {
        let mut route = DisplayRoute::new();
        route.claim_native(true).unwrap();
        route.recover();
        assert_eq!(route.owner(), DisplayOwner::FirmwareRecovery);
        assert_eq!(
            route.firmware_access(FirmwarePurpose::Boot),
            Err(DisplayRouteError::FirmwareAccessDenied)
        );
        assert_eq!(route.firmware_access(FirmwarePurpose::Emergency), Ok(()));
        assert_eq!(
            route.native_access(),
            Err(DisplayRouteError::NativeAccessDenied)
        );
        assert_eq!(
            route.claim_native(true),
            Err(DisplayRouteError::InvalidTransition)
        );
    }

    #[test]
    fn failed_start_can_go_to_firmware_recovery_without_native() {
        let mut route = DisplayRoute::new();
        assert_eq!(
            route.claim_native(false),
            Err(DisplayRouteError::NativeNotReady)
        );
        route.recover();
        assert_eq!(route.firmware_access(FirmwarePurpose::Emergency), Ok(()));
        route.recover();
        assert_eq!(route.owner(), DisplayOwner::FirmwareRecovery);
    }
}
