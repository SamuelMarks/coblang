//! Case-folding COBOL word interner preserving raw identifier spelling for diagnostics and C ABIs.

use std::collections::BTreeMap;

use coblang_common::{CoblangError, Result, SymbolId};

/// Stored canonical-uppercase and first-seen raw spelling for an interned [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolEntry {
    /// Canonical uppercase COBOL identifier or word.
    canonical: String,
    /// Original raw spelling at first interning site.
    original: String,
}

/// Arena interner deduplicating COBOL identifiers case-insensitively while retaining original casing.
#[derive(Debug, Clone)]
pub struct SymbolInterner {
    /// Lookup index from canonical uppercase text to assigned [`SymbolId`].
    by_canonical: BTreeMap<String, SymbolId>,
    /// Dense slot table indexed by [`SymbolId::as_u32`].
    entries: Vec<SymbolEntry>,
    /// Maximum distinct symbols permitted (`u32::MAX` by default).
    max_entries: u32,
}

impl Default for SymbolInterner {
    /// Creates an empty [`SymbolInterner`] with capacity limit `u32::MAX`.
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolInterner {
    /// Creates an empty [`SymbolInterner`] with capacity limit `u32::MAX`.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limit(u32::MAX)
    }

    /// Creates an empty [`SymbolInterner`] with an explicit symbol count ceiling.
    ///
    /// # Arguments
    ///
    /// * `max_entries` - Maximum distinct symbols allowed before [`Self::intern`] returns an error.
    #[must_use]
    pub const fn with_limit(max_entries: u32) -> Self {
        Self {
            by_canonical: BTreeMap::new(),
            entries: Vec::new(),
            max_entries,
        }
    }

    /// Interns `raw` case-insensitively (`to_ascii_uppercase`) and returns its [`SymbolId`].
    ///
    /// # Arguments
    ///
    /// * `raw` - Raw identifier or COBOL word spelling.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if the symbol table reaches `self.max_entries`.
    pub fn intern(&mut self, raw: &str) -> Result<SymbolId> {
        let canonical = raw.to_ascii_uppercase();
        if let Some(&existing) = self.by_canonical.get(&canonical) {
            return Ok(existing);
        }
        if self.entries.len() >= self.max_entries as usize {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "SymbolId",
                value: self.entries.len().to_string(),
                reason: format!(
                    "symbol interner capacity exceeded limit {}",
                    self.max_entries
                ),
            });
        }
        let raw_idx = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        let sym = SymbolId::new(raw_idx);
        self.by_canonical.insert(canonical.clone(), sym);
        self.entries.push(SymbolEntry {
            canonical,
            original: String::from(raw),
        });
        Ok(sym)
    }

    /// Looks up `raw` case-insensitively without inserting a new entry.
    ///
    /// # Arguments
    ///
    /// * `raw` - Candidate identifier or word to query.
    #[must_use]
    pub fn lookup(&self, raw: &str) -> Option<SymbolId> {
        self.by_canonical.get(&raw.to_ascii_uppercase()).copied()
    }

    /// Resolves the canonical uppercase representation of `sym`.
    ///
    /// # Arguments
    ///
    /// * `sym` - Interned symbol handle to resolve.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `sym` was not allocated by this interner.
    pub fn resolve_canonical(&self, sym: SymbolId) -> Result<&str> {
        let idx = sym.as_u32() as usize;
        self.entries
            .get(idx)
            .map(|entry| entry.canonical.as_str())
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "SymbolId",
                value: sym.as_u32().to_string(),
                reason: String::from("unknown SymbolId"),
            })
    }

    /// Resolves the original first-seen spelling of `sym`.
    ///
    /// # Arguments
    ///
    /// * `sym` - Interned symbol handle to resolve.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `sym` was not allocated by this interner.
    pub fn resolve_original(&self, sym: SymbolId) -> Result<&str> {
        let idx = sym.as_u32() as usize;
        self.entries
            .get(idx)
            .map(|entry| entry.original.as_str())
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "SymbolId",
                value: sym.as_u32().to_string(),
                reason: String::from("unknown SymbolId"),
            })
    }

    /// Returns the total number of distinct interned symbols.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when zero symbols have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
