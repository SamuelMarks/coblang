//! Built-in EBCDIC<->ASCII translation tables (`default`, `alternate`, `ebcdic500_*`, `cp1047`,
//! `restricted-gc`) and `.ttbl` custom translation table parser (`-febcdic-table=`).

#![deny(clippy::indexing_slicing)]

/// Runtime internal character encoding selected for `USAGE DISPLAY` / alphanumeric storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InternalCharsetMode {
    /// Native ASCII / UTF-8-compatible single-byte storage (`0x20` space, `0x30..=0x39` digits).
    #[default]
    Ascii,
    /// Native EBCDIC single-byte storage (`0x40` space, `0xF0..=0xF9` unsigned digits, `-finternal-ebcdic`).
    Ebcdic,
}

/// Active paired 256-byte EBCDIC<->ASCII translation tables configured on [`crate::DialectConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EbcdicTranslationTables {
    /// Maps each EBCDIC byte (`0x00..=0xFF`) to its ASCII/Latin-1 byte.
    pub ebcdic_to_ascii: [u8; 256],
    /// Maps each ASCII/Latin-1 byte (`0x00..=0xFF`) to its EBCDIC byte.
    pub ascii_to_ebcdic: [u8; 256],
}

impl Default for EbcdicTranslationTables {
    /// Returns the `GnuCOBOL` `default` EBCDIC<->ASCII translation table pair.
    fn default() -> Self {
        Self::from_ebcdic_to_ascii(DEFAULT_EBCDIC_TO_ASCII)
    }
}

impl EbcdicTranslationTables {
    /// Constructs a paired [`EbcdicTranslationTables`] by inverting `ebcdic_to_ascii`.
    ///
    /// # Arguments
    ///
    /// * `ebcdic_to_ascii` - 256-byte EBCDIC-to-ASCII lookup table.
    #[must_use]
    pub fn from_ebcdic_to_ascii(ebcdic_to_ascii: [u8; 256]) -> Self {
        let ascii_to_ebcdic = invert_byte_table(&ebcdic_to_ascii);
        Self {
            ebcdic_to_ascii,
            ascii_to_ebcdic,
        }
    }

    /// Constructs a paired [`EbcdicTranslationTables`] by inverting `ascii_to_ebcdic`.
    ///
    /// # Arguments
    ///
    /// * `ascii_to_ebcdic` - 256-byte ASCII-to-EBCDIC lookup table.
    #[must_use]
    pub fn from_ascii_to_ebcdic(ascii_to_ebcdic: [u8; 256]) -> Self {
        let ebcdic_to_ascii = invert_byte_table(&ascii_to_ebcdic);
        Self {
            ebcdic_to_ascii,
            ascii_to_ebcdic,
        }
    }

    /// Resolves a named built-in `-febcdic-table=` preset (case-insensitive).
    ///
    /// # Arguments
    ///
    /// * `name` - Built-in table identifier (`"default"`, `"alternate"`, `"ebcdic500_ascii7bit"`, etc.).
    #[must_use]
    pub fn from_builtin_name(name: &str) -> Option<Self> {
        let lower = name.trim().to_ascii_lowercase();
        let normalized = lower.strip_suffix(".ttbl").unwrap_or(&lower);
        match normalized {
            "default" | "cp037" | "ibm037" | "ibm1140" | "cp1140" => Some(Self::default()),
            "alternate" => {
                let mut e2a = DEFAULT_EBCDIC_TO_ASCII;
                set_slot(&mut e2a, 0x4F, b'|');
                set_slot(&mut e2a, 0x6A, 0xA6);
                set_slot(&mut e2a, 0x5F, b'^');
                set_slot(&mut e2a, 0xB0, 0xAC);
                Some(Self::from_ebcdic_to_ascii(e2a))
            }
            "ebcdic500_ascii7bit"
            | "ebcdic500_ascii8bit"
            | "ebcdic500_latin1"
            | "cp500"
            | "ibm500" => {
                let mut e2a = DEFAULT_EBCDIC_TO_ASCII;
                set_slot(&mut e2a, 0x4A, b'[');
                set_slot(&mut e2a, 0x5A, b']');
                set_slot(&mut e2a, 0x4F, b'!');
                set_slot(&mut e2a, 0x5F, b'^');
                set_slot(&mut e2a, 0x6A, b'|');
                set_slot(&mut e2a, 0xBA, 0xA2);
                set_slot(&mut e2a, 0xBB, 0xA6);
                set_slot(&mut e2a, 0xB0, 0xAC);
                Some(Self::from_ebcdic_to_ascii(e2a))
            }
            "cp1047" | "ibm1047" => {
                let mut e2a = DEFAULT_EBCDIC_TO_ASCII;
                set_slot(&mut e2a, 0xAD, b'[');
                set_slot(&mut e2a, 0xBD, b']');
                set_slot(&mut e2a, 0x4F, b'|');
                set_slot(&mut e2a, 0x5F, b'^');
                set_slot(&mut e2a, 0x6A, 0xA6);
                set_slot(&mut e2a, 0xBA, 0xAC);
                set_slot(&mut e2a, 0xBB, 0xA8);
                set_slot(&mut e2a, 0xB0, 0xAF);
                Some(Self::from_ebcdic_to_ascii(e2a))
            }
            "restricted-gc" | "restricted_gc" => {
                let mut e2a = DEFAULT_EBCDIC_TO_ASCII;
                for b in 0_u8..=255_u8 {
                    let mapped = e2a.get(usize::from(b)).copied().unwrap_or(b);
                    if mapped > 0x7F {
                        set_slot(&mut e2a, b, b);
                    }
                }
                Some(Self::from_ebcdic_to_ascii(e2a))
            }
            _ => None,
        }
    }

    /// Parses a `.ttbl` hex translation table file (`256` or `512` hex byte tokens, `#` comments).
    ///
    /// # Arguments
    ///
    /// * `text` - Raw `.ttbl` file contents.
    /// * `display_name` - Original CLI/config table path for `libcob:` diagnostic messages.
    ///
    /// # Errors
    ///
    /// Returns the ordered list of `libcob: error: ...` diagnostic lines on invalid hex bytes or wrong token count.
    pub fn parse_ttbl_text(text: &str, display_name: &str) -> Result<Self, Vec<String>> {
        let mut bytes = Vec::with_capacity(512);
        let mut bad_tokens: Vec<(usize, String)> = Vec::new();
        let mut total_tokens: usize = 0;

        for (idx, raw_line) in text.lines().enumerate() {
            let line_no = idx + 1;
            let uncommented = raw_line.split('#').next().unwrap_or(raw_line);
            for tok in uncommented.split_whitespace() {
                total_tokens += 1;
                if tok.len() == 2
                    && let Ok(val) = u8::from_str_radix(tok, 16)
                {
                    bytes.push(val);
                } else {
                    bad_tokens.push((line_no, String::from(tok)));
                }
            }
        }

        let mut errs = Vec::new();
        if !bad_tokens.is_empty() {
            errs.push(format!(
                "libcob: error: errors in translation table '{display_name}':"
            ));
            for (ln, tok) in &bad_tokens {
                errs.push(format!(
                    "libcob: error: invalid hex byte on line {ln}: '{tok}'"
                ));
            }
        }

        match total_tokens {
            0..=255 => errs.push(format!(
                "libcob: error: not enough data in translation table '{display_name}'"
            )),
            256 | 512 => {}
            257..=511 => errs.push(format!(
                "libcob: error: either not enough or too much data in translation table '{display_name}'"
            )),
            _ => errs.push(format!(
                "libcob: error: too much data in translation table '{display_name}'"
            )),
        }

        if !errs.is_empty() {
            return Err(errs);
        }

        if bytes.len() == 256 {
            let tbl = slice_to_256(&bytes);
            if tbl.get(0x40).copied() == Some(0x20) {
                Ok(Self::from_ebcdic_to_ascii(tbl))
            } else {
                Ok(Self::from_ascii_to_ebcdic(tbl))
            }
        } else {
            let first = slice_to_256(bytes.get(..256).unwrap_or(&[]));
            let second = slice_to_256(bytes.get(256..512).unwrap_or(&[]));
            if first.get(0x40).copied() == Some(0x20) {
                Ok(Self {
                    ebcdic_to_ascii: first,
                    ascii_to_ebcdic: second,
                })
            } else {
                Ok(Self {
                    ebcdic_to_ascii: second,
                    ascii_to_ebcdic: first,
                })
            }
        }
    }

    /// Transcodes ASCII/Latin-1 bytes into EBCDIC using `self.ascii_to_ebcdic`.
    ///
    /// # Arguments
    ///
    /// * `src` - Source ASCII/Latin-1 byte slice.
    #[must_use]
    pub fn transcode_ascii_to_ebcdic(&self, src: &[u8]) -> Vec<u8> {
        src.iter()
            .map(|&b| {
                self.ascii_to_ebcdic
                    .get(usize::from(b))
                    .copied()
                    .unwrap_or(b)
            })
            .collect()
    }

    /// Transcodes EBCDIC bytes into ASCII/Latin-1 using `self.ebcdic_to_ascii`.
    ///
    /// # Arguments
    ///
    /// * `src` - Source EBCDIC byte slice.
    #[must_use]
    pub fn transcode_ebcdic_to_ascii(&self, src: &[u8]) -> Vec<u8> {
        src.iter()
            .map(|&b| {
                self.ebcdic_to_ascii
                    .get(usize::from(b))
                    .copied()
                    .unwrap_or(b)
            })
            .collect()
    }
}

/// Writes `val` at `idx` inside a 256-byte table without panicking or indexing syntax.
///
/// # Arguments
///
/// * `tbl` - Mutable 256-byte lookup array.
/// * `idx` - Byte slot index (`0..=255`).
/// * `val` - Mapped byte value.
fn set_slot(tbl: &mut [u8; 256], idx: u8, val: u8) {
    if let Some(slot) = tbl.get_mut(usize::from(idx)) {
        *slot = val;
    }
}

/// Copies the first up-to-256 bytes of `src` into a `[u8; 256]` array.
///
/// # Arguments
///
/// * `src` - Source byte slice.
#[must_use]
fn slice_to_256(src: &[u8]) -> [u8; 256] {
    let mut out = [0_u8; 256];
    for (dst, &b) in out.iter_mut().zip(src.iter()) {
        *dst = b;
    }
    out
}

/// Computes the inverse lookup table of `tbl`, preserving the first preimage for any duplicate entries.
///
/// # Arguments
///
/// * `tbl` - Forward 256-byte mapping.
#[must_use]
fn invert_byte_table(tbl: &[u8; 256]) -> [u8; 256] {
    let mut inv = [0_u8; 256];
    let mut seen = [false; 256];
    for (src_idx, &dst_byte) in tbl.iter().enumerate() {
        let slot = usize::from(dst_byte);
        if let (Some(seen_flag), Some(inv_slot)) = (seen.get_mut(slot), inv.get_mut(slot))
            && !*seen_flag
        {
            *seen_flag = true;
            *inv_slot = u8::try_from(src_idx).unwrap_or(0);
        }
    }
    inv
}

/// Default `GnuCOBOL` EBCDIC-to-ASCII 256-byte bijective table (`0x40 -> ' '`, `0x6A -> '|'`, `0x5A -> '!'`).
pub const DEFAULT_EBCDIC_TO_ASCII: [u8; 256] = [
    0x00, 0x01, 0x02, 0x03, 0x9C, 0x09, 0x86, 0x7F, 0x97, 0x8D, 0x8E, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x9D, 0x85, 0x08, 0x87, 0x18, 0x19, 0x92, 0x8F, 0x1C, 0x1D, 0x1E, 0x1F,
    0x80, 0x81, 0x82, 0x83, 0x84, 0x0A, 0x17, 0x1B, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x05, 0x06, 0x07,
    0x90, 0x91, 0x16, 0x93, 0x94, 0x95, 0x96, 0x04, 0x98, 0x99, 0x9A, 0x9B, 0x14, 0x15, 0x9E, 0x1A,
    0x20, 0xA0, 0xE2, 0xE4, 0xE0, 0xE1, 0xE3, 0xE5, 0xE7, 0xF1, 0xA2, 0x2E, 0x3C, 0x28, 0x2B, 0xA6,
    0x26, 0xE9, 0xEA, 0xEB, 0xE8, 0xED, 0xEE, 0xEF, 0xEC, 0xDF, 0x21, 0x24, 0x2A, 0x29, 0x3B, 0xAC,
    0x2D, 0x2F, 0xC2, 0xC4, 0xC0, 0xC1, 0xC3, 0xC5, 0xC7, 0xD1, 0x7C, 0x2C, 0x25, 0x5F, 0x3E, 0x3F,
    0xF8, 0xC9, 0xCA, 0xCB, 0xC8, 0xCD, 0xCE, 0xCF, 0xCC, 0x60, 0x3A, 0x23, 0x40, 0x27, 0x3D, 0x22,
    0xD8, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0xAB, 0xBB, 0xF0, 0xFD, 0xFE, 0xB1,
    0xB0, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x71, 0x72, 0xAA, 0xBA, 0xE6, 0xB8, 0xC6, 0xA4,
    0xB5, 0x7E, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0xA1, 0xBF, 0xD0, 0xDD, 0xDE, 0xAE,
    0x5E, 0xA3, 0xA5, 0xB7, 0xA9, 0xA7, 0xB6, 0xBC, 0xBD, 0xBE, 0x5B, 0x5D, 0xAF, 0xA8, 0xB4, 0xD7,
    0x7B, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0xAD, 0xF4, 0xF6, 0xF2, 0xF3, 0xF5,
    0x7D, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0xB9, 0xFB, 0xFC, 0xF9, 0xFA, 0xFF,
    0x5C, 0xF7, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0xB2, 0xD4, 0xD6, 0xD2, 0xD3, 0xD5,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0xB3, 0xDB, 0xDC, 0xD9, 0xDA, 0x9F,
];
