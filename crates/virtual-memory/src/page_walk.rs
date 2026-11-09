//! Пассивный обход ВСЕХ листовых записей текущей CR3 (4 уровня).
//!
//! Не сканирует другие адресные пространства и ядра процессора.
//! Нет записи в таблицы, переключения CR3 или создания отображений.

const PRESENT: u64 = 1;
const HUGE: u64 = 1 << 7;
const PAGE_SIZE: u64 = 4096;
pub const MAX_PAGE_TABLE_ENTRIES: u64 = 1_000_000;
pub const MAX_LEAF_MAPPINGS: u64 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageWalkError {
    UnsupportedAddressWidth,
    InvalidRoot,
    InvalidEntry,
    MissingTable,
    BudgetExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedLeaf {
    pub virtual_start: u64,
    pub physical_start: u64,
    pub size: u64,
    pub pat_index: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageWalkReport {
    pub table_entries: u64,
    pub leaf_mappings: u64,
    pub four_kib_pages: u64,
    pub two_mib_pages: u64,
    pub one_gib_pages: u64,
}

#[derive(Clone, Copy)]
struct Cursor {
    physical: u64,
    virtual_prefix: u64,
    next_index: usize,
    level: u8,
}

impl Cursor {
    const EMPTY: Self = Self {
        physical: 0,
        virtual_prefix: 0,
        next_index: 0,
        level: 0,
    };
}

/// Источник таблиц возвращает 64-битную запись по физическому адресу
/// таблицы и индексу. Прочитать можно только безопасную RAM, без MMIO.
pub fn walk_current_cr3<F, G>(
    root: u64,
    physical_bits: u8,
    mut read_entry: F,
    mut observe: G,
) -> Result<PageWalkReport, PageWalkError>
where
    F: FnMut(u64, usize) -> Option<u64>,
    G: FnMut(MappedLeaf),
{
    if !(36..=52).contains(&physical_bits) {
        return Err(PageWalkError::UnsupportedAddressWidth);
    }
    let physical_limit = 1u64 << physical_bits;
    if root & (PAGE_SIZE - 1) != 0 || root >= physical_limit {
        return Err(PageWalkError::InvalidRoot);
    }

    let mask = (physical_limit - 1) & !(PAGE_SIZE - 1);
    let mut cursors = [Cursor::EMPTY; 4];
    cursors[0] = Cursor {
        physical: root,
        virtual_prefix: 0,
        next_index: 0,
        level: 4,
    };
    let mut depth = 0usize;
    let mut report = PageWalkReport::default();

    loop {
        if cursors[depth].next_index == 512 {
            if depth == 0 {
                break;
            }
            depth -= 1;
            continue;
        }
        if report.table_entries == MAX_PAGE_TABLE_ENTRIES {
            return Err(PageWalkError::BudgetExceeded);
        }
        let cursor = &mut cursors[depth];
        let index = cursor.next_index;
        cursor.next_index += 1;
        report.table_entries += 1;

        let entry = read_entry(cursor.physical, index).ok_or(PageWalkError::MissingTable)?;
        if entry & PRESENT == 0 {
            continue;
        }
        let level = cursor.level;
        let shift = 12 + (level as u64 - 1) * 9;
        let virtual_prefix = cursor.virtual_prefix | ((index as u64) << shift);
        let huge = entry & HUGE != 0;

        if level == 4 && huge {
            return Err(PageWalkError::InvalidEntry);
        }
        if level > 1 && !huge {
            let child = entry & mask;
            if child >= physical_limit || child & (PAGE_SIZE - 1) != 0 {
                return Err(PageWalkError::InvalidEntry);
            }
            depth += 1;
            cursors[depth] = Cursor {
                physical: child,
                virtual_prefix,
                next_index: 0,
                level: level - 1,
            };
            continue;
        }

        if report.leaf_mappings == MAX_LEAF_MAPPINGS {
            return Err(PageWalkError::BudgetExceeded);
        }
        let size = 1u64 << shift;
        let physical = (entry & mask) & !(size - 1);
        if physical.checked_add(size).is_none_or(|end| end > physical_limit) {
            return Err(PageWalkError::InvalidEntry);
        }

        let pat_bit = if level == 1 { 7 } else { 12 };
        let pat_index = ((entry >> 3) & 1) as u8
            | (((entry >> 4) & 1) as u8) << 1
            | (((entry >> pat_bit) & 1) as u8) << 2;
        report.leaf_mappings += 1;
        match level {
            1 => report.four_kib_pages += 1,
            2 => report.two_mib_pages += 1,
            3 => report.one_gib_pages += 1,
            _ => return Err(PageWalkError::InvalidEntry),
        }
        observe(MappedLeaf {
            virtual_start: if virtual_prefix & (1u64 << 47) != 0 {
                virtual_prefix | (!0u64 << 48)
            } else {
                virtual_prefix
            },
            physical_start: physical,
            size,
            pat_index,
        });
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn four_kib_leaf_and_high_half_are_found() {
        let root = 0x1000;
        let report = walk_current_cr3(
            root,
            48,
            |table, index| match (table, index) {
                (0x1000, 511) => Some(0x2000 | 3),
                (0x2000, 0) => Some(0x3000 | 3),
                (0x3000, 0) => Some(0x4000 | 3),
                (0x4000, 1) => Some(0x9000 | 3 | (1 << 7)),
                _ => Some(0),
            },
            |leaf| {
                assert_eq!(leaf.virtual_start, 0xffff_ff80_0000_1000);
                assert_eq!(leaf.physical_start, 0x9000);
                assert_eq!(leaf.size, 4096);
                assert_eq!(leaf.pat_index, 4);
            },
        )
        .unwrap();
        assert_eq!(report.leaf_mappings, 1);
        assert_eq!(report.four_kib_pages, 1);
    }

    #[test]
    fn large_page_pat_comes_from_bit_twelve() {
        let mut observed = 0;
        let report = walk_current_cr3(
            0x1000,
            48,
            |table, index| match (table, index) {
                (0x1000, 0) => Some(0x2000 | 3),
                (0x2000, 1) => Some(0x4000_0000 | 3 | HUGE | (1 << 12)),
                (0x2000, 0) => Some(0x3000 | 3),
                (0x3000, 1) => Some(0x20_0000 | 3 | HUGE | (1 << 3)),
                _ => Some(0),
            },
            |leaf| {
                observed += 1;
                if leaf.size == 1 << 30 {
                    assert_eq!(leaf.pat_index, 4);
                }
                if leaf.size == 1 << 21 {
                    assert_eq!(leaf.pat_index, 1);
                }
            },
        )
        .unwrap();
        assert_eq!(observed, 2);
        assert_eq!(report.one_gib_pages, 1);
        assert_eq!(report.two_mib_pages, 1);
    }

    #[test]
    fn unexpected_or_unreadable_entries_fail_closed() {
        assert_eq!(
            walk_current_cr3(0x1001, 48, |_, _| Some(0), |_| {}),
            Err(PageWalkError::InvalidRoot)
        );
        assert_eq!(
            walk_current_cr3(0x1000, 48, |_, _| None, |_| {}),
            Err(PageWalkError::MissingTable)
        );
        assert_eq!(
            walk_current_cr3(0x1000, 48, |_, _| Some(0x81), |_| {}),
            Err(PageWalkError::InvalidEntry)
        );
        assert_eq!(
            walk_current_cr3(0x1000, 35, |_, _| Some(0), |_| {}),
            Err(PageWalkError::UnsupportedAddressWidth)
        );
    }
}
