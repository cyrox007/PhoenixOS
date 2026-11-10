//! Ограниченный перечень APIC-идентификаторов из ACPI MADT.
//! ACPI не доказывает, что процессоры уже запущены и их MSR прочитаны.
//! Типы 0 и 9 учитываются раздельно; неоднозначность блокирует допуск.

use crate::{AcpiError, SDT_HEADER_SIZE, SdtHeader, read_u32};

pub const MAX_TOPOLOGY_PROCESSORS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyError {
    Acpi(AcpiError),
    InvalidRecord,
    InvalidFlags,
    DuplicateApicId,
    TooManyProcessors,
    NoEnabledProcessors,
}

impl From<AcpiError> for TopologyError {
    fn from(value: AcpiError) -> Self {
        Self::Acpi(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MadtTopology {
    ids: [u32; MAX_TOPOLOGY_PROCESSORS],
    pub enabled_count: usize,
    pub online_capable_count: usize,
    pub disabled_count: usize,
}

impl MadtTopology {
    /// Только объявленные прошивкой включённые процессоры.
    /// Подтверждать исполнение каждого идентификатора обязан сборщик MSR.
    pub fn enabled_apic_ids(&self) -> &[u32] {
        &self.ids[..self.enabled_count]
    }
}

/// Не включает процессоры и не обращается к регистрам оборудования.
pub fn parse_madt_topology(bytes: &[u8]) -> Result<MadtTopology, TopologyError> {
    let header = SdtHeader::validate_table(bytes, *b"APIC")?;
    let length = header.length as usize;
    if length < SDT_HEADER_SIZE + 8 {
        return Err(TopologyError::InvalidRecord);
    }

    let mut topology = MadtTopology {
        ids: [0; MAX_TOPOLOGY_PROCESSORS],
        enabled_count: 0,
        online_capable_count: 0,
        disabled_count: 0,
    };
    let mut offset = SDT_HEADER_SIZE + 8;
    while offset < length {
        let record_header = bytes
            .get(offset..offset.saturating_add(2))
            .ok_or(TopologyError::InvalidRecord)?;
        let kind = record_header[0];
        let record_length = usize::from(record_header[1]);
        if record_length < 2 || record_length > length - offset {
            return Err(TopologyError::InvalidRecord);
        }

        // LAPIC (тип 0) и x2APIC (тип 9) имеют разные поля идентификатора.
        let apic_and_flags = match kind {
            0 if record_length >= 8 => {
                let id = u32::from(bytes[offset + 3]);
                Some((id, read_u32(bytes, offset + 4)?))
            }
            9 if record_length >= 16 => {
                Some((read_u32(bytes, offset + 4)?, read_u32(bytes, offset + 8)?))
            }
            0 | 9 => return Err(TopologyError::InvalidRecord),
            _ => None,
        };

        if let Some((apic_id, flags)) = apic_and_flags {
            // ACPI MADT: бит 0 — включён, бит 1 — возможен последующий запуск.
            if flags & !0b11 != 0 {
                return Err(TopologyError::InvalidFlags);
            }
            if flags & 1 == 0 {
                if flags & 2 != 0 {
                    topology.online_capable_count += 1;
                } else {
                    topology.disabled_count += 1;
                }
            } else {
                if topology.enabled_count == MAX_TOPOLOGY_PROCESSORS {
                    return Err(TopologyError::TooManyProcessors);
                }
                if topology.enabled_apic_ids().contains(&apic_id) {
                    return Err(TopologyError::DuplicateApicId);
                }
                topology.ids[topology.enabled_count] = apic_id;
                topology.enabled_count += 1;
            }
        }

        offset += record_length;
    }

    if topology.enabled_count == 0 {
        return Err(TopologyError::NoEnabledProcessors);
    }
    Ok(topology)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(records: &[u8]) -> std::vec::Vec<u8> {
        let mut result = std::vec![0u8; SDT_HEADER_SIZE + 8];
        result[..4].copy_from_slice(b"APIC");
        result.extend_from_slice(records);
        let len = result.len() as u32;
        result[4..8].copy_from_slice(&len.to_le_bytes());
        result[9] = 0;
        result[9] = 0u8.wrapping_sub(crate::checksum(&result));
        result
    }

    fn lapic(id: u8, flags: u32) -> [u8; 8] {
        let mut item = [0u8; 8];
        item[1] = 8;
        item[3] = id;
        item[4..8].copy_from_slice(&flags.to_le_bytes());
        item
    }

    fn x2apic(id: u32, flags: u32) -> [u8; 16] {
        let mut item = [0u8; 16];
        item[0] = 9;
        item[1] = 16;
        item[4..8].copy_from_slice(&id.to_le_bytes());
        item[8..12].copy_from_slice(&flags.to_le_bytes());
        item
    }

    #[test]
    fn reads_exact_enabled_ids_including_extended_x2apic() {
        let mut records = std::vec::Vec::new();
        records.extend_from_slice(&lapic(5, 1));
        records.extend_from_slice(&x2apic(0x1234, 1));
        records.extend_from_slice(&lapic(8, 2));
        records.extend_from_slice(&lapic(9, 0));
        let result = parse_madt_topology(&table(&records)).unwrap();
        assert_eq!(result.enabled_apic_ids(), &[5, 0x1234]);
        assert_eq!(result.online_capable_count, 1);
        assert_eq!(result.disabled_count, 1);
    }

    #[test]
    fn rejects_duplicate_ids_across_lapic_and_x2apic() {
        let mut records = std::vec::Vec::new();
        records.extend_from_slice(&lapic(3, 1));
        records.extend_from_slice(&x2apic(3, 1));
        assert_eq!(
            parse_madt_topology(&table(&records)),
            Err(TopologyError::DuplicateApicId)
        );
    }

    #[test]
    fn rejects_untrusted_flags_and_missing_enabled_cores() {
        assert_eq!(
            parse_madt_topology(&table(&lapic(2, 4))),
            Err(TopologyError::InvalidFlags)
        );
        assert_eq!(
            parse_madt_topology(&table(&lapic(2, 2))),
            Err(TopologyError::NoEnabledProcessors)
        );
    }

    #[test]
    fn rejects_damaged_or_truncated_entries() {
        let mut broken = lapic(2, 1);
        broken[1] = 7;
        assert_eq!(
            parse_madt_topology(&table(&broken)),
            Err(TopologyError::InvalidRecord)
        );
        let mut data = table(&lapic(2, 1));
        data[12] ^= 1;
        assert_eq!(
            parse_madt_topology(&data),
            Err(TopologyError::Acpi(AcpiError::InvalidChecksum))
        );
    }

    #[test]
    fn rejects_more_than_registry_capacity() {
        let mut records = std::vec::Vec::new();
        for id in 0..=MAX_TOPOLOGY_PROCESSORS {
            records.extend_from_slice(&x2apic(id as u32, 1));
        }
        assert_eq!(
            parse_madt_topology(&table(&records)),
            Err(TopologyError::TooManyProcessors)
        );
    }
}
