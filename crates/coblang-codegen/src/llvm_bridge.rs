//! Audited `mlir-sys` / `llvm-sys` 201 translation and `TargetMachine` artifact emission.

use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::Once;

use coblang_common::{CoblangError, OptimizationLevel, Result};
use coblang_mlir::LoweredLlvmMlirModule;
use llvm_sys::core::{
    LLVMContextCreate, LLVMContextDispose, LLVMDisposeMemoryBuffer, LLVMDisposeMessage,
    LLVMDisposeModule, LLVMGetBufferSize, LLVMGetBufferStart, LLVMPrintModuleToString,
    LLVMSetDataLayout, LLVMSetTarget,
};
use llvm_sys::prelude::{LLVMContextRef, LLVMMemoryBufferRef, LLVMModuleRef};
use llvm_sys::target::{
    LLVM_InitializeAllAsmParsers, LLVM_InitializeAllAsmPrinters, LLVM_InitializeAllTargetInfos,
    LLVM_InitializeAllTargetMCs, LLVM_InitializeAllTargets, LLVMCopyStringRepOfTargetData,
    LLVMDisposeTargetData,
};
use llvm_sys::target_machine::{
    LLVMCodeGenFileType, LLVMCodeGenOptLevel, LLVMCodeModel, LLVMCreateTargetDataLayout,
    LLVMCreateTargetMachine, LLVMDisposeTargetMachine, LLVMGetDefaultTargetTriple,
    LLVMGetTargetFromTriple, LLVMRelocMode, LLVMTargetMachineEmitToMemoryBuffer,
    LLVMTargetMachineRef, LLVMTargetRef,
};
use mlir_sys::mlirTranslateModuleToLLVMIR;

/// Typestate-verified native relocatable object file artifact emitted by LLVM 20.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledObjectArtifact {
    /// Raw ELF / Mach-O / COFF `.o` bytes.
    bytes: Vec<u8>,
    /// Target triple used to generate `bytes`.
    target_triple: String,
}

impl CompiledObjectArtifact {
    /// Returns the raw `.o` object file bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the artifact and returns the owned `.o` byte vector.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the target triple string.
    #[must_use]
    pub fn target_triple(&self) -> &str {
        &self.target_triple
    }
}

/// RAII owner of a per-invocation `LLVMContextRef` and translated `LLVMModuleRef`.
#[derive(Debug)]
pub struct OwnedLlvmModule {
    /// Dedicated thread-isolated LLVM context.
    llvm_ctx: LLVMContextRef,
    /// Translated LLVM IR module owned by `llvm_ctx`.
    llvm_mod: LLVMModuleRef,
}

impl Drop for OwnedLlvmModule {
    /// Disposes `llvm_mod` followed by `llvm_ctx`.
    fn drop(&mut self) {
        #[expect(
            unsafe_code,
            reason = "Audited RAII disposal of `LLVMModuleRef` before its owning `LLVMContextRef`."
        )]
        // SAFETY: `self.llvm_mod` and `self.llvm_ctx` were created together in `translate_to_llvm_module` and are disposed once here in reverse order.
        unsafe {
            LLVMDisposeModule(self.llvm_mod);
            LLVMContextDispose(self.llvm_ctx);
        }
    }
}

/// RAII owner of an `LLVMTargetMachineRef`.
struct OwnedTargetMachine {
    /// Non-null target machine pointer.
    raw: LLVMTargetMachineRef,
}

impl Drop for OwnedTargetMachine {
    /// Disposes the underlying `LLVMTargetMachineRef`.
    fn drop(&mut self) {
        #[expect(
            unsafe_code,
            reason = "Audited RAII disposal of `LLVMTargetMachineRef`."
        )]
        // SAFETY: `self.raw` is a valid non-null `LLVMTargetMachineRef` created by `LLVMCreateTargetMachine`.
        unsafe {
            LLVMDisposeTargetMachine(self.raw);
        }
    }
}

/// Initializes all compiled-in LLVM 20 target infos, MC layers, asm parsers, and asm printers once per process.
pub fn ensure_llvm_targets_initialized() {
    /// Process-wide one-time guard preventing concurrent target-registry initialization races.
    static INIT_TARGETS: Once = Once::new();
    INIT_TARGETS.call_once(|| {
        #[expect(
            unsafe_code,
            reason = "Audited one-time LLVM 20 global target registry initialization."
        )]
        // SAFETY: Protected by `Once`, so global LLVM target registries are initialized at most once across all threads.
        unsafe {
            LLVM_InitializeAllTargetInfos();
            LLVM_InitializeAllTargets();
            LLVM_InitializeAllTargetMCs();
            LLVM_InitializeAllAsmParsers();
            LLVM_InitializeAllAsmPrinters();
        }
    });
}

/// Queries LLVM 20's default host target triple (e.g. `arm64-apple-darwin...` or `x86_64-unknown-linux-gnu`).
#[must_use]
pub fn host_default_target_triple() -> String {
    ensure_llvm_targets_initialized();
    #[expect(
        unsafe_code,
        reason = "Audited `LLVMGetDefaultTargetTriple` + `LLVMDisposeMessage` FFI call."
    )]
    // SAFETY: `LLVMGetDefaultTargetTriple` allocates a NUL-terminated C string owned by caller until `LLVMDisposeMessage`.
    unsafe {
        let raw_ptr = LLVMGetDefaultTargetTriple();
        let triple = CStr::from_ptr(raw_ptr).to_string_lossy().into_owned();
        LLVMDisposeMessage(raw_ptr);
        triple
    }
}

/// Converts an owned LLVM diagnostic `*mut c_char` message into a Rust `String` and disposes it.
///
/// # Arguments
///
/// * `err_ptr` - Nullable LLVM message pointer allocated via `LLVMCreateMessage`.
///
/// # Safety
///
/// When non-null, `err_ptr` must have been allocated by LLVM message APIs so `LLVMDisposeMessage` is valid.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Audited conversion and `LLVMDisposeMessage` cleanup of LLVM diagnostic string."
)]
pub unsafe fn consume_llvm_error_message(err_ptr: *mut c_char) -> String {
    if err_ptr.is_null() {
        String::from("unknown LLVM target error")
    } else {
        // SAFETY: Caller guarantees non-null `err_ptr` is a valid LLVM-allocated NUL-terminated string.
        unsafe {
            let msg = CStr::from_ptr(err_ptr).to_string_lossy().into_owned();
            LLVMDisposeMessage(err_ptr);
            msg
        }
    }
}

/// Translates a [`LoweredLlvmMlirModule`] into an [`OwnedLlvmModule`] via `mlirTranslateModuleToLLVMIR`.
///
/// # Arguments
///
/// * `lowered` - Verified `llvm`-dialect MLIR module.
///
/// # Errors
///
/// Returns [`CoblangError::CodegenTarget`] if `mlirTranslateModuleToLLVMIR` returns a null module.
pub fn translate_to_llvm_module(lowered: &LoweredLlvmMlirModule<'_>) -> Result<OwnedLlvmModule> {
    let raw_op = lowered.as_module().as_operation().to_raw();
    translate_raw_mlir_op(raw_op)
}

/// Translates a raw `MlirOperation` into an [`OwnedLlvmModule`], exposed in-crate for negative error-path testing.
///
/// # Arguments
///
/// * `raw_op` - Raw `MlirOperation` handle of a module operation.
///
/// # Errors
///
/// Returns [`CoblangError::CodegenTarget`] if MLIR-to-LLVM-IR translation fails.
pub fn translate_raw_mlir_op(raw_op: mlir_sys::MlirOperation) -> Result<OwnedLlvmModule> {
    ensure_llvm_targets_initialized();
    #[expect(
        unsafe_code,
        reason = "Audited `LLVMContextCreate` + `mlirTranslateModuleToLLVMIR` FFI translation."
    )]
    // SAFETY: `LLVMContextCreate` creates a dedicated context; `raw_op` is a live `MlirOperation`. On failure `llvm_ctx` is disposed immediately.
    unsafe {
        let llvm_ctx = LLVMContextCreate();
        let mlir_llvm_ctx = llvm_ctx.cast::<mlir_sys::LLVMOpaqueContext>();
        let raw_mod = mlirTranslateModuleToLLVMIR(raw_op, mlir_llvm_ctx);
        let llvm_mod = raw_mod.cast::<llvm_sys::LLVMModule>();
        if llvm_mod.is_null() {
            LLVMContextDispose(llvm_ctx);
            return Err(CoblangError::CodegenTarget {
                triple: String::from("<mlir-translate>"),
                message: String::from("`mlirTranslateModuleToLLVMIR` rejected module"),
            });
        }
        Ok(OwnedLlvmModule { llvm_ctx, llvm_mod })
    }
}

/// Renders textual LLVM 20 IR (`.ll` / `-emit-llvm`) from `module`.
///
/// # Arguments
///
/// * `module` - Translated [`OwnedLlvmModule`].
#[must_use]
pub fn emit_llvm_ir_text(module: &OwnedLlvmModule) -> String {
    #[expect(
        unsafe_code,
        reason = "Audited `LLVMPrintModuleToString` + `LLVMDisposeMessage` FFI call."
    )]
    // SAFETY: `module.llvm_mod` is a valid live `LLVMModuleRef`; returned C string is disposed before returning.
    unsafe {
        let raw = LLVMPrintModuleToString(module.llvm_mod);
        let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
        LLVMDisposeMessage(raw);
        text
    }
}

/// Maps [`OptimizationLevel`] to `LLVMCodeGenOptLevel`.
///
/// # Arguments
///
/// * `opt` - Workspace optimization level.
#[must_use]
pub const fn map_codegen_opt_level(opt: OptimizationLevel) -> LLVMCodeGenOptLevel {
    match opt {
        OptimizationLevel::O0 => LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
        OptimizationLevel::O1 => LLVMCodeGenOptLevel::LLVMCodeGenLevelLess,
        OptimizationLevel::O2 | OptimizationLevel::Os => {
            LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
        }
        OptimizationLevel::O3 => LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive,
    }
}

/// Emits target assembly (`.s` / `-S`) for `triple` at `opt`.
///
/// # Arguments
///
/// * `module` - Translated [`OwnedLlvmModule`].
/// * `triple` - Target triple string.
/// * `opt` - Codegen optimization level.
///
/// # Errors
///
/// Returns [`CoblangError::CodegenTarget`] if `triple` is invalid or assembly emission fails.
pub fn emit_target_assembly(
    module: &OwnedLlvmModule,
    triple: &str,
    opt: OptimizationLevel,
) -> Result<String> {
    let bytes = emit_machine_buffer(module, triple, opt, LLVMCodeGenFileType::LLVMAssemblyFile)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Emits native relocatable object code (`.o` / `-c`) for `triple` at `opt`.
///
/// # Arguments
///
/// * `module` - Translated [`OwnedLlvmModule`].
/// * `triple` - Target triple string.
/// * `opt` - Codegen optimization level.
///
/// # Errors
///
/// Returns [`CoblangError::CodegenTarget`] if `triple` is invalid or object emission fails.
pub fn emit_target_object(
    module: &OwnedLlvmModule,
    triple: &str,
    opt: OptimizationLevel,
) -> Result<CompiledObjectArtifact> {
    let bytes = emit_machine_buffer(module, triple, opt, LLVMCodeGenFileType::LLVMObjectFile)?;
    Ok(CompiledObjectArtifact {
        bytes,
        target_triple: String::from(triple),
    })
}

/// Configures an `LLVMTargetMachineRef` for `triple` and emits `file_type` into a `Vec<u8>`.
///
/// # Arguments
///
/// * `module` - Translated [`OwnedLlvmModule`].
/// * `triple` - Target triple string.
/// * `opt` - Optimization level.
/// * `file_type` - `LLVMAssemblyFile` or `LLVMObjectFile`.
fn emit_machine_buffer(
    module: &OwnedLlvmModule,
    triple: &str,
    opt: OptimizationLevel,
    file_type: LLVMCodeGenFileType,
) -> Result<Vec<u8>> {
    ensure_llvm_targets_initialized();
    let c_triple = CString::new(triple).map_err(|_| CoblangError::CodegenTarget {
        triple: String::from(triple),
        message: String::from("target triple contains interior NUL byte"),
    })?;

    #[expect(
        unsafe_code,
        reason = "Audited LLVM 20 `TargetMachine` lookup, data-layout assignment, and memory-buffer codegen."
    )]
    // SAFETY: All C strings are NUL-terminated; `OwnedTargetMachine` and `LLVMDisposeMemoryBuffer` ensure deterministic cleanup on all paths.
    unsafe {
        let mut target: LLVMTargetRef = ptr::null_mut();
        let mut err_msg: *mut c_char = ptr::null_mut();
        if LLVMGetTargetFromTriple(c_triple.as_ptr(), &raw mut target, &raw mut err_msg) != 0 {
            return Err(CoblangError::CodegenTarget {
                triple: String::from(triple),
                message: consume_llvm_error_message(err_msg),
            });
        }

        let tm_raw = LLVMCreateTargetMachine(
            target,
            c_triple.as_ptr(),
            c"generic".as_ptr(),
            c"".as_ptr(),
            map_codegen_opt_level(opt),
            LLVMRelocMode::LLVMRelocPIC,
            LLVMCodeModel::LLVMCodeModelDefault,
        );
        let tm = OwnedTargetMachine { raw: tm_raw };

        LLVMSetTarget(module.llvm_mod, c_triple.as_ptr());
        let data_layout = LLVMCreateTargetDataLayout(tm.raw);
        let dl_str = LLVMCopyStringRepOfTargetData(data_layout);
        LLVMSetDataLayout(module.llvm_mod, dl_str);
        LLVMDisposeMessage(dl_str);
        LLVMDisposeTargetData(data_layout);

        let mut mem_buf: LLVMMemoryBufferRef = ptr::null_mut();
        let mut emit_err: *mut c_char = ptr::null_mut();
        let emit_rc = LLVMTargetMachineEmitToMemoryBuffer(
            tm.raw,
            module.llvm_mod,
            file_type,
            &raw mut emit_err,
            &raw mut mem_buf,
        );
        check_emit_buffer_result(emit_rc, emit_err, mem_buf, triple)
    }
}

/// Validates the return status of `LLVMTargetMachineEmitToMemoryBuffer` and extracts owned bytes.
///
/// # Arguments
///
/// * `emit_rc` - Return status (`0` on success, non-zero on failure).
/// * `emit_err` - Nullable LLVM error message pointer when `emit_rc != 0`.
/// * `mem_buf` - Valid `LLVMMemoryBufferRef` when `emit_rc == 0`.
/// * `triple` - Target triple for diagnostic attribution.
///
/// # Errors
///
/// Returns [`CoblangError::CodegenTarget`] when `emit_rc != 0`.
///
/// # Safety
///
/// When `emit_rc == 0`, `mem_buf` must be a valid non-null `LLVMMemoryBufferRef` to read and dispose;
/// when `emit_rc != 0`, `emit_err` must be either null or an LLVM-allocated message string.
#[expect(
    unsafe_code,
    reason = "Audited memory-buffer extraction for `LLVMTargetMachineEmitToMemoryBuffer`."
)]
pub unsafe fn check_emit_buffer_result(
    emit_rc: i32,
    emit_err: *mut c_char,
    mem_buf: LLVMMemoryBufferRef,
    triple: &str,
) -> Result<Vec<u8>> {
    if emit_rc != 0 {
        // SAFETY: Caller guarantees `emit_err` is either null or an LLVM-allocated message string when `emit_rc != 0`.
        let message = unsafe { consume_llvm_error_message(emit_err) };
        return Err(CoblangError::CodegenTarget {
            triple: String::from(triple),
            message,
        });
    }
    // SAFETY: Caller guarantees `mem_buf` is valid when `emit_rc == 0`.
    unsafe {
        let start = LLVMGetBufferStart(mem_buf).cast::<u8>();
        let len = LLVMGetBufferSize(mem_buf);
        let bytes = if len == 0 {
            Vec::new()
        } else {
            core::slice::from_raw_parts(start, len).to_vec()
        };
        LLVMDisposeMemoryBuffer(mem_buf);
        Ok(bytes)
    }
}
