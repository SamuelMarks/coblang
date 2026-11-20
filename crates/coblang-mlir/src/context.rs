//! MLIR 20 context initialization and `SourceSpan`-to-`Location` mapping.

use coblang_common::{Result, SourceSpan};
use coblang_diag::SourceMap;
use melior::Context;
use melior::dialect::DialectRegistry;
use melior::ir::Location;
use melior::utility::{register_all_dialects, register_all_llvm_translations, register_all_passes};

/// Thread-local/stack-owned MLIR 20 context configured with standard dialects, LLVM translations, and `cobol.*` support.
#[derive(Debug)]
pub struct CobolMlirContext {
    /// Underlying `melior` MLIR 20 context.
    context: Context,
}

impl Default for CobolMlirContext {
    /// Creates a default single-threaded [`CobolMlirContext`].
    fn default() -> Self {
        Self::new()
    }
}

impl CobolMlirContext {
    /// Creates and initializes a new single-threaded [`CobolMlirContext`] with all MLIR 20 dialects and passes registered.
    #[must_use]
    pub fn new() -> Self {
        register_all_passes();
        let registry = DialectRegistry::new();
        register_all_dialects(&registry);

        let context = Context::new();
        context.enable_multi_threading(false);
        context.append_dialect_registry(&registry);
        context.load_all_available_dialects();
        register_all_llvm_translations(&context);
        context.set_allow_unregistered_dialects(true);

        Self { context }
    }

    /// Borrows the underlying [`melior::Context`].
    #[must_use]
    pub const fn raw(&self) -> &Context {
        &self.context
    }

    /// Converts a compiler [`SourceSpan`] into a file/line/column MLIR [`Location`].
    ///
    /// # Arguments
    ///
    /// * `map` - Multi-file [`SourceMap`] owning `span.file_id`.
    /// * `span` - Byte-exact source span to resolve.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::InvalidDomainValue`] if `span` references an unknown file or out-of-bounds offset.
    pub fn span_to_location<'c>(
        &'c self,
        map: &SourceMap,
        span: SourceSpan,
    ) -> Result<Location<'c>> {
        let resolved = map.resolve_span(span)?;
        Ok(Location::new(
            &self.context,
            &resolved.file_path,
            resolved.line.get() as usize,
            resolved.column.get() as usize,
        ))
    }

    /// Returns an unknown MLIR [`Location`] for module-level or synthetic operations.
    #[must_use]
    pub fn unknown_location(&self) -> Location<'_> {
        Location::unknown(&self.context)
    }
}
