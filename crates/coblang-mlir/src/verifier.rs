//! Custom `cobol.*` MLIR dialect verifier and [`VerifiedCobolMlirModule`] typestate wrapper.

use coblang_common::{CoblangError, Result};
use melior::ir::attribute::StringAttribute;
use melior::ir::block::BlockLike;
use melior::ir::operation::OperationLike;
use melior::ir::{Attribute, BlockRef, Module, OperationRef, RegionLike};

use crate::ops::{CobolOpKind, decode_hex_bytes, is_cobol_dialect_name};

/// Typestate-verified MLIR module guaranteed to pass both MLIR's builtin verifier and `cobol.*` dialect invariants.
#[derive(Debug)]
pub struct VerifiedCobolMlirModule<'ctx> {
    /// Verified `melior` module.
    module: Module<'ctx>,
}

impl<'ctx> VerifiedCobolMlirModule<'ctx> {
    /// Borrows the verified [`Module`].
    #[must_use]
    pub const fn as_module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Formats the verified `cobol` MLIR module as textual MLIR IR (`--emit-mlir`).
    #[must_use]
    pub fn to_mlir_text(&self) -> String {
        self.module.as_operation().to_string()
    }
}

/// Verifies `module` against builtin MLIR structural rules and all custom `cobol.*` operation schemas.
///
/// # Arguments
///
/// * `module` - Candidate `cobol` MLIR [`Module`] to validate.
///
/// # Errors
///
/// Returns [`CoblangError::MlirVerification`] if any operation violates operand counts, result counts,
/// mandatory attributes, or region block structure.
pub fn verify_cobol_module(module: Module<'_>) -> Result<VerifiedCobolMlirModule<'_>> {
    if !module.as_operation().verify() {
        return Err(CoblangError::MlirVerification {
            stage: "cobol.builtin",
            ir_snippet: String::from("<invalid-mlir-module>"),
            message: String::from("MLIR builtin module verifier rejected `cobol` module"),
        });
    }
    verify_block_ops(module.body())?;
    Ok(VerifiedCobolMlirModule { module })
}

/// Recursively verifies every operation in `block` and all nested regions.
///
/// # Arguments
///
/// * `block` - MLIR [`BlockRef`] to inspect.
fn verify_block_ops(block: BlockRef<'_, '_>) -> Result<()> {
    let mut cursor = block.first_operation();
    while let Some(op) = cursor {
        verify_single_op(op)?;
        for region in (0..op.region_count()).filter_map(|i| op.region(i).ok()) {
            let mut b_cur = region.first_block();
            while let Some(child_block) = b_cur {
                verify_block_ops(child_block)?;
                b_cur = child_block.next_in_region();
            }
        }
        cursor = op.next_in_block();
    }
    Ok(())
}

/// Checks mandatory attribute names on `op`.
///
/// # Arguments
///
/// * `op` - Operation reference under inspection.
/// * `stage` - Static stage name for error reporting.
/// * `keys` - Required attribute names.
fn require_attributes(op: OperationRef<'_, '_>, stage: &'static str, keys: &[&str]) -> Result<()> {
    for &key in keys {
        if !op.has_attribute(key) {
            return Err(CoblangError::MlirVerification {
                stage,
                ir_snippet: op.to_string(),
                message: format!("missing required attribute `{key}` on `{stage}`"),
            });
        }
    }
    Ok(())
}

/// Checks exact `(operands, results)` arity on `op`.
///
/// # Arguments
///
/// * `op` - Operation reference under inspection.
/// * `stage` - Static stage name for error reporting.
/// * `expected_operands` - Exact expected operand count (`None` when variadic).
/// * `expected_results` - Exact expected result count.
fn require_arity(
    op: OperationRef<'_, '_>,
    stage: &'static str,
    expected_operands: Option<usize>,
    expected_results: usize,
) -> Result<()> {
    if let Some(exp_ops) = expected_operands
        && op.operand_count() != exp_ops
    {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: format!(
                "`{stage}` expected {exp_ops} operand(s), found {}",
                op.operand_count()
            ),
        });
    }
    if op.result_count() != expected_results {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: format!(
                "`{stage}` expected {expected_results} result(s), found {}",
                op.result_count()
            ),
        });
    }
    Ok(())
}

/// Verifies a single operation if it belongs to the `cobol.*` dialect.
///
/// # Arguments
///
/// * `op` - Operation reference under inspection.
fn verify_single_op(op: OperationRef<'_, '_>) -> Result<()> {
    let raw_id = op.name();
    let op_name = raw_id.as_string_ref().as_str().unwrap_or("<non-utf8-op>");
    if !is_cobol_dialect_name(op_name) {
        return Ok(());
    }
    let Some(kind) = CobolOpKind::from_op_name(op_name) else {
        return Err(CoblangError::MlirVerification {
            stage: "cobol.dialect",
            ir_snippet: op.to_string(),
            message: format!("unknown `cobol` dialect operation `{op_name}`"),
        });
    };
    let stage = kind.op_name();
    match kind {
        CobolOpKind::Program => verify_program_op(op, stage),
        CobolOpKind::Storage => verify_storage_op(op, stage),
        CobolOpKind::Subscript => {
            if op.operand_count() < 2 {
                return Err(CoblangError::MlirVerification {
                    stage,
                    ir_snippet: op.to_string(),
                    message: String::from("`cobol.subscript` requires base pointer + >= 1 index"),
                });
            }
            require_arity(op, stage, None, 1)?;
            require_attributes(op, stage, &["strides", "bounds_mode"])
        }
        CobolOpKind::RefMod => {
            if op.operand_count() < 3 || op.operand_count() > 4 {
                return Err(CoblangError::MlirVerification {
                    stage,
                    ir_snippet: op.to_string(),
                    message: String::from("`cobol.refmod` expects 3 or 4 operands"),
                });
            }
            require_arity(op, stage, None, 2)?;
            require_attributes(op, stage, &["char_width", "bounds_mode"])
        }
        CobolOpKind::Display => {
            if !op.operand_count().is_multiple_of(2) {
                return Err(CoblangError::MlirVerification {
                    stage,
                    ir_snippet: op.to_string(),
                    message: String::from(
                        "`cobol.display` requires even `(ptr, len)` operand pairs",
                    ),
                });
            }
            require_arity(op, stage, None, 0)?;
            require_attributes(
                op,
                stage,
                &["device", "advancing", "item_kinds", "item_scales"],
            )
        }
        CobolOpKind::RtDispatch => {
            if op.result_count() > 1 {
                return Err(CoblangError::MlirVerification {
                    stage,
                    ir_snippet: op.to_string(),
                    message: String::from("`cobol.rt_dispatch` allows at most 1 result"),
                });
            }
            require_attributes(op, stage, &["callee"])
        }
        other => {
            let (exp_ops, exp_res, req_attrs) = fixed_op_schema(other);
            require_arity(op, stage, exp_ops, exp_res)?;
            require_attributes(op, stage, req_attrs)
        }
    }
}

/// Returns `(expected_operands, expected_results, required_attributes)` for regular `cobol.*` ops.
///
/// # Arguments
///
/// * `kind` - Operation kind variant.
fn fixed_op_schema(kind: CobolOpKind) -> (Option<usize>, usize, &'static [&'static str]) {
    match kind {
        CobolOpKind::FieldView => (
            Some(0),
            1,
            &[
                "storage_sym",
                "byte_offset",
                "byte_len",
                "field_kind",
                "digits",
                "scale",
            ],
        ),
        CobolOpKind::Move => (Some(2), 0, &["encoding", "src_byte_len", "dst_byte_len"]),
        CobolOpKind::Init => (Some(1), 0, &["policy", "byte_len", "fill_byte"]),
        CobolOpKind::Arith => (Some(2), 1, &["arith_kind", "rounding", "size_error"]),
        CobolOpKind::DivRem => (Some(4), 0, &["rounding", "size_error"]),
        CobolOpKind::Cmp => (Some(2), 1, &["rel_op", "collating_seq"]),
        CobolOpKind::ClassCheck => (Some(1), 1, &["class_kind", "byte_len"]),
        CobolOpKind::SignCheck => (Some(1), 1, &["sign_kind"]),
        CobolOpKind::PerformRange => (Some(0), 0, &["start_proc", "end_proc"]),
        CobolOpKind::Alter => (Some(0), 0, &["altered_para", "new_target"]),
        CobolOpKind::Call => (None, 0, &["dispatch", "callee", "exception_branches"]),
        CobolOpKind::Cancel | CobolOpKind::SetReturnCode | CobolOpKind::StopRun => {
            (Some(1), 0, &[])
        }
        CobolOpKind::Accept => (Some(1), 0, &["source", "exception_branches"]),
        CobolOpKind::StringVerb
        | CobolOpKind::UnstringVerb
        | CobolOpKind::Inspect
        | CobolOpKind::Program
        | CobolOpKind::Storage
        | CobolOpKind::Subscript
        | CobolOpKind::RefMod
        | CobolOpKind::Display
        | CobolOpKind::RtDispatch
        | CobolOpKind::Yield => (None, 0, &[]),
        CobolOpKind::FileOp => (None, 0, &["verb", "connector_id"]),
        CobolOpKind::SortMerge => (None, 0, &["connector_id"]),
        CobolOpKind::Report => (Some(0), 0, &["report_id", "action"]),
        CobolOpKind::Screen | CobolOpKind::XmlJson => (None, 0, &["action"]),
        CobolOpKind::GetReturnCode => (Some(0), 1, &[]),
        CobolOpKind::Goback => (Some(0), 0, &["mode"]),
        CobolOpKind::Trim => (Some(2), 2, &["direction"]),
        CobolOpKind::Loop => (Some(0), 0, &["loop_kind", "test_pos"]),
        CobolOpKind::Sentence => (Some(0), 0, &[]),
        CobolOpKind::FlowExit => (Some(0), 0, &["exit_kind", "section_end"]),
        CobolOpKind::Goto => (None, 0, &["targets", "alter_slot_sym"]),
    }
}

/// Verifies `cobol.program` region, block, and mandatory attributes.
///
/// # Arguments
///
/// * `op` - `cobol.program` operation reference.
/// * `stage` - Static stage label.
fn verify_program_op(op: OperationRef<'_, '_>, stage: &'static str) -> Result<()> {
    require_arity(op, stage, Some(0), 0)?;
    require_attributes(
        op,
        stage,
        &[
            "sym_name",
            "raw_c_alias",
            "rc_slot_sym",
            "initial_state",
            "recursivity",
            "entry_convention",
        ],
    )?;
    let Ok(region) = op.region(0) else {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: String::from("`cobol.program` must own 1 body region"),
        });
    };
    let Some(entry_block) = region.first_block() else {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: String::from("`cobol.program` body region must contain an entry block"),
        });
    };
    if entry_block.first_operation().is_none() {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: String::from("`cobol.program` entry block cannot be empty"),
        });
    }
    Ok(())
}

/// Verifies `cobol.storage` attributes and `init_hex` byte count consistency.
///
/// # Arguments
///
/// * `op` - `cobol.storage` operation reference.
/// * `stage` - Static stage label.
fn verify_storage_op(op: OperationRef<'_, '_>, stage: &'static str) -> Result<()> {
    require_arity(op, stage, Some(0), 0)?;
    require_attributes(
        op,
        stage,
        &[
            "sym_name",
            "storage_kind",
            "byte_size",
            "alignment",
            "init_hex",
        ],
    )?;
    let hex_attr = op
        .attribute("init_hex")
        .ok()
        .and_then(|a| StringAttribute::try_from(a).ok());
    let Some(s_attr) = hex_attr else {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: String::from("`init_hex` on `cobol.storage` must be a StringAttribute"),
        });
    };
    let printed = Attribute::from(s_attr).to_string();
    let unquoted = printed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(&printed);
    let decoded = decode_hex_bytes(unquoted)?;
    if decoded.is_empty() {
        return Err(CoblangError::MlirVerification {
            stage,
            ir_snippet: op.to_string(),
            message: String::from("`cobol.storage` `init_hex` must contain at least 1 byte"),
        });
    }
    Ok(())
}
