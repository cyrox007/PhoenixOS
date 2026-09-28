#![no_std]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProtectionMode {
    BoundProtected,
    PortableProtected,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUnlock {
    None,
    Bound,
    Portable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaAccessContext {
    pub protection_active: bool,
    pub unlock: MediaUnlock,
    pub open_write_confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPolicyError {
    BoundUnlockRequired,
    PortableUnlockRequired,
    OpenWriteConfirmationRequired,
}

/// Проверяет возможность чтения носителя в выбранном режиме.
pub const fn authorize_read(
    mode: MediaProtectionMode,
    context: MediaAccessContext,
) -> Result<(), MediaPolicyError> {
    match mode {
        MediaProtectionMode::BoundProtected if !matches!(context.unlock, MediaUnlock::Bound) => {
            Err(MediaPolicyError::BoundUnlockRequired)
        }
        MediaProtectionMode::PortableProtected
            if !matches!(context.unlock, MediaUnlock::Portable) =>
        {
            Err(MediaPolicyError::PortableUnlockRequired)
        }
        _ => Ok(()),
    }
}

/// Проверяет возможность записи.
///
/// Открытая запись при активной системной защите разрешается только после
/// отдельного явного подтверждения пользователя.
pub const fn authorize_write(
    mode: MediaProtectionMode,
    context: MediaAccessContext,
) -> Result<(), MediaPolicyError> {
    match authorize_read(mode, context) {
        Ok(()) => {}
        Err(error) => return Err(error),
    }

    if matches!(mode, MediaProtectionMode::Open)
        && context.protection_active
        && !context.open_write_confirmed
    {
        return Err(MediaPolicyError::OpenWriteConfirmationRequired);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTECTED: MediaAccessContext = MediaAccessContext {
        protection_active: true,
        unlock: MediaUnlock::None,
        open_write_confirmed: false,
    };

    #[test]
    fn bound_mode_requires_bound_unlock() {
        assert_eq!(
            authorize_read(MediaProtectionMode::BoundProtected, PROTECTED),
            Err(MediaPolicyError::BoundUnlockRequired)
        );

        let context = MediaAccessContext {
            unlock: MediaUnlock::Bound,
            ..PROTECTED
        };
        assert_eq!(
            authorize_read(MediaProtectionMode::BoundProtected, context),
            Ok(())
        );
    }

    #[test]
    fn portable_mode_rejects_bound_unlock() {
        let bound = MediaAccessContext {
            unlock: MediaUnlock::Bound,
            ..PROTECTED
        };
        assert_eq!(
            authorize_read(MediaProtectionMode::PortableProtected, bound),
            Err(MediaPolicyError::PortableUnlockRequired)
        );

        let portable = MediaAccessContext {
            unlock: MediaUnlock::Portable,
            ..PROTECTED
        };
        assert_eq!(
            authorize_write(MediaProtectionMode::PortableProtected, portable),
            Ok(())
        );
    }

    #[test]
    fn open_write_needs_confirmation_only_when_protection_is_active() {
        assert_eq!(
            authorize_write(MediaProtectionMode::Open, PROTECTED),
            Err(MediaPolicyError::OpenWriteConfirmationRequired)
        );

        let confirmed = MediaAccessContext {
            open_write_confirmed: true,
            ..PROTECTED
        };
        assert_eq!(
            authorize_write(MediaProtectionMode::Open, confirmed),
            Ok(())
        );

        let protection_disabled = MediaAccessContext {
            protection_active: false,
            ..PROTECTED
        };
        assert_eq!(
            authorize_write(MediaProtectionMode::Open, protection_disabled),
            Ok(())
        );
    }

    #[test]
    fn open_read_never_requires_write_confirmation() {
        assert_eq!(authorize_read(MediaProtectionMode::Open, PROTECTED), Ok(()));
    }
}
