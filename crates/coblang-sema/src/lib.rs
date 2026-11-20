//! COBOL symbol resolver, `PICTURE` compiler, storage layout solver, typechecker, and HIR lowerer.

/// Milestone M12 intrinsic function typechecker, `REPOSITORY` resolver, `ALL` slices, and `FUNCTION-ID` signatures.
pub mod intrinsics;
/// Data Division hierarchy normalization, byte-exact layout, `REDEFINES`, `66 RENAMES`, and `88` conditions.
pub mod layout;
/// AST-to-HIR desugaring pass (`CORRESPONDING`, `88` conditions, abbreviated relations).
pub mod lower_hir;
/// Milestone M10 string, inspect, transform, and extended `ACCEPT` / `DISPLAY UPON ENVIRONMENT-*` lowering.
mod m10_strings;
/// Milestone M11 pointers, `SET` pointer/address/entry operations, `ALLOCATE`, and `FREE` lowering.
mod m11_pointers;
/// Milestone M13 `>>TURN`, `RAISE`/`RESUME`, `DECLARATIVES` `EC-*` rules, and NIST `DB` Debug Module lowering.
pub mod m13_exceptions;
/// Milestones M14–M16 File I/O (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`), `INDEXED` ISAM, and `SORT`/`MERGE` lowering.
pub mod m14_fileio;
/// Milestone M17 `NATIONAL` (`UTF-16`), `UTF-8`, and `-finternal-ebcdic` / `-febcdic-table=` lowering.
pub mod m17_charset;
/// Milestone M18 declarative `REPORT SECTION` (`RD`, `INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`) lowering.
pub mod m18_report;
/// Milestone M19 `SCREEN SECTION`, vendor screen rules, and extended `DISPLAY`/`ACCEPT` lowering.
pub mod m19_screen;
/// Milestone M20 `XML`/`JSON` generation & parsing and Object-Oriented COBOL (`CLASS-ID` / `INVOKE`) lowering.
pub mod m20_xml_json_oo;
/// Milestone M8 control-flow (`PERFORM`, `EVALUATE`, `SET`, `ALTER`, `NEXT SENTENCE`) lowering helpers.
mod m8_flow;
/// Milestone M9 table operations (`EC-BOUND-*` masks, `SEARCH`/`SEARCH ALL`, and `SORT table`).
mod m9_tables;
/// `PICTURE` string expander, validator, and `PicEditOp` bytecode compiler.
pub mod picture;
/// `SPECIAL-NAMES` `ALPHABET`, `CLASS`, and `SWITCH-0..36` HIR table builders.
pub mod special_names_hir;
/// Nested lexical scopes, `GLOBAL` propagation, special registers, and qualified path resolution.
pub mod symbols;
/// `MOVE`, arithmetic, subscript, refmod, `SET`, file, and intrinsic semantic validator.
pub mod typecheck;

pub use lower_hir::lower_compilation_group;
pub use picture::{
    ElementaryClauses, ParsedPicture, PictureContext, PictureFlavor, compile_elementary_type,
    parse_picture_string,
};
pub use symbols::{ProcedureEntry, ResolvedSymbol, ScopeSymbols, SymbolCandidate, SymbolTable};
