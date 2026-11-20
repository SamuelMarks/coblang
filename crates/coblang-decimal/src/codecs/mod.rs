//! Storage representation codecs for zoned decimal, packed BCD, binary integers, and floats.

/// 1..16-byte big-endian and native-endian `BINARY` / `COMP` / `COMP-4` / `COMP-5` / `COMP-X` codecs.
pub mod binary;
/// `COMP-1` (`f32`), `COMP-2` (`f64`), `FLOAT-EXTENDED` (`binary128`), and `FLOAT-DECIMAL-16/34` (`BID`/`DPD`) codecs.
pub mod float;
/// `PACKED-DECIMAL` (`COMP-3`) and unsigned `COMP-6` BCD codecs and `IS NUMERIC` validators.
pub mod packed;
/// Zoned decimal (`USAGE DISPLAY`) codecs across ASCII/`GnuCOBOL`, COBOL-85/EBCDIC-mapped, IBM EBCDIC, and `MicroFocus` overpunch tables.
pub mod zoned;
