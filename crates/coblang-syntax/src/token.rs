//! Strongly-typed COBOL lexical token definitions, literal prefixes, and lexer modes.

use coblang_common::{ScaleFactor, SourceSpan, SymbolId};
use coblang_pp::MarginZone;

/// Encoding and radix prefix for COBOL string, national, UTF-8, hexadecimal, and bit literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringPrefix {
    /// Unprefixed alphanumeric literal (`"..."` or `'...'`).
    Plain,
    /// Hexadecimal alphanumeric literal (`X"..."` or `X'...'`).
    HexX,
    /// Hexadecimal numeric/alphanumeric literal (`H"..."` or `H'...'`).
    HexH,
    /// Null-terminated C-compatible alphanumeric literal (`Z"..."` or `Z'...'`).
    NullTermZ,
    /// Binary bit-string literal (`B"0101"` or `B'0101'`).
    BitB,
    /// Hexadecimal bit-string literal (`BX"0F"` or `BX'0F'`).
    BitHexBX,
    /// National UTF-16 character literal (`N"..."` or `N'...'`).
    NationalN,
    /// Hexadecimal national UTF-16 literal (`NX"0041"` or `NX'0041'`).
    NationalHexNX,
    /// UTF-8 character literal (`U"..."` or `U'...'`).
    Utf8U,
    /// Hexadecimal UTF-8 character literal (`UX"41"` or `UX'41'`).
    Utf8HexUX,
}

/// Explicit leading sign classification on numeric literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiteralSign {
    /// No explicit `+` or `-` prefix (`123`).
    Unsigned,
    /// Explicit `+` prefix (`+123`).
    Positive,
    /// Explicit `-` prefix (`-123`).
    Negative,
}

/// Dialect reservation status of an interned COBOL word token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WordRole {
    /// Word appears in the active [`coblang_diag::DialectConfig::reserved_words`] set.
    ReservedKeyword,
    /// User-defined identifier or context-sensitive soft keyword.
    ContextSoftWord,
}

/// Active context-sensitive scanner mode inside [`crate::lexer::Lexer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LexerMode {
    /// Standard token scanning across COBOL divisions.
    Normal,
    /// Dedicated `PICTURE` character-string scanning after `PIC` / `PICTURE [IS]`.
    Picture,
    /// Obsolete `IDENTIFICATION DIVISION` comment-entry scanning until the next header.
    IdCommentParagraph,
}

/// Decimal-point character convention active during lexical scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalCommaMode {
    /// Period `.` is the decimal point and `,` is a separator (`12.34`).
    PeriodIsDecimal,
    /// Comma `,` is the decimal point under `DECIMAL-POINT IS COMMA` (`12,34`).
    CommaIsDecimal,
}

/// Strongly-typed COBOL lexical token classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// Clause, sentence, or header terminator period (`.`).
    Period,
    /// Optional separator comma (`,`).
    Comma,
    /// Optional separator semicolon (`;`).
    Semicolon,
    /// Left parenthesis (`(`).
    LParen,
    /// Right parenthesis (`)`).
    RParen,
    /// Reference-modification colon (`:`).
    Colon,
    /// String concatenation operator (`&`).
    Ampersand,
    /// Inline object-method invocation operator (`::`).
    DoubleColon,
    /// Addition or unary plus operator (`+`).
    Plus,
    /// Subtraction or unary minus operator (`-`).
    Minus,
    /// Multiplication operator (`*`).
    Star,
    /// Division operator (`/`).
    Slash,
    /// Exponentiation operator (`**`).
    StarStar,
    /// Relational equality operator (`=`).
    Eq,
    /// Relational inequality operator (`<>`).
    Ne,
    /// Relational less-than operator (`<`).
    Lt,
    /// Relational less-than-or-equal operator (`<=`).
    Le,
    /// Relational greater-than operator (`>`).
    Gt,
    /// Relational greater-than-or-equal operator (`>=`).
    Ge,
    /// Decoded string, national, UTF-8, hex, or bit literal.
    StringLit {
        /// Decoded payload bytes (UTF-16BE for `N`/`NX`, NUL-suffixed for `Z`, raw bytes otherwise).
        bytes: Vec<u8>,
        /// Original literal prefix kind.
        prefix: StringPrefix,
    },
    /// Exact integer literal (`digits` interned, e.g. `"123"`, `"+45"`, `"-9"`).
    IntegerLit(SymbolId),
    /// Exact fixed-point decimal literal (`12.34`, `-0.5`, `+3,14`).
    DecimalLit {
        /// Interned unscaled decimal digit string without sign or decimal point (e.g. `"1234"`).
        digits: SymbolId,
        /// Number of fractional decimal digits (`ScaleFactor` >= 0).
        scale: ScaleFactor,
        /// Explicit sign status of the literal.
        sign: LiteralSign,
    },
    /// Floating-point scientific literal (`1.5E+10`, `-2.0e-3`).
    FloatLit(SymbolId),
    /// Raw `PICTURE` character-string lexed in [`LexerMode::Picture`].
    PictureString(SymbolId),
    /// Interned COBOL identifier or keyword with margin zone and dialect reservation role.
    Word {
        /// Interned symbol handle in [`coblang_diag::SymbolInterner`].
        sym: SymbolId,
        /// Source margin zone (`AreaA`, `AreaB`, or `FreeFlow`).
        area: MarginZone,
        /// Hard-reserved vs context-soft classification.
        role: WordRole,
    },
    /// End-of-file sentinel token.
    Eof,
}

/// Single lexed COBOL [`TokenKind`] paired with its exact [`SourceSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Lexical category and payload.
    pub kind: TokenKind,
    /// Source span covering the token.
    pub span: SourceSpan,
}
