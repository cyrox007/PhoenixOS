#![allow(clippy::identity_op)]
//! Административные команды NVMe и кольца команд/завершений.
//!
//! Не владеет аппаратной памятью. Работа с очередями начинается только после
//! настройки DMA и включения контроллера в платформенном драйвере.

pub const ADMIN_QUEUE_DEPTH: u16 = 16;
pub const SUBMISSION_ENTRY_BYTES: usize = 64;
pub const COMPLETION_ENTRY_BYTES: usize = 16;
pub const IDENTIFY_BUFFER_BYTES: usize = 4096;
pub const IDENTIFY_OPCODE: u8 = 0x06;
pub const IDENTIFY_CONTROLLER: u8 = 1;
pub const IDENTIFY_NAMESPACE: u8 = 0;

pub const REG_CC: u64 = 0x14;
pub const REG_CSTS: u64 = 0x1c;
pub const REG_AQA: u64 = 0x24;
pub const REG_ASQ: u64 = 0x28;
pub const REG_ACQ: u64 = 0x30;
pub const REG_DOORBELL: u64 = 0x1000;

pub const CONTROLLER_ENABLED: u32 = 1;
pub const CONTROLLER_READY: u32 = 1;
pub const CONTROLLER_FATAL: u32 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminQueueError {
    InvalidQueueDepth,
    InvalidDmaAddress,
    UnexpectedSubmissionQueue,
    UnexpectedCommandId,
    CompletionStatus(u16),
    InvalidCompletion,
}

pub fn queue_attributes(depth: u16, max_entries: u32) -> Result<u32, AdminQueueError> {
    if depth < 2 || u32::from(depth) > max_entries || depth > 64 {
        return Err(AdminQueueError::InvalidQueueDepth);
    }
    let count = u32::from(depth - 1);
    Ok(count | (count << 16))
}

/// Формат очередей подачи: 2^6 = 64 байта; завершения: 2^4 = 16 байт.
/// Размер страницы MPS=0 соответствует 4096 байтам.
pub const fn enabled_configuration() -> u32 {
    CONTROLLER_ENABLED | (6 << 16) | (4 << 20)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentifyCommand {
    pub words: [u32; 16],
}

impl IdentifyCommand {
    pub fn build(
        command_id: u16,
        namespace: u32,
        controller: bool,
        data_address: u64,
    ) -> Result<Self, AdminQueueError> {
        if data_address == 0 || data_address % IDENTIFY_BUFFER_BYTES as u64 != 0 {
            return Err(AdminQueueError::InvalidDmaAddress);
        }
        let mut words = [0; 16];
        words[0] = (u32::from(command_id) << 16) | u32::from(IDENTIFY_OPCODE);
        words[1] = namespace;
        words[6] = data_address as u32;
        words[7] = (data_address >> 32) as u32;
        words[10] = u32::from(if controller {
            IDENTIFY_CONTROLLER
        } else {
            IDENTIFY_NAMESPACE
        });
        Ok(Self { words })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminCompletion {
    pub result: u32,
    pub submission_head: u16,
    pub submission_queue: u16,
    pub command_id: u16,
    pub status: u16,
    pub phase: bool,
}

impl AdminCompletion {
    pub fn decode(
        words: [u32; 4],
        expected_command_id: u16,
        expected_phase: bool,
    ) -> Result<Self, AdminQueueError> {
        let status_with_phase = (words[3] >> 16) as u16;
        let phase = status_with_phase & 1 != 0;
        if phase != expected_phase {
            return Err(AdminQueueError::InvalidCompletion);
        }
        let completion = Self {
            result: words[0],
            submission_head: words[2] as u16,
            submission_queue: (words[2] >> 16) as u16,
            command_id: words[3] as u16,
            status: status_with_phase >> 1,
            phase,
        };
        if completion.submission_queue != 0 {
            return Err(AdminQueueError::UnexpectedSubmissionQueue);
        }
        if completion.command_id != expected_command_id {
            return Err(AdminQueueError::UnexpectedCommandId);
        }
        if completion.status != 0 {
            return Err(AdminQueueError::CompletionStatus(completion.status));
        }
        Ok(completion)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminQueueCursor {
    depth: u16,
    submission_tail: u16,
    completion_head: u16,
    completion_phase: bool,
    next_command: u16,
}

impl AdminQueueCursor {
    pub fn new(depth: u16) -> Result<Self, AdminQueueError> {
        queue_attributes(depth, 64)?;
        Ok(Self {
            depth,
            submission_tail: 0,
            completion_head: 0,
            completion_phase: true,
            next_command: 1,
        })
    }

    pub const fn submission_tail(&self) -> u16 {
        self.submission_tail
    }

    pub const fn completion_head(&self) -> u16 {
        self.completion_head
    }

    pub const fn completion_phase(&self) -> bool {
        self.completion_phase
    }

    pub const fn command_id(&self) -> u16 {
        self.next_command
    }

    /// Используется только после успешного размещения очередной команды.
    pub fn submitted(&mut self) {
        self.submission_tail = (self.submission_tail + 1) % self.depth;
    }

    /// Используется после проверки завершения и чтения буфера ответа.
    pub fn completed(&mut self) {
        self.completion_head += 1;
        if self.completion_head == self.depth {
            self.completion_head = 0;
            self.completion_phase = !self.completion_phase;
        }
        self.next_command = self.next_command.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_sizes_and_configuration_are_spec_compliant() {
        assert_eq!(queue_attributes(16, 2048), Ok(0x000f_000f));
        assert_eq!(enabled_configuration(), 0x0046_0001);
        assert_eq!(queue_attributes(0, 2048), Err(AdminQueueError::InvalidQueueDepth));
        assert_eq!(queue_attributes(16, 8), Err(AdminQueueError::InvalidQueueDepth));
        assert_eq!(queue_attributes(65, 2048), Err(AdminQueueError::InvalidQueueDepth));
    }

    #[test]
    fn identify_command_has_correct_prp_and_command_id() {
        let command = IdentifyCommand::build(7, 0, true, 0x1234_0000_9000).unwrap();
        assert_eq!(command.words[0], (7 << 16) | 6);
        assert_eq!(command.words[1], 0);
        assert_eq!(command.words[6], 0x9000);
        assert_eq!(command.words[7], 0x1234);
        assert_eq!(command.words[10], 1);
        assert_eq!(command.words[11..], [0; 5]);
        assert_eq!(
            IdentifyCommand::build(1, 0, true, 0x1001),
            Err(AdminQueueError::InvalidDmaAddress)
        );
    }

    #[test]
    fn completion_checks_phase_status_queue_and_command() {
        let okay = [0, 0, 0x0000_0001, 0x0001_0001];
        let completion = AdminCompletion::decode(okay, 1, true).unwrap();
        assert_eq!(completion.submission_head, 1);
        assert_eq!(completion.status, 0);
        assert_eq!(
            AdminCompletion::decode(okay, 1, false),
            Err(AdminQueueError::InvalidCompletion)
        );
        assert_eq!(
            AdminCompletion::decode(okay, 2, true),
            Err(AdminQueueError::UnexpectedCommandId)
        );
        assert_eq!(
            AdminCompletion::decode([0, 0, 0x0001_0001, 0x0001_0001], 1, true),
            Err(AdminQueueError::UnexpectedSubmissionQueue)
        );
        assert_eq!(
            AdminCompletion::decode([0, 0, 1, 0x0003_0001], 1, true),
            Err(AdminQueueError::CompletionStatus(1))
        );
    }

    #[test]
    fn cursor_wraps_and_toggles_phase_after_last_completion() {
        let mut cursor = AdminQueueCursor::new(2).unwrap();
        assert_eq!(cursor.command_id(), 1);
        cursor.submitted();
        cursor.completed();
        assert_eq!(cursor.submission_tail(), 1);
        assert_eq!(cursor.completion_head(), 1);
        assert!(cursor.completion_phase());
        cursor.submitted();
        cursor.completed();
        assert_eq!(cursor.submission_tail(), 0);
        assert_eq!(cursor.completion_head(), 0);
        assert!(!cursor.completion_phase());
        assert_eq!(cursor.command_id(), 3);
    }
}
