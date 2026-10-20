#![no_std]

use phoenix_block::{BlockDevice, BlockError, BlockGeometry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProtectionMode {
    BoundProtected,
    PortableProtected,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallationId([u8; 16]);

impl InstallationId {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUnlock {
    Locked,
    Bound(InstallationId),
    Portable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenWriteScope {
    OneOperation,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenWriteGrant {
    None,
    OneOperation,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPolicyError {
    MissingBoundInstallation,
    UnexpectedBoundInstallation,
    WrongMode,
    BoundInstallationMismatch,
    BoundUnlockRequired,
    PortableUnlockRequired,
    OpenWriteConfirmationRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaSession {
    mode: MediaProtectionMode,
    protection_active: bool,
    bound_installation: Option<InstallationId>,
    unlock: MediaUnlock,
    open_write_grant: OpenWriteGrant,
}

impl MediaSession {
    pub const fn new(
        mode: MediaProtectionMode,
        protection_active: bool,
        bound_installation: Option<InstallationId>,
    ) -> Result<Self, MediaPolicyError> {
        if matches!(mode, MediaProtectionMode::BoundProtected) && bound_installation.is_none() {
            return Err(MediaPolicyError::MissingBoundInstallation);
        }

        if !matches!(mode, MediaProtectionMode::BoundProtected) && bound_installation.is_some() {
            return Err(MediaPolicyError::UnexpectedBoundInstallation);
        }

        Ok(Self {
            mode,
            protection_active,
            bound_installation,
            unlock: MediaUnlock::Locked,
            open_write_grant: OpenWriteGrant::None,
        })
    }

    pub const fn mode(&self) -> MediaProtectionMode {
        self.mode
    }

    pub const fn protection_active(&self) -> bool {
        self.protection_active
    }

    pub const fn unlock_state(&self) -> MediaUnlock {
        self.unlock
    }

    pub fn unlock_bound(
        &mut self,
        current_installation: InstallationId,
    ) -> Result<(), MediaPolicyError> {
        if !matches!(self.mode, MediaProtectionMode::BoundProtected) {
            return Err(MediaPolicyError::WrongMode);
        }

        if self.bound_installation != Some(current_installation) {
            return Err(MediaPolicyError::BoundInstallationMismatch);
        }

        self.unlock = MediaUnlock::Bound(current_installation);
        Ok(())
    }

    pub fn unlock_portable(&mut self) -> Result<(), MediaPolicyError> {
        if !matches!(self.mode, MediaProtectionMode::PortableProtected) {
            return Err(MediaPolicyError::WrongMode);
        }

        self.unlock = MediaUnlock::Portable;
        Ok(())
    }

    pub fn confirm_open_write(&mut self, scope: OpenWriteScope) -> Result<(), MediaPolicyError> {
        if !matches!(self.mode, MediaProtectionMode::Open) {
            return Err(MediaPolicyError::WrongMode);
        }

        self.open_write_grant = match scope {
            OpenWriteScope::OneOperation => OpenWriteGrant::OneOperation,
            OpenWriteScope::Session => OpenWriteGrant::Session,
        };
        Ok(())
    }

    pub fn revoke_open_write(&mut self) {
        self.open_write_grant = OpenWriteGrant::None;
    }

    pub fn lock(&mut self) {
        self.unlock = MediaUnlock::Locked;
        self.open_write_grant = OpenWriteGrant::None;
    }

    pub const fn authorize_read(&self) -> Result<(), MediaPolicyError> {
        match self.mode {
            MediaProtectionMode::BoundProtected => match self.unlock {
                MediaUnlock::Bound(installation)
                    if Some(installation) == self.bound_installation =>
                {
                    Ok(())
                }
                _ => Err(MediaPolicyError::BoundUnlockRequired),
            },
            MediaProtectionMode::PortableProtected => match self.unlock {
                MediaUnlock::Portable => Ok(()),
                _ => Err(MediaPolicyError::PortableUnlockRequired),
            },
            MediaProtectionMode::Open => Ok(()),
        }
    }

    pub fn authorize_write(&mut self) -> Result<(), MediaPolicyError> {
        self.authorize_read()?;

        if !matches!(self.mode, MediaProtectionMode::Open) || !self.protection_active {
            return Ok(());
        }

        match self.open_write_grant {
            OpenWriteGrant::None => Err(MediaPolicyError::OpenWriteConfirmationRequired),
            OpenWriteGrant::OneOperation => {
                self.open_write_grant = OpenWriteGrant::None;
                Ok(())
            }
            OpenWriteGrant::Session => Ok(()),
        }
    }
}

/// Блочное устройство внешнего носителя, через которое невозможно обойти
/// выбранную политику чтения и записи.
///
/// Доступ к нижележащему устройству не выдаётся по изменяемой ссылке. Извлечь
/// его можно только потребив обёртку целиком.
pub struct ExternalMediaDevice<D> {
    inner: D,
    session: MediaSession,
}

impl<D> ExternalMediaDevice<D> {
    pub const fn new(inner: D, session: MediaSession) -> Self {
        Self { inner, session }
    }

    pub const fn session(&self) -> &MediaSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut MediaSession {
        &mut self.session
    }

    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: BlockDevice> BlockDevice for ExternalMediaDevice<D> {
    fn geometry(&self) -> BlockGeometry {
        self.inner.geometry()
    }

    fn read_blocks(&mut self, first_block: u64, destination: &mut [u8]) -> Result<(), BlockError> {
        self.session
            .authorize_read()
            .map_err(|_| BlockError::AccessDenied)?;
        self.inner.read_blocks(first_block, destination)
    }

    fn write_blocks(&mut self, first_block: u64, source: &[u8]) -> Result<(), BlockError> {
        self.session
            .authorize_write()
            .map_err(|_| BlockError::AccessDenied)?;
        self.inner.write_blocks(first_block, source)
    }

    fn flush(&mut self) -> Result<(), BlockError> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoenix_block::MemoryBlockDevice;

    const INSTALLATION_A: InstallationId = InstallationId::new([
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f,
    ]);
    const INSTALLATION_B: InstallationId = InstallationId::new([
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e,
        0x2f,
    ]);

    #[test]
    fn bound_media_only_unlocks_for_its_installation() {
        let mut session = MediaSession::new(
            MediaProtectionMode::BoundProtected,
            true,
            Some(INSTALLATION_A),
        )
        .unwrap();

        assert_eq!(
            session.authorize_read(),
            Err(MediaPolicyError::BoundUnlockRequired)
        );
        assert_eq!(
            session.unlock_bound(INSTALLATION_B),
            Err(MediaPolicyError::BoundInstallationMismatch)
        );
        assert_eq!(
            session.authorize_read(),
            Err(MediaPolicyError::BoundUnlockRequired)
        );

        session.unlock_bound(INSTALLATION_A).unwrap();
        assert_eq!(session.authorize_read(), Ok(()));
        assert_eq!(session.authorize_write(), Ok(()));

        session.lock();
        assert_eq!(
            session.authorize_read(),
            Err(MediaPolicyError::BoundUnlockRequired)
        );
    }

    #[test]
    fn portable_media_requires_explicit_portable_unlock() {
        let mut session =
            MediaSession::new(MediaProtectionMode::PortableProtected, true, None).unwrap();

        assert_eq!(
            session.unlock_bound(INSTALLATION_A),
            Err(MediaPolicyError::WrongMode)
        );
        assert_eq!(
            session.authorize_write(),
            Err(MediaPolicyError::PortableUnlockRequired)
        );

        session.unlock_portable().unwrap();
        assert_eq!(session.authorize_read(), Ok(()));
        assert_eq!(session.authorize_write(), Ok(()));
    }

    #[test]
    fn protected_open_media_consumes_one_operation_confirmation() {
        let mut session = MediaSession::new(MediaProtectionMode::Open, true, None).unwrap();

        assert_eq!(session.authorize_read(), Ok(()));
        assert_eq!(
            session.authorize_write(),
            Err(MediaPolicyError::OpenWriteConfirmationRequired)
        );

        session
            .confirm_open_write(OpenWriteScope::OneOperation)
            .unwrap();
        assert_eq!(session.authorize_write(), Ok(()));
        assert_eq!(
            session.authorize_write(),
            Err(MediaPolicyError::OpenWriteConfirmationRequired)
        );
    }

    #[test]
    fn open_session_confirmation_persists_until_lock_or_revoke() {
        let mut session = MediaSession::new(MediaProtectionMode::Open, true, None).unwrap();
        session.confirm_open_write(OpenWriteScope::Session).unwrap();

        assert_eq!(session.authorize_write(), Ok(()));
        assert_eq!(session.authorize_write(), Ok(()));

        session.revoke_open_write();
        assert_eq!(
            session.authorize_write(),
            Err(MediaPolicyError::OpenWriteConfirmationRequired)
        );

        session
            .confirm_open_write(OpenWriteScope::Session)
            .unwrap();
        session.lock();
        assert_eq!(
            session.authorize_write(),
            Err(MediaPolicyError::OpenWriteConfirmationRequired)
        );
    }

    #[test]
    fn open_media_without_system_protection_is_writable_without_confirmation() {
        let mut session = MediaSession::new(MediaProtectionMode::Open, false, None).unwrap();

        assert_eq!(session.authorize_read(), Ok(()));
        assert_eq!(session.authorize_write(), Ok(()));
    }

    #[test]
    fn session_rejects_inconsistent_binding_configuration() {
        assert_eq!(
            MediaSession::new(MediaProtectionMode::BoundProtected, true, None),
            Err(MediaPolicyError::MissingBoundInstallation)
        );
        assert_eq!(
            MediaSession::new(
                MediaProtectionMode::PortableProtected,
                true,
                Some(INSTALLATION_A),
            ),
            Err(MediaPolicyError::UnexpectedBoundInstallation)
        );
        assert_eq!(
            MediaSession::new(MediaProtectionMode::Open, true, Some(INSTALLATION_A)),
            Err(MediaPolicyError::UnexpectedBoundInstallation)
        );
    }

    #[test]
    fn block_device_enforces_open_write_confirmation_at_io_boundary() {
        let inner = MemoryBlockDevice::<8, 2>::new();
        let session = MediaSession::new(MediaProtectionMode::Open, true, None).unwrap();
        let mut device = ExternalMediaDevice::new(inner, session);

        let mut output = [0_u8; 8];
        device.read_blocks(0, &mut output).unwrap();
        assert_eq!(
            device.write_blocks(0, b"phoenix!"),
            Err(BlockError::AccessDenied)
        );

        device
            .session_mut()
            .confirm_open_write(OpenWriteScope::OneOperation)
            .unwrap();
        device.write_blocks(0, b"phoenix!").unwrap();
        assert_eq!(
            device.write_blocks(1, b"storage!"),
            Err(BlockError::AccessDenied)
        );

        output.fill(0);
        device.read_blocks(0, &mut output).unwrap();
        assert_eq!(&output, b"phoenix!");
    }

    #[test]
    fn locked_protected_device_blocks_reads_and_writes() {
        let inner = MemoryBlockDevice::<8, 2>::new();
        let session = MediaSession::new(
            MediaProtectionMode::BoundProtected,
            true,
            Some(INSTALLATION_A),
        )
        .unwrap();
        let mut device = ExternalMediaDevice::new(inner, session);

        let mut output = [0_u8; 8];
        assert_eq!(
            device.read_blocks(0, &mut output),
            Err(BlockError::AccessDenied)
        );
        assert_eq!(
            device.write_blocks(0, b"phoenix!"),
            Err(BlockError::AccessDenied)
        );

        device.session_mut().unlock_bound(INSTALLATION_A).unwrap();
        device.write_blocks(0, b"phoenix!").unwrap();
        device.read_blocks(0, &mut output).unwrap();
        assert_eq!(&output, b"phoenix!");
    }
}
