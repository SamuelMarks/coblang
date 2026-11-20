//! Audited LLVM 20 `TargetMachine` artifact emission (`.ll`, `.s`, `.o`) and native executable/module linking.

/// Pure-safe system linker invocation builder for `-x` executables and `-m` shared modules.
pub mod linker;
/// Audited `mlir-sys` / `llvm-sys` 201 translation and `TargetMachine` emission bridge.
pub mod llvm_bridge;

pub use linker::{
    LinkArtifactKind, LinkerInvocationSpec, invoke_system_linker, locate_runtime_lib_dir,
    resolve_output_path,
};
pub use llvm_bridge::{
    CompiledObjectArtifact, OwnedLlvmModule, emit_llvm_ir_text, emit_target_assembly,
    emit_target_object, host_default_target_triple, translate_to_llvm_module,
};
