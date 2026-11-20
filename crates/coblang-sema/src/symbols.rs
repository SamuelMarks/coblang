//! Lexical scope tree, `GLOBAL`/`IS COMMON` visibility, special registers, and qualified path resolution.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    ByteLength, ByteOffset, DataItemId, DigitCount, ExpansionId, FileConnectorId, FileId,
    LevelNumber, ProcedureId, Result, ScaleFactor, ScopeId, SourceSpan, SymbolId,
};
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink, SymbolInterner};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, DynamicOdoPresence, HirCondition88, HirConstValue,
    HirDataItem, HirFigurative, HirStorageSection, HirTruthValue, HirType, JustifiedSpec,
    NumericSignSpec, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::GlobalVisibility;

/// Resolved target of a Data Division or `SPECIAL-NAMES` identifier lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSymbol {
    /// Storage data item or `INDEXED BY` index item (`DataItemId`).
    DataItem(DataItemId),
    /// Level-`88` condition-name index into `conditions_88`.
    Condition88(usize),
    /// Compile-time level-`78` or `01 CONSTANT` value.
    Constant(HirConstValue),
    /// `SPECIAL-NAMES` switch-status condition (`ON STATUS` / `OFF STATUS`).
    SwitchCondition {
        /// Switch slot (`0..=36`).
        switch_index: u8,
        /// Expected switch state (`True` for `ON`, `False` for `OFF`).
        expected: HirTruthValue,
    },
    /// File connector (`FD`/`SD`/`SELECT`).
    File(FileConnectorId),
}

/// Candidate registration inside a [`ScopeSymbols`] table.
#[derive(Debug, Clone)]
pub struct SymbolCandidate {
    /// Resolved entity kind.
    pub target: ResolvedSymbol,
    /// Ordered ancestor qualifier chain (`[parent_group, ..., file_name]`, uppercase symbols).
    pub qualifiers: Vec<SymbolId>,
    /// Visibility to contained nested programs (`Local` vs `Global`).
    pub visibility: GlobalVisibility,
}

/// Registered paragraph or section entry in `PROCEDURE DIVISION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcedureEntry {
    /// Assigned [`ProcedureId`].
    pub id: ProcedureId,
    /// Paragraph or section name symbol.
    pub name: SymbolId,
    /// Optional enclosing `SECTION` name symbol (`None` for section headers or unsectioned paragraphs).
    pub section: Option<SymbolId>,
}

/// Symbol bindings owned by a single program/function/method lexical scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeSymbols {
    /// Scope identifier.
    pub scope_id: Option<ScopeId>,
    /// Optional enclosing parent scope (`Some` for nested programs).
    pub parent_scope: Option<ScopeId>,
    /// Data, condition-88, constant, index, and file candidates keyed by leaf `SymbolId`.
    pub by_name: BTreeMap<SymbolId, Vec<SymbolCandidate>>,
    /// User-defined `SPECIAL-NAMES` `CLASS` names.
    pub custom_classes: BTreeSet<SymbolId>,
    /// Declared `ALPHABET` names.
    pub alphabets: BTreeSet<SymbolId>,
    /// Declared device mnemonics.
    pub device_mnemonics: BTreeSet<SymbolId>,
    /// Contained child program names `(name, ScopeId, is_common)`.
    pub child_programs: Vec<(SymbolId, ScopeId, bool)>,
    /// Procedure Division paragraphs and sections in declaration order.
    pub procedures: Vec<ProcedureEntry>,
}

/// Multi-scope symbol table for a compilation group.
#[derive(Debug, Default)]
pub struct SymbolTable {
    /// Arena of [`ScopeSymbols`] indexed by `ScopeId::as_u32()`.
    scopes: Vec<ScopeSymbols>,
}

impl SymbolTable {
    /// Allocates a new [`ScopeId`] with optional `parent` scope.
    ///
    /// # Arguments
    ///
    /// * `parent` - Enclosing program scope for nested subprograms.
    #[must_use]
    pub fn create_scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        let raw = u32::try_from(self.scopes.len()).unwrap_or(u32::MAX);
        let id = ScopeId::new(raw);
        self.scopes.push(ScopeSymbols {
            scope_id: Some(id),
            parent_scope: parent,
            ..ScopeSymbols::default()
        });
        id
    }

    /// Returns an immutable reference to `scope`'s [`ScopeSymbols`].
    ///
    /// # Arguments
    ///
    /// * `scope` - Target scope handle.
    #[must_use]
    pub fn scope(&self, scope: ScopeId) -> Option<&ScopeSymbols> {
        self.scopes.get(scope.as_u32() as usize)
    }

    /// Returns a mutable reference to `scope`'s [`ScopeSymbols`].
    ///
    /// # Arguments
    ///
    /// * `scope` - Target scope handle.
    #[must_use]
    pub fn scope_mut(&mut self, scope: ScopeId) -> Option<&mut ScopeSymbols> {
        self.scopes.get_mut(scope.as_u32() as usize)
    }

    /// Registers a named data, condition, constant, or file symbol in `scope`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Owning lexical scope.
    /// * `name` - Leaf identifier symbol.
    /// * `candidate` - Candidate entry with qualifiers and `GLOBAL` visibility.
    pub fn insert_symbol(&mut self, scope: ScopeId, name: SymbolId, candidate: SymbolCandidate) {
        if let Some(sc) = self.scope_mut(scope) {
            sc.by_name.entry(name).or_default().push(candidate);
        }
    }

    /// Populates standard ISO / `GnuCOBOL` / `gcobol` implicit special registers into `scope` and `items`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Program scope ID.
    /// * `items` - Program's `HirDataItem` arena.
    /// * `interner` - Shared [`SymbolInterner`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if symbol interning fails.
    pub fn inject_special_registers(
        &mut self,
        scope: ScopeId,
        items: &mut Vec<HirDataItem>,
        interner: &mut SymbolInterner,
    ) -> Result<()> {
        let synth_span = SourceSpan::new(
            FileId::new(0),
            ByteOffset::ZERO,
            ByteLength::ZERO,
            ExpansionId::ROOT,
        );
        let s9_9 = HirType::BinaryInt {
            byte_width: 4,
            digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        };
        let s9_9_pic = HirType::BinaryInt {
            byte_width: 4,
            digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::TruncateToPic,
        };
        let int_regs = [
            "RETURN-CODE",
            "TALLY",
            "SORT-RETURN",
            "NUMBER-OF-CALL-PARAMETERS",
            "LINAGE-COUNTER",
            "PAGE-COUNTER",
            "LINE-COUNTER",
            "PRINT-SWITCH",
        ];
        for reg in int_regs {
            self.register_special_item(scope, reg, s9_9.clone(), 4, synth_span, items, interner)?;
        }
        for reg in ["XML-CODE", "XML-INFORMATION", "JSON-CODE", "JSON-STATUS"] {
            self.register_special_item(
                scope,
                reg,
                s9_9_pic.clone(),
                4,
                synth_span,
                items,
                interner,
            )?;
        }

        let crt_ty = HirType::ZonedDecimal {
            digits: DigitCount::try_new(4).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        };
        let crt_sym = interner.intern("COB-CRT-STATUS")?;
        let crt_id = DataItemId::new(u32::try_from(items.len()).unwrap_or(0));
        items.push(HirDataItem {
            id: crt_id,
            scope,
            name: Some(crt_sym),
            level: LevelNumber::Independent77,
            parent: None,
            children: Vec::new(),
            section: HirStorageSection::SpecialRegister,
            ty: crt_ty,
            byte_offset: ByteOffset::ZERO,
            total_byte_size: 4,
            dimensions: Vec::new(),
            redefines: None,
            initial_value: Some(HirConstValue::Figurative(HirFigurative::Zero)),
            span: synth_span,
        });
        self.insert_symbol(
            scope,
            crt_sym,
            SymbolCandidate {
                target: ResolvedSymbol::DataItem(crt_id),
                qualifiers: Vec::new(),
                visibility: GlobalVisibility::Local,
            },
        );

        let str_regs = [
            ("WHEN-COMPILED", 16_u32),
            ("XML-EVENT", 30_u32),
            ("XML-TEXT", 256_u32),
            ("XML-NTEXT", 256_u32),
            ("XML-NAMESPACE", 256_u32),
            ("XML-NNAMESPACE", 256_u32),
            ("XML-NAMESPACE-PREFIX", 256_u32),
            ("XML-NNAMESPACE-PREFIX", 256_u32),
        ];
        for (reg, len) in str_regs {
            let ty = HirType::Alphanumeric {
                byte_len: len,
                justified: JustifiedSpec::Left,
            };
            self.register_special_item(scope, reg, ty, len, synth_span, items, interner)?;
        }
        self.register_debug_item_group(scope, synth_span, items, interner)?;
        Ok(())
    }

    /// Registers dialect-enabled custom special registers (`-fregister=LIN,COL`).
    ///
    /// # Arguments
    ///
    /// * `scope` - Program scope ID.
    /// * `custom_registers` - Uppercase custom register names from [`coblang_diag::DialectConfig`].
    /// * `items` - Program's `HirDataItem` arena.
    /// * `interner` - Shared [`SymbolInterner`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if symbol interning fails.
    pub fn inject_custom_registers(
        &mut self,
        scope: ScopeId,
        custom_registers: &BTreeSet<String>,
        items: &mut Vec<HirDataItem>,
        interner: &mut SymbolInterner,
    ) -> Result<()> {
        let synth_span = SourceSpan::new(
            FileId::new(0),
            ByteOffset::ZERO,
            ByteLength::ZERO,
            ExpansionId::ROOT,
        );
        let reg_ty = HirType::BinaryInt {
            byte_width: 4,
            digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        };
        for reg in custom_registers {
            let sym = interner.intern(reg)?;
            if self.resolve_symbol_silent(scope, sym, &[]).is_none() {
                self.register_special_item(
                    scope,
                    reg,
                    reg_ty.clone(),
                    4,
                    synth_span,
                    items,
                    interner,
                )?;
            }
        }
        Ok(())
    }

    /// Synthesizes the standard 132-byte `01 DEBUG-ITEM` group and its `02` subfields in `WorkingStorage`
    /// unless the user program explicitly declared `DEBUG-ITEM`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Target program scope.
    /// * `span` - Synthetic source span.
    /// * `items` - Unit `HirDataItem` arena.
    /// * `interner` - Shared [`SymbolInterner`].
    fn register_debug_item_group(
        &mut self,
        scope: ScopeId,
        span: SourceSpan,
        items: &mut Vec<HirDataItem>,
        interner: &mut SymbolInterner,
    ) -> Result<()> {
        let dbg_sym = interner.intern("DEBUG-ITEM")?;
        if self.resolve_symbol_silent(scope, dbg_sym, &[]).is_some() {
            return Ok(());
        }
        let root_id = DataItemId::new(u32::try_from(items.len()).unwrap_or(0));
        let total_len = 132_u32;
        items.push(HirDataItem {
            id: root_id,
            scope,
            name: Some(dbg_sym),
            level: LevelNumber::Hierarchy(1),
            parent: None,
            children: Vec::new(),
            section: HirStorageSection::WorkingStorage,
            ty: HirType::Group {
                static_byte_len: total_len,
                odo_presence: DynamicOdoPresence::StaticSize,
            },
            byte_offset: ByteOffset::ZERO,
            total_byte_size: total_len,
            dimensions: Vec::new(),
            redefines: None,
            initial_value: None,
            span,
        });
        self.insert_symbol(
            scope,
            dbg_sym,
            SymbolCandidate {
                target: ResolvedSymbol::DataItem(root_id),
                qualifiers: Vec::new(),
                visibility: GlobalVisibility::Local,
            },
        );

        let fields = build_debug_item_subfields();

        let mut child_ids = Vec::with_capacity(fields.len());
        for (opt_name, off, len, f_ty, init) in fields {
            let cid = DataItemId::new(u32::try_from(items.len()).unwrap_or(0));
            let opt_sym = match opt_name {
                Some(nm) => Some(interner.intern(nm)?),
                None => None,
            };
            items.push(HirDataItem {
                id: cid,
                scope,
                name: opt_sym,
                level: LevelNumber::Hierarchy(2),
                parent: Some(root_id),
                children: Vec::new(),
                section: HirStorageSection::WorkingStorage,
                ty: f_ty,
                byte_offset: ByteOffset::new(off),
                total_byte_size: len,
                dimensions: Vec::new(),
                redefines: None,
                initial_value: init,
                span,
            });
            if let Some(sym) = opt_sym {
                self.insert_symbol(
                    scope,
                    sym,
                    SymbolCandidate {
                        target: ResolvedSymbol::DataItem(cid),
                        qualifiers: vec![dbg_sym],
                        visibility: GlobalVisibility::Local,
                    },
                );
            }
            child_ids.push(cid);
        }
        if let Some(root_item) = items.get_mut(root_id.as_u32() as usize) {
            root_item.children = child_ids;
        }
        Ok(())
    }

    /// Inserts a single synthesized special-register [`HirDataItem`] and symbol binding into `scope`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Target program/function/method scope.
    /// * `reg` - Special register name.
    /// * `ty` - Synthesized HIR type.
    /// * `byte_size` - Static byte width.
    /// * `span` - Synthetic source span.
    /// * `items` - Program data-item arena.
    /// * `interner` - Symbol interner.
    #[expect(
        clippy::too_many_arguments,
        reason = "Internal special-register builder helper."
    )]
    fn register_special_item(
        &mut self,
        scope: ScopeId,
        reg: &str,
        ty: HirType,
        byte_size: u32,
        span: SourceSpan,
        items: &mut Vec<HirDataItem>,
        interner: &mut SymbolInterner,
    ) -> Result<()> {
        let sym = interner.intern(reg)?;
        let id = DataItemId::new(u32::try_from(items.len()).unwrap_or(0));
        items.push(HirDataItem {
            id,
            scope,
            name: Some(sym),
            level: LevelNumber::Independent77,
            parent: None,
            children: Vec::new(),
            section: HirStorageSection::SpecialRegister,
            ty,
            byte_offset: ByteOffset::ZERO,
            total_byte_size: byte_size,
            dimensions: Vec::new(),
            redefines: None,
            initial_value: None,
            span,
        });
        self.insert_symbol(
            scope,
            sym,
            SymbolCandidate {
                target: ResolvedSymbol::DataItem(id),
                qualifiers: Vec::new(),
                visibility: GlobalVisibility::Local,
            },
        );
        Ok(())
    }

    /// Resolves `base [{IN|OF} qualifiers...]` starting in `scope` and walking `GLOBAL` ancestors.
    ///
    /// Peeks symbol resolution inside `scope` without emitting diagnostics.
    ///
    /// # Arguments
    ///
    /// * `scope` - Lookup starting scope.
    /// * `base` - Leaf identifier symbol.
    /// * `qualifiers` - Required ordered subsequence of ancestor qualifiers.
    #[must_use]
    pub fn resolve_symbol_silent(
        &self,
        scope: ScopeId,
        base: SymbolId,
        qualifiers: &[SymbolId],
    ) -> Option<ResolvedSymbol> {
        self.lookup_symbol_status(scope, base, qualifiers).0
    }

    /// Resolves `(base, qualifiers)` starting in `scope`, returning `(resolved, is_ambiguous)`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Lookup starting scope.
    /// * `base` - Leaf identifier symbol.
    /// * `qualifiers` - Required ordered subsequence of ancestor qualifiers.
    fn lookup_symbol_status(
        &self,
        scope: ScopeId,
        base: SymbolId,
        qualifiers: &[SymbolId],
    ) -> (Option<ResolvedSymbol>, bool) {
        let mut cur_opt = Some(scope);
        let mut in_local = true;

        while let Some(cur) = cur_opt {
            let Some(sc) = self.scope(cur) else {
                break;
            };
            if let Some(cands) = sc.by_name.get(&base) {
                let mut exact_matches = Vec::new();
                let mut partial_matches = Vec::new();
                for cand in cands {
                    if !in_local && cand.visibility != GlobalVisibility::Global {
                        continue;
                    }
                    if qualifiers_match(&cand.qualifiers, qualifiers) {
                        if cand.qualifiers.len() == qualifiers.len() {
                            exact_matches.push(&cand.target);
                        } else {
                            partial_matches.push(&cand.target);
                        }
                    }
                }
                if exact_matches.len() == 1 {
                    return (exact_matches.first().copied().cloned(), false);
                }
                let total = exact_matches.len() + partial_matches.len();
                if total == 1 {
                    return (partial_matches.first().copied().cloned(), false);
                }
                if total > 1 {
                    return (
                        exact_matches
                            .first()
                            .or_else(|| partial_matches.first())
                            .copied()
                            .cloned(),
                        true,
                    );
                }
            }
            cur_opt = sc.parent_scope;
            in_local = false;
        }
        (None, false)
    }

    /// Resolves a qualified identifier (`base IN q1 OF q2`) starting in `scope`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Lookup starting scope.
    /// * `base` - Leaf identifier symbol.
    /// * `qualifiers` - Required ordered subsequence of ancestor qualifiers.
    /// * `span` - Reference span for diagnostics.
    /// * `interner` - Interner for formatting identifier names in diagnostics.
    /// * `sink` - Diagnostic sink.
    pub fn resolve_symbol(
        &self,
        scope: ScopeId,
        base: SymbolId,
        qualifiers: &[SymbolId],
        span: SourceSpan,
        interner: &SymbolInterner,
        sink: &mut DiagnosticSink,
    ) -> Option<ResolvedSymbol> {
        let (resolved, is_ambiguous) = self.lookup_symbol_status(scope, base, qualifiers);
        if is_ambiguous {
            let label = interner.resolve_original(base).unwrap_or("identifier");
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0005AmbiguousSymbol,
                Some(span),
                format!("`{label}` is not uniquely defined; add `IN`/`OF` qualification"),
            ));
            return resolved;
        }
        if resolved.is_none() {
            let label = interner.resolve_original(base).unwrap_or("identifier");
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(span),
                format!("`{label}` is not defined"),
            ));
        }
        resolved
    }

    /// Resolves a paragraph or section reference (`name [IN section]`) inside `scope`.
    ///
    /// # Arguments
    ///
    /// * `scope` - Current program scope.
    /// * `name` - Paragraph or section symbol.
    /// * `section_qual` - Optional `IN`/`OF` section qualifier.
    /// * `span` - Reference span for diagnostics.
    /// * `interner` - Symbol interner.
    /// * `sink` - Diagnostic sink.
    pub fn resolve_procedure(
        &self,
        scope: ScopeId,
        name: SymbolId,
        section_qual: Option<SymbolId>,
        span: SourceSpan,
        interner: &SymbolInterner,
        sink: &mut DiagnosticSink,
    ) -> Option<ProcedureId> {
        let sc = self.scope(scope)?;
        let matches: Vec<ProcedureId> = sc
            .procedures
            .iter()
            .filter(|p| p.name == name && section_qual.is_none_or(|sq| p.section == Some(sq)))
            .map(|p| p.id)
            .collect();

        if matches.len() == 1 {
            matches.first().copied()
        } else if matches.is_empty() {
            let label = interner.resolve_original(name).unwrap_or("procedure");
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(span),
                format!("`{label}` is not a defined paragraph or section"),
            ));
            None
        } else {
            let label = interner.resolve_original(name).unwrap_or("procedure");
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0005AmbiguousSymbol,
                Some(span),
                format!("procedure `{label}` is ambiguous; qualify with `IN section-name`"),
            ));
            matches.first().copied()
        }
    }
}

/// Checks whether `required` is an ordered subsequence of `actual` ancestor qualifiers.
///
/// # Arguments
///
/// * `actual` - Candidate's full ancestor chain from immediate parent outward.
/// * `required` - User-supplied `IN`/`OF` qualifiers from immediate outward.
#[must_use]
fn qualifiers_match(actual: &[SymbolId], required: &[SymbolId]) -> bool {
    if required.is_empty() {
        return true;
    }
    let mut act_iter = actual.iter();
    for req in required {
        if !act_iter.any(|a| a == req) {
            return false;
        }
    }
    true
}

/// Helper alias keeping `HirCondition88` referenced for layout-to-symbol handoff.
pub type Condition88List = Vec<HirCondition88>;

/// Tuple `(opt_name, byte_offset, byte_len, ty, initial_value)` describing one `DEBUG-ITEM` subfield.
type DebugSubfieldSpec = (
    Option<&'static str>,
    u32,
    u32,
    HirType,
    Option<HirConstValue>,
);

/// Builds the 11 standard 132-byte `DEBUG-ITEM` subfield specifications.
fn build_debug_item_subfields() -> [DebugSubfieldSpec; 11] {
    let sub_ty = HirType::ZonedDecimal {
        digits: DigitCount::try_new(4).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::SeparateLeading,
        blank_zero: BlankZeroSpec::RetainZero,
    };
    let space_val = Some(HirConstValue::Figurative(HirFigurative::Space));
    let zero_val = Some(HirConstValue::Figurative(HirFigurative::Zero));
    let alnum = |byte_len: u32| HirType::Alphanumeric {
        byte_len,
        justified: JustifiedSpec::Left,
    };
    [
        (Some("DEBUG-LINE"), 0, 6, alnum(6), space_val.clone()),
        (None, 6, 1, alnum(1), space_val.clone()),
        (Some("DEBUG-NAME"), 7, 30, alnum(30), space_val.clone()),
        (None, 37, 1, alnum(1), space_val.clone()),
        (Some("DEBUG-SUB-1"), 38, 5, sub_ty.clone(), zero_val.clone()),
        (None, 43, 1, alnum(1), space_val.clone()),
        (Some("DEBUG-SUB-2"), 44, 5, sub_ty.clone(), zero_val.clone()),
        (None, 49, 1, alnum(1), space_val.clone()),
        (Some("DEBUG-SUB-3"), 50, 5, sub_ty, zero_val),
        (None, 55, 1, alnum(1), space_val.clone()),
        (Some("DEBUG-CONTENTS"), 56, 76, alnum(76), space_val),
    ]
}
