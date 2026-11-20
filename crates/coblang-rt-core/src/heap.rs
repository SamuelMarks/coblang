//! Pure-Rust heap allocation ledger for `ALLOCATE`, `FREE`, `cob_malloc`, and `cob_free`.

use std::collections::BTreeMap;

/// Tracks active `ALLOCATE` / `cob_malloc` heap blocks and `IS EXTERNAL` shared storage segments
/// for safe `FREE` and `cob_tidy` cleanup.
#[derive(Debug, Default)]
pub struct HeapTracker {
    /// Active `ALLOCATE` / `cob_malloc` heap allocations keyed by non-zero base address.
    blocks: BTreeMap<usize, usize>,
    /// Run-unit `IS EXTERNAL` shared storage segments keyed by canonical segment name bytes `(addr, size)`.
    external_segments: BTreeMap<Vec<u8>, (usize, usize)>,
}

impl HeapTracker {
    /// Creates an empty [`HeapTracker`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            external_segments: BTreeMap::new(),
        }
    }

    /// Records a newly allocated heap block of `size` bytes at `addr`.
    ///
    /// # Arguments
    ///
    /// * `addr` - Non-zero pointer address cast to `usize`.
    /// * `size` - Allocated byte count (`>= 1`).
    pub fn record(&mut self, addr: usize, size: usize) {
        if addr != 0 {
            self.blocks.insert(addr, size);
        }
    }

    /// Looks up an existing `IS EXTERNAL` segment base address by `name`.
    ///
    /// # Arguments
    ///
    /// * `name` - Canonical or explicit `AS` segment name bytes.
    #[must_use]
    pub fn lookup_external(&self, name: &[u8]) -> Option<usize> {
        self.external_segments.get(name).map(|&(addr, _)| addr)
    }

    /// Registers a newly allocated `IS EXTERNAL` shared storage segment `(addr, size)` under `name`.
    ///
    /// # Arguments
    ///
    /// * `name` - Canonical or explicit `AS` segment name bytes.
    /// * `addr` - Non-zero base address cast to `usize`.
    /// * `size` - Allocated byte count (`>= 1`).
    pub fn record_external(&mut self, name: Vec<u8>, addr: usize, size: usize) {
        if addr != 0 {
            self.external_segments.insert(name, (addr, size));
        }
    }

    /// Removes and returns the byte size of `addr` if it is an active `ALLOCATE` block.
    ///
    /// # Arguments
    ///
    /// * `addr` - Pointer address cast to `usize`.
    pub fn take(&mut self, addr: usize) -> Option<usize> {
        self.blocks.remove(&addr)
    }

    /// Drains all remaining active allocations and `EXTERNAL` segments `(addr, size)` on `cob_tidy` teardown.
    pub fn drain_all(&mut self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = self.blocks.iter().map(|(&a, &s)| (a, s)).collect();
        self.blocks.clear();
        out.extend(self.external_segments.values().copied());
        self.external_segments.clear();
        out
    }

    /// Returns the number of currently live `ALLOCATE` heap blocks.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.blocks.len()
    }
}
