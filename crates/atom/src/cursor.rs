//! In-memory cursor for the indexer (no disk persistence).
//!
//! Process restarts start from start_block again (duplicates possible).

/// Cursor: last processed block. In-memory only.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    pub last_processed_block: u64,
}

impl Cursor {
    pub fn new(last_processed_block: u64) -> Self {
        Self {
            last_processed_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_new() {
        let c = Cursor::new(123);
        assert_eq!(c.last_processed_block, 123);
    }

    #[test]
    fn cursor_default() {
        let c = Cursor::default();
        assert_eq!(c.last_processed_block, 0);
    }
}
