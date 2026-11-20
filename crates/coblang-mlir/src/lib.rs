//! Custom COBOL MLIR 20 dialect builders, verifiers, canonicalizers, and LLVM dialect lowering.

#![forbid(unsafe_code)]

/// Zero-heap-allocation `PERFORM ... THRU`, sequential fallthrough, `GO TO` / `ALTER`, and loop CFG lowering.
pub mod cfg_perform;
/// MLIR 20 context initialization and source-span location mapping.
pub mod context;
/// HIR-to-`cobol` MLIR dialect lowering pass.
pub mod lower_from_hir;
/// Milestone M10 (`STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`, `ACCEPT`, `DISPLAY UPON ENVIRONMENT-*`) MLIR lowering.
pub mod lower_m10;
/// Milestone M11 (`USAGE POINTER`, `PROGRAM-POINTER`, `ADDRESS OF`, `BASED`, `SET`, `ALLOCATE`, `FREE`) MLIR lowering.
pub mod lower_m11;
/// Milestone M12 (~120 intrinsics, `ALL` table slices, `MODULE-*` stack, and `FUNCTION-ID` UDF) MLIR lowering.
pub mod lower_m12;
/// Milestone M13 (`>>TURN`, `RAISE`/`RESUME`, `DECLARATIVES`, and NIST `DB` Debug Module) MLIR lowering.
pub mod lower_m13;
/// Milestones M14 & M15 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`, and `INDEXED` ISAM) MLIR lowering.
pub mod lower_m14;
/// Milestone M16 (`SORT`, `MERGE`, `RELEASE`, `RETURN`, and `SORT-RETURN`) MLIR lowering.
pub mod lower_m16;
/// Milestone M17 (`NATIONAL` UTF-16, `UTF-8`, and `-finternal-ebcdic`) MLIR lowering.
pub mod lower_m17;
/// Milestone M18 (`REPORT SECTION`, `INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`, and `USE BEFORE REPORTING`) MLIR lowering.
pub mod lower_m18;
/// Milestone M19 (`SCREEN SECTION`, extended `DISPLAY`/`ACCEPT`, and `SET ... ATTRIBUTE`) MLIR lowering.
pub mod lower_m19;
/// Milestone M20 (`XML`/`JSON` generation & parsing and Object-Oriented COBOL `CLASS-ID`/`INVOKE`) MLIR lowering.
pub mod lower_m20;
/// `cobol` MLIR-to-`llvm` MLIR dialect lowering and optimization pass pipeline.
pub mod lower_to_llvm;
/// Strongly-typed `cobol.*` MLIR operation builders and attribute descriptors.
pub mod ops;
/// Fast-path classifier for fixed-length alphanumeric `MOVE`s and inline `i64`/`i128` `BINARY` arithmetic.
pub mod opt_fastpath;
/// Compile-time Working-Storage layout grouping, initial `VALUE` byte encoding, and `INITIALIZE` plan builders.
pub mod storage_init;
/// Custom `cobol.*` dialect verifier and `VerifiedCobolMlirModule` typestate wrapper.
pub mod verifier;

pub use context::CobolMlirContext;
pub use lower_from_hir::lower_hir_to_cobol_mlir;
pub use lower_to_llvm::{LoweredLlvmMlirModule, MainEntryMode, lower_cobol_to_llvm_mlir};
pub use verifier::{VerifiedCobolMlirModule, verify_cobol_module};
