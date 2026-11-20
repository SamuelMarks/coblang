//! Clean-room `#[repr(C)]` runtime descriptor structures (`CobFieldAttr`, `CobField`,
//! `CobModule`, `CobDecimal`, `CobFile`) and safe conversion helpers.

use std::ffi::{c_char, c_void};

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::{DecimalSign, FixedDecimal, SignedInt256, Uint256};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, FloatBinaryWidth, HirType, JustifiedSpec,
    NumericSignSpec, Signedness, TruncationPolicy,
};
use coblang_pic::MoveFieldDesc;

/// `CobFieldAttr::field_type` tag for group items (`PIC X(n)` group semantics).
pub const COB_TYPE_GROUP: u8 = 0x01;
/// `CobFieldAttr::field_type` tag for boolean / bit items.
pub const COB_TYPE_BOOL: u8 = 0x02;
/// `CobFieldAttr::field_type` tag for `USAGE DISPLAY` zoned decimal items.
pub const COB_TYPE_NUMERIC_DISPLAY: u8 = 0x10;
/// `CobFieldAttr::field_type` tag for `USAGE BINARY` / `COMP` / `COMP-4` / `COMP-5` items.
pub const COB_TYPE_NUMERIC_BINARY: u8 = 0x11;
/// `CobFieldAttr::field_type` tag for `USAGE PACKED-DECIMAL` / `COMP-3` items.
pub const COB_TYPE_NUMERIC_PACKED: u8 = 0x12;
/// `CobFieldAttr::field_type` tag for `COMP-1` single-precision IEEE-754 float items.
pub const COB_TYPE_NUMERIC_FLOAT: u8 = 0x13;
/// `CobFieldAttr::field_type` tag for `COMP-2` double-precision IEEE-754 float items.
pub const COB_TYPE_NUMERIC_DOUBLE: u8 = 0x14;
/// `CobFieldAttr::field_type` tag for numeric-edited items.
pub const COB_TYPE_NUMERIC_EDITED: u8 = 0x24;
/// `CobFieldAttr::field_type` tag for alphanumeric items (`PIC X`).
pub const COB_TYPE_ALPHANUMERIC: u8 = 0x21;
/// `CobFieldAttr::field_type` tag for `ALL` figurative literal descriptors.
pub const COB_TYPE_ALPHANUMERIC_ALL: u8 = 0x22;
/// `CobFieldAttr::field_type` tag for alphanumeric-edited items.
pub const COB_TYPE_ALPHANUMERIC_EDITED: u8 = 0x23;
/// `CobFieldAttr::field_type` tag for `USAGE NATIONAL` UTF-16 items.
pub const COB_TYPE_NATIONAL: u8 = 0x40;
/// `CobFieldAttr::field_type` tag for national-edited items.
pub const COB_TYPE_NATIONAL_EDITED: u8 = 0x41;

/// `CobFieldAttr::flags` bit indicating an operational sign (`PIC S9...`).
pub const COB_FLAG_HAVE_SIGN: u8 = 0x01;
/// `CobFieldAttr::flags` bit indicating `SIGN IS {LEADING | TRAILING} SEPARATE`.
pub const COB_FLAG_SIGN_SEPARATE: u8 = 0x02;
/// `CobFieldAttr::flags` bit indicating `SIGN IS LEADING`.
pub const COB_FLAG_SIGN_LEADING: u8 = 0x04;
/// `CobFieldAttr::flags` bit indicating `BLANK WHEN ZERO`.
pub const COB_FLAG_BLANK_ZERO: u8 = 0x08;
/// `CobFieldAttr::flags` bit indicating `JUSTIFIED RIGHT`.
pub const COB_FLAG_JUSTIFIED: u8 = 0x10;
/// `CobFieldAttr::flags` bit indicating big-endian byte-swapped binary storage (`COMP` / `COMP-4`).
pub const COB_FLAG_BINARY_SWAP: u8 = 0x20;
/// `CobFieldAttr::flags` bit indicating full binary range without decimal `PICTURE` truncation (`COMP-5`).
pub const COB_FLAG_REAL_BINARY: u8 = 0x40;
/// `CobFieldAttr::flags` bit indicating a `USAGE POINTER` field.
pub const COB_FLAG_IS_POINTER: u8 = 0x80;

/// `cob_decimal_get_field` / `cob_add_int` option bit requesting `ROUNDED` nearest-away-from-zero.
pub const COB_STORE_ROUND: i32 = 0x01;
/// `cob_decimal_get_field` option bit preserving destination on `SIZE ERROR` overflow.
pub const COB_STORE_KEEP_ON_OVERFLOW: i32 = 0x02;
/// `cob_decimal_get_field` option bit allowing high-order modulo truncation without overflow status.
pub const COB_STORE_TRUNC_ON_OVERFLOW: i32 = 0x04;

/// Sentinel `CobDecimal::scale` marking NaN / division-by-zero / invalid decimal state.
pub const COB_DECIMAL_NAN: i32 = i32::MIN;

/// Clean-room C-ABI field attribute descriptor (`#[repr(C)]`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobFieldAttr {
    /// Field storage type code (`COB_TYPE_*`).
    pub field_type: u8,
    /// Declared decimal digit count.
    pub digits: u8,
    /// Signed decimal scale factor.
    pub scale: i16,
    /// Bitwise sign/justified/blank-zero/endian flags (`COB_FLAG_*`).
    pub flags: u8,
    /// Pointer to optional editing bytecode or null.
    pub pic_ptr: *const u8,
}

/// Clean-room C-ABI field view descriptor (`#[repr(C)]`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobField {
    /// Byte length of the referenced storage window.
    pub size: usize,
    /// Mutable pointer to raw field storage bytes.
    pub data: *mut u8,
    /// Pointer to static [`CobFieldAttr`] metadata.
    pub attr: *const CobFieldAttr,
}

/// Clean-room C-ABI active module frame descriptor (`#[repr(C)]`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobModule {
    /// Caller module link in the active run-unit call chain.
    pub next: *mut CobModule,
    /// NUL-terminated logical module name (`PROGRAM-ID`), or null.
    pub module_name: *const c_char,
    /// NUL-terminated compilation timestamp string, or null.
    pub module_formatted_date: *const c_char,
    /// NUL-terminated source filename string, or null.
    pub module_source: *const c_char,
    /// Module entrypoint code pointer, or null.
    pub module_entry: *const c_void,
    /// Pointer to caller-supplied `*mut CobField` parameter array, or null.
    pub cob_procedure_params: *const *mut CobField,
    /// Number of parameters passed by caller.
    pub module_num_params: i32,
    /// Dialect overpunch sign mode (`0` = ASCII/Gnu, `1` = EBCDIC/IBM).
    pub ebcdic_sign: u8,
    /// Active decimal point character (`b'.'` or `b','`).
    pub decimal_point: u8,
    /// Active currency symbol character (e.g. `b'$'`).
    pub currency_symbol: u8,
    /// Numeric group separator character (e.g. `b','` or `b'.'`).
    pub numeric_separator: u8,
    /// Environment filename mapping flag.
    pub flag_filename_mapping: u8,
    /// `BINARY` decimal truncation flag.
    pub flag_binary_truncate: u8,
    /// Pretty numeric `DISPLAY` formatting flag.
    pub flag_pretty_display: u8,
    /// Host operational sign flag.
    pub flag_host_sign: u8,
}

/// Clean-room C-ABI exact 256-bit (76-digit) fixed-point decimal register (`#[repr(C)]`, zero heap allocation).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CobDecimal {
    /// Four little-endian 64-bit magnitude limbs (`[l0, l1, l2, l3]`).
    pub limbs: [u64; 4],
    /// Sign indicator (`0` = zero, `1` = positive, `-1` = negative).
    pub sign: i32,
    /// Base-10 fractional scale (`-76..=76`, or [`COB_DECIMAL_NAN`] on domain error).
    pub scale: i32,
}

impl CobDecimal {
    /// Resets this decimal register to exact zero (`0 * 10^0`).
    pub const fn clear_zero(&mut self) {
        self.limbs = [0, 0, 0, 0];
        self.sign = 0;
        self.scale = 0;
    }

    /// Marks this decimal register as [`COB_DECIMAL_NAN`].
    pub const fn set_nan(&mut self) {
        self.limbs = [0, 0, 0, 0];
        self.sign = 0;
        self.scale = COB_DECIMAL_NAN;
    }

    /// Returns `true` when this decimal register holds [`COB_DECIMAL_NAN`].
    #[must_use]
    pub const fn is_nan(&self) -> bool {
        self.scale == COB_DECIMAL_NAN
    }

    /// Converts `self` into a [`FixedDecimal`], returning [`CoblangError::DecimalDomain`] if NaN or out of scale.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] when `self.is_nan()` or `scale` exceeds `-76..=76`.
    pub fn to_fixed_decimal(&self) -> Result<FixedDecimal> {
        if self.is_nan() {
            return Err(CoblangError::DecimalDomain {
                operation: "CobDecimal::to_fixed_decimal",
                message: String::from("decimal register is NaN"),
            });
        }
        let scale_i16 = i16::try_from(self.scale).map_err(|_| CoblangError::DecimalDomain {
            operation: "CobDecimal::to_fixed_decimal",
            message: format!("decimal scale {} overflows i16", self.scale),
        })?;
        let mag = Uint256::from_limbs(self.limbs);
        let dsign = if mag.is_zero() || self.sign == 0 {
            DecimalSign::UnsignedZero
        } else if self.sign < 0 {
            DecimalSign::Negative
        } else {
            DecimalSign::Positive
        };
        FixedDecimal::try_new(SignedInt256::from_parts(mag, dsign), scale_i16)
    }

    /// Populates `self` from `dec`, stripping redundant trailing fractional zeros.
    ///
    /// # Arguments
    ///
    /// * `dec` - Source [`FixedDecimal`] value.
    pub fn set_from_fixed_decimal(&mut self, dec: &FixedDecimal) {
        let mant = dec.mantissa();
        let mut mag = mant.magnitude();
        let mut scale = i32::from(dec.scale());
        let ten = Uint256::from_u64(10);
        while scale > 0 && !mag.is_zero() {
            let (q, r) = mag
                .checked_div_rem(ten)
                .unwrap_or((Uint256::ZERO, Uint256::ONE));
            if r.is_zero() {
                mag = q;
                scale -= 1;
            } else {
                break;
            }
        }
        if mag.is_zero() {
            self.clear_zero();
            return;
        }
        self.limbs = mag.limbs();
        self.sign = match mant.sign() {
            DecimalSign::UnsignedZero | DecimalSign::Positive => 1,
            DecimalSign::Negative => -1,
        };
        self.scale = scale;
    }
}

/// Clean-room C-ABI file connector descriptor (`#[repr(C)]`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobFile {
    /// NUL-terminated logical `SELECT` name, or null.
    pub select_name: *const c_char,
    /// Pointer to 2-byte `FILE STATUS` storage, or null.
    pub file_status: *mut u8,
    /// Pointer to `ASSIGN` filename [`CobField`], or null.
    pub assign: *mut CobField,
    /// Pointer to current record buffer [`CobField`], or null.
    pub record: *mut CobField,
    /// Pointer to `DEPENDING ON` record-length [`CobField`], or null.
    pub variable_record: *mut CobField,
    /// Minimum record byte length.
    pub record_min: usize,
    /// Maximum record byte length.
    pub record_max: usize,
    /// Underlying OS file descriptor (`-1` when closed).
    pub fd: i32,
    /// Organization code (`0` = sequential, `1` = line sequential, `2` = relative, `3` = indexed).
    pub organization: u8,
    /// Access mode code (`1` = sequential, `2` = random, `3` = dynamic).
    pub access_mode: u8,
    /// Lock mode flags.
    pub lock_mode: u8,
    /// Current open mode (`0` = closed, `1` = input, `2` = output, `3` = i-o, `4` = extend).
    pub open_mode: u8,
    /// Non-zero when declared `SELECT OPTIONAL`.
    pub flag_optional: u8,
}

/// Returns a default alphanumeric [`CobFieldAttr`] when `CobField::attr` is null.
#[must_use]
pub const fn default_alnum_attr() -> CobFieldAttr {
    CobFieldAttr {
        field_type: COB_TYPE_ALPHANUMERIC,
        digits: 0,
        scale: 0,
        flags: 0,
        pic_ptr: core::ptr::null(),
    }
}

/// Returns `true` when `field_type` represents an arithmetic numeric [`CobField`].
///
/// # Arguments
///
/// * `field_type` - `COB_TYPE_*` tag byte.
#[must_use]
pub const fn is_numeric_cob_type(field_type: u8) -> bool {
    matches!(
        field_type,
        COB_TYPE_NUMERIC_DISPLAY
            | COB_TYPE_NUMERIC_BINARY
            | COB_TYPE_NUMERIC_PACKED
            | COB_TYPE_NUMERIC_FLOAT
            | COB_TYPE_NUMERIC_DOUBLE
    )
}

/// Converts `(size, attr)` into a strongly-typed [`MoveFieldDesc`] for `coblang-pic` operations.
///
/// # Arguments
///
/// * `size` - Field byte length (`CobField::size`).
/// * `attr` - [`CobFieldAttr`] metadata.
#[must_use]
pub fn cob_attr_to_move_desc(size: usize, attr: CobFieldAttr) -> MoveFieldDesc {
    let signed = if (attr.flags & COB_FLAG_HAVE_SIGN) != 0 {
        Signedness::Signed
    } else {
        Signedness::Unsigned
    };
    let sign = match (
        (attr.flags & COB_FLAG_HAVE_SIGN) != 0,
        (attr.flags & COB_FLAG_SIGN_LEADING) != 0,
        (attr.flags & COB_FLAG_SIGN_SEPARATE) != 0,
    ) {
        (false, _, _) => NumericSignSpec::Unsigned,
        (true, false, false) => NumericSignSpec::EmbeddedTrailing,
        (true, true, false) => NumericSignSpec::EmbeddedLeading,
        (true, false, true) => NumericSignSpec::SeparateTrailing,
        (true, true, true) => NumericSignSpec::SeparateLeading,
    };
    let blank_zero = if (attr.flags & COB_FLAG_BLANK_ZERO) != 0 {
        BlankZeroSpec::BlankWhenZero
    } else {
        BlankZeroSpec::RetainZero
    };
    let justified = if (attr.flags & COB_FLAG_JUSTIFIED) != 0 {
        JustifiedSpec::Right
    } else {
        JustifiedSpec::Left
    };
    let raw_digits = if attr.digits == 0 {
        u8::try_from(size.clamp(1, 38)).unwrap_or(18)
    } else {
        attr.digits.clamp(1, 38)
    };
    let digits = DigitCount::try_new(raw_digits).unwrap_or(DigitCount::ONE);
    let clamped_scale = attr.scale.clamp(-38, 38);
    let scale = ScaleFactor::try_new(clamped_scale).unwrap_or(ScaleFactor::ZERO);
    let byte_len = u32::try_from(size).unwrap_or(u32::MAX);
    let byte_width = u8::try_from(size.clamp(1, 16)).unwrap_or(8);

    let ty = match attr.field_type {
        COB_TYPE_NUMERIC_DISPLAY => HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        },
        COB_TYPE_NUMERIC_BINARY => {
            let endian = if (attr.flags & COB_FLAG_BINARY_SWAP) != 0 {
                BinaryEndianness::BigEndian
            } else {
                BinaryEndianness::Native
            };
            let trunc = if (attr.flags & COB_FLAG_REAL_BINARY) != 0 {
                TruncationPolicy::FullBinaryRange
            } else {
                TruncationPolicy::TruncateToPic
            };
            HirType::BinaryInt {
                byte_width,
                digits,
                scale,
                sign: signed,
                endian,
                trunc,
            }
        }
        COB_TYPE_NUMERIC_PACKED => HirType::PackedDecimal {
            digits,
            scale,
            sign: signed,
            unsigned_comp6: Comp6Mode::StandardComp3,
        },
        COB_TYPE_NUMERIC_FLOAT => HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        },
        COB_TYPE_NUMERIC_DOUBLE => HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        },
        _ => HirType::Alphanumeric {
            byte_len,
            justified,
        },
    };

    MoveFieldDesc::standard_gnu(ty)
}

/// Validates a readable `*const CobField` pointer, returning `(bytes, MoveFieldDesc, CobFieldAttr)`.
///
/// # Arguments
///
/// * `field_ptr` - Candidate `*const CobField`.
/// * `symbol` - Exporting C-ABI symbol name for diagnostics.
/// * `param_name` - Parameter name for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::AbiNullOrInvalidPointer`] if `field_ptr` is null or `size > 0 && data.is_null()`.
///
/// # Safety
///
/// When non-null, `field_ptr` must point to an initialized [`CobField`], `attr` (if non-null) to a
/// [`CobFieldAttr`], and `data` (if `size > 0` and non-null) to `size` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited validation and borrowing of `#[repr(C)]` `CobField` read view."
)]
pub unsafe fn validate_cob_field_read<'a>(
    field_ptr: *const CobField,
    symbol: &'static str,
    param_name: &'static str,
) -> Result<(&'a [u8], MoveFieldDesc, CobFieldAttr)> {
    if field_ptr.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    }
    // SAFETY: Non-null `field_ptr` guaranteed valid by caller contract.
    let field = unsafe { *field_ptr };
    let attr = if field.attr.is_null() {
        default_alnum_attr()
    } else {
        // SAFETY: Non-null `field.attr` points to a valid `CobFieldAttr`.
        unsafe { *field.attr }
    };
    let bytes: &'a [u8] = if field.size == 0 {
        &[]
    } else if field.data.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    } else {
        // SAFETY: Validated `field.size > 0` and non-null `field.data`.
        unsafe { core::slice::from_raw_parts(field.data.cast_const(), field.size) }
    };
    Ok((bytes, cob_attr_to_move_desc(field.size, attr), attr))
}

/// Validates a writable `*mut CobField` pointer, returning `(bytes_mut, MoveFieldDesc, CobFieldAttr)`.
///
/// # Arguments
///
/// * `field_ptr` - Candidate `*mut CobField`.
/// * `symbol` - Exporting C-ABI symbol name for diagnostics.
/// * `param_name` - Parameter name for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::AbiNullOrInvalidPointer`] if `field_ptr` is null or `size > 0 && data.is_null()`.
///
/// # Safety
///
/// When non-null, `field_ptr` must point to an initialized [`CobField`], `attr` (if non-null) to a
/// [`CobFieldAttr`], and `data` (if `size > 0` and non-null) to `size` writable bytes not aliased elsewhere.
#[expect(
    unsafe_code,
    reason = "Audited validation and borrowing of `#[repr(C)]` `CobField` mutable view."
)]
pub unsafe fn validate_cob_field_write<'a>(
    field_ptr: *mut CobField,
    symbol: &'static str,
    param_name: &'static str,
) -> Result<(&'a mut [u8], MoveFieldDesc, CobFieldAttr)> {
    if field_ptr.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    }
    // SAFETY: Non-null `field_ptr` guaranteed valid by caller contract.
    let field = unsafe { *field_ptr };
    let attr = if field.attr.is_null() {
        default_alnum_attr()
    } else {
        // SAFETY: Non-null `field.attr` points to a valid `CobFieldAttr`.
        unsafe { *field.attr }
    };
    let bytes: &'a mut [u8] = if field.size == 0 {
        &mut []
    } else if field.data.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    } else {
        // SAFETY: Validated `field.size > 0` and non-null `field.data`.
        unsafe { core::slice::from_raw_parts_mut(field.data, field.size) }
    };
    Ok((bytes, cob_attr_to_move_desc(field.size, attr), attr))
}
