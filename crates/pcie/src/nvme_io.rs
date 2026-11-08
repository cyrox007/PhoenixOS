//! Команды NVMe для создания рабочих очередей и обмена блоками.
//! Все команды записываются в 64-байтовые элементы очереди.
pub const CREATE_IO_CQ: u8 = 0x05;
pub const CREATE_IO_SQ: u8 = 0x01;
pub const READ: u8 = 0x02;
pub const WRITE: u8 = 0x01;
pub const IO_QUEUE_ID: u16 = 1;
pub const IO_QUEUE_DEPTH: u16 = 16;
pub const LOGICAL_BLOCK_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    InvalidQueue,
    InvalidAddress,
    InvalidBlockRange,
    InvalidCompletion,
    CompletionStatus(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoCommand {
    pub words: [u32; 16],
}

impl IoCommand {
    pub fn create_completion(id: u16, address: u64) -> Result<Self, IoError> {
        let mut command = Self::queue_command(CREATE_IO_CQ, id, address)?;
        command.words[10] = (u32::from(IO_QUEUE_DEPTH - 1) << 16) | u32::from(IO_QUEUE_ID);
        command.words[11] = 1;
        Ok(command)
    }

    pub fn create_submission(id: u16, address: u64) -> Result<Self, IoError> {
        let mut command = Self::queue_command(CREATE_IO_SQ, id, address)?;
        command.words[10] = (u32::from(IO_QUEUE_DEPTH - 1) << 16) | u32::from(IO_QUEUE_ID);
        command.words[11] = (u32::from(IO_QUEUE_ID) << 16) | 1;
        Ok(command)
    }

    fn queue_command(opcode: u8, id: u16, address: u64) -> Result<Self, IoError> {
        if address == 0 || address % 4096 != 0 {
            return Err(IoError::InvalidAddress);
        }
        let mut words = [0; 16];
        words[0] = (u32::from(id) << 16) | u32::from(opcode);
        words[6] = address as u32;
        words[7] = (address >> 32) as u32;
        Ok(Self { words })
    }

    pub fn transfer(
        id: u16,
        write: bool,
        namespace: u32,
        lba: u64,
        count: u16,
        total_blocks: u64,
        address: u64,
    ) -> Result<Self, IoError> {
        if namespace == 0
            || count == 0
            || count > 8
            || lba
                .checked_add(u64::from(count))
                .is_none_or(|end| end > total_blocks)
        {
            return Err(IoError::InvalidBlockRange);
        }
        if address == 0 || address % 4096 != 0 {
            return Err(IoError::InvalidAddress);
        }
        let mut words = [0; 16];
        words[0] = (u32::from(id) << 16) | u32::from(if write { WRITE } else { READ });
        words[1] = namespace;
        words[6] = address as u32;
        words[7] = (address >> 32) as u32;
        words[10] = lba as u32;
        words[11] = (lba >> 32) as u32;
        words[12] = u32::from(count - 1);
        Ok(Self { words })
    }
}

/// Проверка завершения очереди 1, фазового бита, идентификатора и кода состояния.
pub fn check_completion(words: [u32; 4], id: u16, phase: bool) -> Result<u16, IoError> {
    let status = (words[3] >> 16) as u16;
    if (status & 1 != 0) != phase || words[3] as u16 != id || (words[2] >> 16) as u16 != IO_QUEUE_ID
    {
        return Err(IoError::InvalidCompletion);
    }
    if status >> 1 != 0 {
        return Err(IoError::CompletionStatus(status >> 1));
    }
    Ok(words[2] as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_queues_with_valid_parameters() {
        let cq = IoCommand::create_completion(3, 0x1000).unwrap();
        let sq = IoCommand::create_submission(4, 0x2000).unwrap();
        assert_eq!(cq.words[0], (3 << 16) | 5);
        assert_eq!(sq.words[0], (4 << 16) | 1);
        assert_eq!(cq.words[10], 0x000f_0001);
        assert_eq!(sq.words[11], 0x0001_0001);
        assert_eq!(
            IoCommand::create_completion(1, 7),
            Err(IoError::InvalidAddress)
        );
    }

    #[test]
    fn validates_block_bounds_and_command_encoding() {
        let command = IoCommand::transfer(7, true, 1, 32767, 1, 32768, 0x3000).unwrap();
        assert_eq!(command.words[0], (7 << 16) | 1);
        assert_eq!(command.words[10], 32767);
        assert_eq!(command.words[12], 0);
        assert_eq!(
            IoCommand::transfer(1, false, 1, 32768, 1, 32768, 0x3000),
            Err(IoError::InvalidBlockRange)
        );
        assert_eq!(
            IoCommand::transfer(1, false, 1, u64::MAX, 2, u64::MAX, 0x3000),
            Err(IoError::InvalidBlockRange)
        );
        assert_eq!(
            IoCommand::transfer(1, false, 1, 0, 0, 32768, 0x3000),
            Err(IoError::InvalidBlockRange)
        );
    }

    #[test]
    fn validates_completion_identity_phase_and_status() {
        assert_eq!(
            check_completion([0, 0, 0x0001_0001, 0x0001_0007], 7, true),
            Ok(1)
        );
        assert_eq!(
            check_completion([0, 0, 0x0001_0001, 0x0001_0007], 7, false),
            Err(IoError::InvalidCompletion)
        );
        assert_eq!(
            check_completion([0, 0, 0x0000_0001, 0x0001_0007], 7, true),
            Err(IoError::InvalidCompletion)
        );
        assert_eq!(
            check_completion([0, 0, 0x0001_0001, 0x0003_0007], 7, true),
            Err(IoError::CompletionStatus(1))
        );
    }
}
