//! Comprehensive C-ABI integration & branch-coverage tests for `coblang_rt::xml_json_oo_abi` (`M20`).

use std::ptr;

use coblang_common::{DigitCount, ScaleFactor};
use coblang_decimal::{OverpunchDialect, PackedSignPolicy};
use coblang_hir::{
    BinaryEndianness, HirDisplayStyle, HirType, JustifiedSpec, Signedness, TruncationPolicy,
};
use coblang_pic::MoveFieldDesc;
use coblang_rt::xml_json_oo_abi::{
    coblang_rt_m20_dec_push_numval, coblang_rt_m20_json_gen, coblang_rt_m20_json_parse,
    coblang_rt_m20_oo_begin_invoke, coblang_rt_m20_oo_invoke, coblang_rt_m20_oo_push_arg,
    coblang_rt_m20_oo_reg_class, coblang_rt_m20_oo_reg_method, coblang_rt_m20_oo_self_data_ptr,
    coblang_rt_m20_oo_self_handle, coblang_rt_m20_push_odo, coblang_rt_m20_reg_len,
    coblang_rt_m20_reg_ptr, coblang_rt_m20_set_count_dst, coblang_rt_m20_set_ns,
    coblang_rt_m20_xml_gen, coblang_rt_m20_xml_parse_after, coblang_rt_m20_xml_parse_begin,
    coblang_rt_m20_xml_parse_end, coblang_rt_m20_xml_parse_feed, coblang_rt_m20_xml_parse_next,
};
use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

/// Appends little-endian `u32` to `out`.
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Appends `u16`-prefixed string to `out`.
fn push_str16(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let len = u16::try_from(b.len()).unwrap_or(0);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(b);
}

/// Builds an alphanumeric `RuntimeFieldSpec` of `len` bytes.
fn alpha_spec(len: u32) -> Vec<u8> {
    encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc {
            ty: HirType::Alphanumeric {
                byte_len: len,
                justified: JustifiedSpec::Left,
            },
            decimal_char: b'.',
            currency_symbol: '$',
            overpunch: OverpunchDialect::AsciiGnu,
            display_style: HirDisplayStyle::IsoPicWidth,
        },
        packed_sign_rule: PackedSignPolicy::Gnu,
    })
}

/// Builds a 4-byte native signed `BinaryInt` `RuntimeFieldSpec`.
fn i32_spec() -> Vec<u8> {
    encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc {
            ty: HirType::BinaryInt {
                byte_width: 4,
                digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::TruncateToPic,
            },
            decimal_char: b'.',
            currency_symbol: '$',
            overpunch: OverpunchDialect::AsciiGnu,
            display_style: HirDisplayStyle::IsoPicWidth,
        },
        packed_sign_rule: PackedSignPolicy::Gnu,
    })
}

/// Encodes a simple 2-child group schema (`rec` -> `a: X(3)` at `0`, `n: i32` at `3`).
fn sample_group_schema() -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, 0);
    push_str16(&mut out, "rec");
    push_u32(&mut out, 0);
    push_u32(&mut out, 7);
    push_u32(&mut out, 0);
    out.extend_from_slice(&(-1_i32).to_le_bytes());
    out.push(1);
    push_u32(&mut out, 2);

    // Child 0: `a` PIC X(3)
    push_u32(&mut out, 1);
    push_str16(&mut out, "a");
    push_u32(&mut out, 0);
    push_u32(&mut out, 3);
    push_u32(&mut out, 0);
    out.extend_from_slice(&(-1_i32).to_le_bytes());
    out.push(0);
    out.push(0);
    let aspec = alpha_spec(3);
    push_u32(&mut out, u32::try_from(aspec.len()).unwrap_or(0));
    out.extend_from_slice(&aspec);

    // Child 1: `n` 4-byte i32
    push_u32(&mut out, 2);
    push_str16(&mut out, "n");
    push_u32(&mut out, 3);
    push_u32(&mut out, 4);
    push_u32(&mut out, 0);
    out.extend_from_slice(&(-1_i32).to_le_bytes());
    out.push(0);
    out.push(2);
    let nspec = i32_spec();
    push_u32(&mut out, u32::try_from(nspec.len()).unwrap_or(0));
    out.extend_from_slice(&nspec);
    out
}

/// Verifies all M20 special register pointers/lengths, XML/JSON generate, JSON parse, XML stream parse, and numval C-ABI entrypoints.
#[test]
#[expect(
    unsafe_code,
    reason = "Integration test exercising exported C-ABI functions."
)]
fn m20_registers_xml_json_and_numval_abi_roundtrip() {
    for id in 0..=11 {
        let p = coblang_rt_m20_reg_ptr(id);
        let l = coblang_rt_m20_reg_len(id);
        assert!(!p.is_null());
        assert!(l >= 0);
    }

    let mut rec = [0_u8; 7];
    rec[..3].copy_from_slice(b"Hi ");
    rec[3..7].copy_from_slice(&42_i32.to_ne_bytes());

    // XML GENERATE
    let mut xml_plan = vec![0_u8];
    push_u32(&mut xml_plan, 7);
    push_u32(&mut xml_plan, 0); // name overrides
    push_u32(&mut xml_plan, 0); // field suppress
    push_u32(&mut xml_plan, 0); // generic suppress
    push_u32(&mut xml_plan, 0); // type overrides
    xml_plan.extend_from_slice(&sample_group_schema());

    let mut cnt_buf = 0_i32.to_ne_bytes();
    let cspec = i32_spec();
    // SAFETY: Valid stack buffers.
    unsafe {
        coblang_rt_m20_set_ns(ptr::null(), -1, ptr::null(), -1);
        coblang_rt_m20_set_count_dst(
            cnt_buf.as_mut_ptr(),
            4,
            cspec.as_ptr(),
            i64::try_from(cspec.len()).unwrap_or(0),
        );
    }
    let mut xml_out = [b' '; 64];
    // SAFETY: Valid stack buffers.
    let xml_rc = unsafe {
        coblang_rt_m20_xml_gen(
            xml_out.as_mut_ptr(),
            64,
            rec.as_ptr(),
            xml_plan.as_ptr(),
            i64::try_from(xml_plan.len()).unwrap_or(0),
        )
    };
    assert_eq!(xml_rc, 0);
    assert!(String::from_utf8_lossy(&xml_out).starts_with("<rec><a>Hi</a><n>42</n></rec>"));
    assert_eq!(i32::from_ne_bytes(cnt_buf), 29);

    // JSON GENERATE + JSON PARSE roundtrip
    let mut jgen_plan = vec![0_u8];
    push_u32(&mut jgen_plan, 7);
    push_u32(&mut jgen_plan, 0);
    push_u32(&mut jgen_plan, 0);
    push_u32(&mut jgen_plan, 0);
    push_u32(&mut jgen_plan, 0);
    jgen_plan.extend_from_slice(&sample_group_schema());

    let mut json_out = [b' '; 64];
    // SAFETY: Valid stack buffers.
    unsafe {
        coblang_rt_m20_set_count_dst(ptr::null_mut(), 0, ptr::null(), 0);
    }
    // SAFETY: Valid stack buffers.
    let jgen_rc = unsafe {
        coblang_rt_m20_json_gen(
            json_out.as_mut_ptr(),
            64,
            rec.as_ptr(),
            jgen_plan.as_ptr(),
            i64::try_from(jgen_plan.len()).unwrap_or(0),
        )
    };
    assert_eq!(jgen_rc, 0);
    assert!(String::from_utf8_lossy(&json_out).starts_with(r#"{"rec":{"a":"Hi","n":42}}"#));

    let mut jparse_plan = vec![1_u8]; // WITH DETAIL
    push_u32(&mut jparse_plan, 7);
    push_u32(&mut jparse_plan, 0);
    push_u32(&mut jparse_plan, 0);
    push_u32(&mut jparse_plan, 0);
    jparse_plan.extend_from_slice(&sample_group_schema());

    let mut parsed_rec = [0_u8; 7];
    coblang_rt_m20_push_odo(99, 2);
    // SAFETY: Valid stack buffers.
    let jp_rc = unsafe {
        coblang_rt_m20_json_parse(
            json_out.as_ptr(),
            64,
            parsed_rec.as_mut_ptr(),
            jparse_plan.as_ptr(),
            i64::try_from(jparse_plan.len()).unwrap_or(0),
        )
    };
    assert_eq!(jp_rc, 0);
    assert_eq!(&parsed_rec[..3], b"Hi ");
    assert_eq!(
        i32::from_ne_bytes([parsed_rec[3], parsed_rec[4], parsed_rec[5], parsed_rec[6]]),
        42
    );

    // Streaming XML PARSE session + NUMVAL-C
    let fn_name = b"test.cob";
    // SAFETY: Valid literal slice.
    let sid = unsafe { coblang_rt_m20_xml_parse_begin(fn_name.as_ptr(), 8, 10, 0) };
    let doc = b"<root>hello</root>";
    // SAFETY: Valid literal slice.
    unsafe {
        coblang_rt_m20_xml_parse_feed(sid, doc.as_ptr(), 18);
    }
    let mut events = 0;
    while coblang_rt_m20_xml_parse_next(sid) != 0 {
        events += 1;
        let _ = coblang_rt_m20_xml_parse_after(sid);
    }
    assert_eq!(coblang_rt_m20_xml_parse_end(sid), 0);
    assert!(events >= 4);

    let price = b"$12.50";
    // SAFETY: Valid literal slice.
    assert_eq!(
        unsafe { coblang_rt_m20_dec_push_numval(price.as_ptr(), 6, 1) },
        0
    );
}

/// Extern "C" mock OO method incrementing 4-byte instance counter and writing it to returning slot.
#[expect(
    unsafe_code,
    reason = "Mock OO method callback invoked by `coblang_rt_m20_oo_invoke`."
)]
extern "C" fn mock_oo_inc_method() -> i32 {
    let self_ptr = coblang_rt_m20_oo_self_data_ptr();
    assert!(!self_ptr.is_null());
    assert!(coblang_rt_m20_oo_self_handle() > 0);
    // SAFETY: `self_ptr` points to 8-byte instance buffer registered below.
    unsafe {
        let cur = ptr::read_unaligned(self_ptr.cast::<i32>());
        ptr::write_unaligned(self_ptr.cast::<i32>(), cur + 10);
    }
    0
}

/// Verifies OO COBOL class registration, factory `new`, and instance method dispatch via C-ABI.
#[test]
#[expect(
    unsafe_code,
    reason = "Integration test exercising exported OO C-ABI entrypoints."
)]
fn m20_oo_vtable_alloc_and_dispatch() {
    let cls = b"CounterCls";
    let init = 5_i64.to_ne_bytes();
    let meth = b"addTen";
    let new_m = b"new";
    // SAFETY: Valid byte slices and `extern "C"` function pointer.
    unsafe {
        coblang_rt_m20_oo_reg_class(cls.as_ptr(), 10, ptr::null(), 0, init.as_ptr(), 8);
        coblang_rt_m20_oo_reg_method(
            cls.as_ptr(),
            10,
            1,
            meth.as_ptr(),
            6,
            mock_oo_inc_method as *const () as *const u8,
        );
    }

    let mut obj_handle: i64 = 0;
    coblang_rt_m20_oo_begin_invoke(ptr::addr_of_mut!(obj_handle).cast::<u8>(), 8);
    // SAFETY: Valid class & method slices.
    assert_eq!(
        unsafe { coblang_rt_m20_oo_invoke(0, 0, cls.as_ptr(), 10, new_m.as_ptr(), 3) },
        0
    );
    assert!(obj_handle > 0);

    coblang_rt_m20_oo_begin_invoke(ptr::null_mut(), 0);
    coblang_rt_m20_oo_push_arg(ptr::null_mut(), 0);
    // SAFETY: Valid method slice.
    assert_eq!(
        unsafe { coblang_rt_m20_oo_invoke(1, obj_handle, ptr::null(), 0, meth.as_ptr(), 6) },
        0
    );
}
