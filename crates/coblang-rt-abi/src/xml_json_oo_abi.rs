//! Audited clean-room C-ABI entrypoints for Milestone M20 (`XML GENERATE`/`PARSE`, `JSON GENERATE`/`PARSE`,
//! `XML-*`/`JSON-*` special registers, and Object-Oriented COBOL `CLASS-ID` / `INVOKE` dispatch).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::sync::{Mutex, OnceLock};

use coblang_decimal::{FixedDecimal, HighOrderPolicy, RoundingMode};
use coblang_hir::HirType;
use coblang_intrinsics::strings::{CurrencyCasePolicy, str_numval, str_numval_c};
use coblang_pic::{
    DecimalPointKind as IntrDecPoint, MoveFieldDesc, MoveSource, de_edit_to_decimal,
    decode_numeric_field, execute_move, store_decimal_to_field,
};
use coblang_rt_core::desc_codec::decode_field_spec;
use coblang_rt_core::with_global_runtime;
use coblang_xml_json::cobol_runtime::{
    CobolGenCategory, CobolGenNode, CobolGenNodeKind, CobolGenericSuppress, CobolJsonGenOptions,
    CobolSuppressMask, CobolXmlDeclMode, CobolXmlGenOptions, CobolXmlPostAction,
    CobolXmlStreamMode, CobolXmlStreamParser, DecimalSepMode, JustifiedTrim,
    SuppressPlacementFilter, cobol_json_generate_into_buffer, cobol_xml_generate_into_buffer,
};
use coblang_xml_json::{
    CobolFieldName, FieldAssignState, JsonBooleanMapping, JsonDetailMode, JsonParseOptions,
    JsonStatus, NameMapping, NameOverride, ParseTargetKind, ParseTargetNode,
    SuppressCategoryFilter, ValueCategory, XmlAttributeMode, XmlNodePlacement, json_parse,
};

use crate::{ActiveModuleFrame, with_c_abi_state};

/// Register ID for `XML-CODE` (`4`-byte native `i32`).
pub const M20_REG_XML_CODE: i32 = 0;
/// Register ID for `XML-INFORMATION` (`4`-byte native `i32`).
pub const M20_REG_XML_INFORMATION: i32 = 1;
/// Register ID for `XML-EVENT` (`30`-byte space-padded ASCII).
pub const M20_REG_XML_EVENT: i32 = 2;
/// Register ID for `XML-TEXT` (dynamic-length UTF-8/ASCII buffer).
pub const M20_REG_XML_TEXT: i32 = 3;
/// Register ID for `XML-NTEXT` (dynamic-length UTF-16LE buffer).
pub const M20_REG_XML_NTEXT: i32 = 4;
/// Register ID for `XML-NAMESPACE` (dynamic-length URI buffer).
pub const M20_REG_XML_NAMESPACE: i32 = 5;
/// Register ID for `XML-NNAMESPACE` (dynamic-length UTF-16LE URI buffer).
pub const M20_REG_XML_NNAMESPACE: i32 = 6;
/// Register ID for `XML-NAMESPACE-PREFIX` (dynamic-length prefix buffer).
pub const M20_REG_XML_NAMESPACE_PREFIX: i32 = 7;
/// Register ID for `XML-NNAMESPACE-PREFIX` (dynamic-length UTF-16LE prefix buffer).
pub const M20_REG_XML_NNAMESPACE_PREFIX: i32 = 8;
/// Register ID for `JSON-CODE` (`4`-byte native `i32`).
pub const M20_REG_JSON_CODE: i32 = 9;
/// Register ID for `JSON-STATUS` (`4`-byte native `i32`).
pub const M20_REG_JSON_STATUS: i32 = 10;

/// Non-null 1-byte sentinel returned when a dynamic `XML-*` register has length `0`.
static EMPTY_SENTINEL: u8 = b' ';

/// Heap-pinned fixed registers whose raw pointers remain stable across the entire run unit.
struct PinnedFixedRegisters {
    /// Stable 4-byte native `i32` backing `XML-CODE`.
    xml_code: Box<[u8; 4]>,
    /// Stable 4-byte native `i32` backing `XML-INFORMATION`.
    xml_info: Box<[u8; 4]>,
    /// Stable 30-byte ASCII buffer backing `XML-EVENT`.
    xml_event: Box<[u8; 30]>,
    /// Stable 4-byte native `i32` backing `JSON-CODE`.
    json_code: Box<[u8; 4]>,
    /// Stable 4-byte native `i32` backing `JSON-STATUS`.
    json_status: Box<[u8; 4]>,
}

impl Default for PinnedFixedRegisters {
    /// Returns zero-initialized `XML-CODE`, `XML-INFORMATION`, `JSON-CODE`, `JSON-STATUS`, and blank `XML-EVENT`.
    fn default() -> Self {
        Self {
            xml_code: Box::new(0_i32.to_ne_bytes()),
            xml_info: Box::new(0_i32.to_ne_bytes()),
            xml_event: Box::new([b' '; 30]),
            json_code: Box::new(0_i32.to_ne_bytes()),
            json_status: Box::new(0_i32.to_ne_bytes()),
        }
    }
}

/// Heap-pinned dynamic register buffer retaining older allocations so interior pointers stay valid.
#[derive(Default)]
struct DynamicRegBuffer {
    /// Active buffer slice (`ptr` + `len`).
    active: Box<[u8]>,
    /// Retired allocations retained for run-unit pointer stability.
    retired: Vec<Box<[u8]>>,
}

impl DynamicRegBuffer {
    /// Replaces active contents with `bytes`.
    ///
    /// # Arguments
    ///
    /// * `bytes` - New register byte payload.
    fn set(&mut self, bytes: &[u8]) {
        let old = std::mem::replace(&mut self.active, bytes.to_vec().into_boxed_slice());
        if !old.is_empty() {
            self.retired.push(old);
        }
    }

    /// Returns a stable non-null pointer for the current contents.
    fn as_ptr(&self) -> *mut u8 {
        if self.active.is_empty() {
            core::ptr::addr_of!(EMPTY_SENTINEL).cast_mut()
        } else {
            self.active.as_ptr().cast_mut()
        }
    }

    /// Returns the current active byte length.
    fn len(&self) -> usize {
        self.active.len()
    }
}

/// Staged `COUNT IN` destination descriptor and buffer pointer.
struct StagedCountTarget {
    /// Destination field pointer.
    ptr: usize,
    /// Destination field byte length.
    len: usize,
    /// Decoded `MoveFieldDesc`.
    desc: MoveFieldDesc,
}

/// Registered OO class runtime entry.
struct RegisteredOoClass {
    /// Ordered immediate superclass external names.
    parents: Vec<String>,
    /// Initial instance byte template for `"new"`.
    init_template: Vec<u8>,
    /// Method function pointers keyed by `(comp: i32, UPPERCASE_METHOD_NAME)`.
    methods: BTreeMap<(i32, String), usize>,
}

/// Allocated OO instance or factory singleton entry.
struct OoHeapInstance {
    /// Owning external class name.
    class_ext: String,
    /// Compartment (`0` = Factory singleton, `1` = Object instance).
    comp: i32,
    /// Pinned instance `WORKING-STORAGE` buffer.
    storage: Box<[u8]>,
}

/// Complete run-unit M20 state guarded by a process-wide mutex.
#[derive(Default)]
struct M20RuntimeState {
    /// Fixed-width special registers.
    fixed: PinnedFixedRegisters,
    /// Dynamic `XML-TEXT`.
    xml_text: DynamicRegBuffer,
    /// Dynamic `XML-NTEXT` (UTF-16LE).
    xml_ntext: DynamicRegBuffer,
    /// Dynamic `XML-NAMESPACE`.
    xml_ns: DynamicRegBuffer,
    /// Dynamic `XML-NNAMESPACE` (UTF-16LE).
    xml_nns: DynamicRegBuffer,
    /// Dynamic `XML-NAMESPACE-PREFIX`.
    xml_pfx: DynamicRegBuffer,
    /// Dynamic `XML-NNAMESPACE-PREFIX` (UTF-16LE).
    xml_npfx: DynamicRegBuffer,
    /// Staged `NAMESPACE` bytes for next `XML GENERATE`.
    staged_ns: Option<Vec<u8>>,
    /// Staged `NAMESPACE-PREFIX` bytes for next `XML GENERATE`.
    staged_pfx: Option<Vec<u8>>,
    /// Staged `COUNT IN` target for next `XML`/`JSON GENERATE`.
    staged_count: Option<StagedCountTarget>,
    /// Staged dynamic ODO counts keyed by `DataItemId`.
    staged_odo: BTreeMap<u32, usize>,
    /// Active streaming `XML PARSE` sessions keyed by `session_id`.
    xml_sessions: BTreeMap<i32, CobolXmlStreamParser>,
    /// Next `XML PARSE` session ID (`>= 1`).
    next_session_id: i32,
    /// Registered OO classes keyed by uppercase external class name.
    oo_classes: BTreeMap<String, RegisteredOoClass>,
    /// Allocated OO instances & factory singletons keyed by `handle >= 1`.
    oo_instances: BTreeMap<i64, OoHeapInstance>,
    /// Factory singleton handle cache keyed by uppercase external class name.
    oo_factory_handles: BTreeMap<String, i64>,
    /// Next OO handle (`>= 1`).
    next_oo_handle: i64,
    /// Active `(self_handle, self_data_ptr)` call stack across nested `INVOKE`s.
    oo_self_stack: Vec<(i64, usize)>,
    /// Staged `(arg_ptr, arg_len)` arguments for next `INVOKE`.
    oo_staged_args: Vec<(usize, usize)>,
    /// Staged `(ret_ptr, ret_len)` return buffer for next `INVOKE`.
    oo_staged_ret: Option<(usize, usize)>,
}

impl M20RuntimeState {
    /// Reads a 4-byte native `i32` register.
    ///
    /// # Arguments
    ///
    /// * `buf` - 4-byte native-endian register array.
    fn get_i32_reg(buf: [u8; 4]) -> i32 {
        i32::from_ne_bytes(buf)
    }

    /// Writes a 4-byte native `i32` register.
    ///
    /// # Arguments
    ///
    /// * `buf` - Destination 4-byte native-endian register array.
    /// * `val` - Signed 32-bit integer value to store.
    fn set_i32_reg(buf: &mut [u8; 4], val: i32) {
        *buf = val.to_ne_bytes();
    }

    /// Writes a space-padded 30-byte `XML-EVENT` string.
    ///
    /// # Arguments
    ///
    /// * `name` - Event name text (`"START-OF-ELEMENT"`, etc.).
    fn set_xml_event(&mut self, name: &str) {
        self.fixed.xml_event.fill(b' ');
        for (d, &s) in self.fixed.xml_event.iter_mut().zip(name.as_bytes().iter()) {
            *d = s;
        }
    }

    /// Updates UTF-8 and UTF-16LE dynamic register pairs.
    ///
    /// # Arguments
    ///
    /// * `utf8_buf` - UTF-8 dynamic register buffer.
    /// * `utf16_buf` - UTF-16LE dynamic register buffer.
    /// * `s` - New text value.
    fn set_text_pair(utf8_buf: &mut DynamicRegBuffer, utf16_buf: &mut DynamicRegBuffer, s: &str) {
        utf8_buf.set(s.as_bytes());
        let mut u16_bytes = Vec::with_capacity(s.len() * 2);
        for cu in s.encode_utf16() {
            u16_bytes.extend_from_slice(&cu.to_le_bytes());
        }
        utf16_buf.set(&u16_bytes);
    }
}

/// Accesses the singleton [`M20RuntimeState`].
///
/// # Arguments
///
/// * `f` - Closure mutating [`M20RuntimeState`].
fn with_m20_state<R, F: FnOnce(&mut M20RuntimeState) -> R>(f: F) -> R {
    /// Lazily initialized M20 runtime state mutex.
    static STATE: OnceLock<Mutex<M20RuntimeState>> = OnceLock::new();
    let mtx = STATE.get_or_init(|| Mutex::new(M20RuntimeState::default()));
    let mut guard = match mtx.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    f(&mut guard)
}

/// Safely reads `(ptr, len)` into `&[u8]`.
///
/// # Arguments
///
/// * `ptr` - Raw byte pointer.
/// * `len` - Byte length (`<= 0` yields `&[]`).
///
/// # Safety
/// When `len > 0` and `!ptr.is_null()`, `ptr` must point to `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Internal null-checked slice conversion helper for M20 C-ABI entrypoints."
)]
unsafe fn slice_in<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    let Ok(n) = usize::try_from(len) else {
        return &[];
    };
    if n == 0 || ptr.is_null() {
        &[]
    } else {
        // SAFETY: Caller guarantees `ptr` has `n` readable bytes.
        unsafe { core::slice::from_raw_parts(ptr, n) }
    }
}

/// Safely reads `(ptr, len)` into `&mut [u8]`.
///
/// # Arguments
///
/// * `ptr` - Raw mutable byte pointer.
/// * `len` - Byte length (`<= 0` yields `&mut []`).
///
/// # Safety
/// When `len > 0` and `!ptr.is_null()`, `ptr` must point to `len` writable bytes.
#[expect(
    unsafe_code,
    reason = "Internal null-checked mutable slice helper for M20 C-ABI entrypoints."
)]
unsafe fn slice_out<'a>(ptr: *mut u8, len: i64) -> &'a mut [u8] {
    let Ok(n) = usize::try_from(len) else {
        return &mut [];
    };
    if n == 0 || ptr.is_null() {
        &mut []
    } else {
        // SAFETY: Caller guarantees `ptr` has `n` writable bytes.
        unsafe { core::slice::from_raw_parts_mut(ptr, n) }
    }
}

/// Returns the non-null data pointer for an M20 `XML-*` / `JSON-*` special register.
///
/// # Arguments
/// * `reg_id` - One of `M20_REG_*` (`0..=10`).
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_reg_ptr` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_reg_ptr(reg_id: i32) -> *mut u8 {
    with_m20_state(|st| match reg_id {
        M20_REG_XML_CODE => st.fixed.xml_code.as_mut_ptr(),
        M20_REG_XML_INFORMATION => st.fixed.xml_info.as_mut_ptr(),
        M20_REG_XML_EVENT => st.fixed.xml_event.as_mut_ptr(),
        M20_REG_XML_TEXT => st.xml_text.as_ptr(),
        M20_REG_XML_NTEXT => st.xml_ntext.as_ptr(),
        M20_REG_XML_NAMESPACE => st.xml_ns.as_ptr(),
        M20_REG_XML_NNAMESPACE => st.xml_nns.as_ptr(),
        M20_REG_XML_NAMESPACE_PREFIX => st.xml_pfx.as_ptr(),
        M20_REG_XML_NNAMESPACE_PREFIX => st.xml_npfx.as_ptr(),
        M20_REG_JSON_CODE => st.fixed.json_code.as_mut_ptr(),
        M20_REG_JSON_STATUS => st.fixed.json_status.as_mut_ptr(),
        _ => core::ptr::addr_of!(EMPTY_SENTINEL).cast_mut(),
    })
}

/// Returns the active byte length of an M20 `XML-*` / `JSON-*` special register.
///
/// # Arguments
/// * `reg_id` - One of `M20_REG_*` (`0..=10`).
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_reg_len` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_reg_len(reg_id: i32) -> i64 {
    with_m20_state(|st| {
        let n = match reg_id {
            M20_REG_XML_CODE
            | M20_REG_XML_INFORMATION
            | M20_REG_JSON_CODE
            | M20_REG_JSON_STATUS => 4,
            M20_REG_XML_EVENT => 30,
            M20_REG_XML_TEXT => st.xml_text.len(),
            M20_REG_XML_NTEXT => st.xml_ntext.len(),
            M20_REG_XML_NAMESPACE => st.xml_ns.len(),
            M20_REG_XML_NNAMESPACE => st.xml_nns.len(),
            M20_REG_XML_NAMESPACE_PREFIX => st.xml_pfx.len(),
            M20_REG_XML_NNAMESPACE_PREFIX => st.xml_npfx.len(),
            _ => 0,
        };
        i64::try_from(n).unwrap_or(0)
    })
}

/// Stages optional `NAMESPACE` and `NAMESPACE-PREFIX` operands for the next `XML GENERATE`.
///
/// # Arguments
/// * `ns_ptr` - `NAMESPACE` bytes pointer.
/// * `ns_len` - `NAMESPACE` byte length (`< 0` when clause omitted).
/// * `pfx_ptr` - `NAMESPACE-PREFIX` bytes pointer.
/// * `pfx_len` - `NAMESPACE-PREFIX` byte length (`< 0` when clause omitted).
///
/// # Safety
/// Non-negative lengths with non-null pointers must reference valid readable byte slices.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_set_ns` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_set_ns(
    ns_ptr: *const u8,
    ns_len: i64,
    pfx_ptr: *const u8,
    pfx_len: i64,
) {
    // SAFETY: Validated by caller per `# Safety`.
    let ns_opt = (ns_len >= 0).then(|| unsafe { slice_in(ns_ptr, ns_len) }.to_vec());
    // SAFETY: Validated by caller per `# Safety`.
    let pfx_opt = (pfx_len >= 0).then(|| unsafe { slice_in(pfx_ptr, pfx_len) }.to_vec());
    with_m20_state(|st| {
        st.staged_ns = ns_opt;
        st.staged_pfx = pfx_opt;
    });
}

/// Stages optional `COUNT IN` destination field for the next `XML GENERATE` or `JSON GENERATE`.
///
/// # Arguments
/// * `cnt_ptr` - Destination field pointer (`null` clears staged target).
/// * `cnt_len` - Destination field byte length.
/// * `desc_ptr` - Serialized `RuntimeFieldSpec` pointer.
/// * `desc_len` - Serialized `RuntimeFieldSpec` length.
///
/// # Safety
/// Non-null pointers with positive lengths must reference valid readable/writable buffers.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_set_count_dst` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_set_count_dst(
    cnt_ptr: *mut u8,
    cnt_len: i64,
    desc_ptr: *const u8,
    desc_len: i64,
) {
    if cnt_ptr.is_null() || cnt_len <= 0 {
        with_m20_state(|st| st.staged_count = None);
        return;
    }
    // SAFETY: Validated by caller per `# Safety`.
    let desc_bytes = unsafe { slice_in(desc_ptr, desc_len) };
    let Ok(spec) = decode_field_spec(desc_bytes) else {
        with_m20_state(|st| st.staged_count = None);
        return;
    };
    let len = usize::try_from(cnt_len).unwrap_or(0);
    with_m20_state(|st| {
        st.staged_count = Some(StagedCountTarget {
            ptr: cnt_ptr as usize,
            len,
            desc: spec.desc,
        });
    });
}

/// Records a runtime `OCCURS ... DEPENDING ON` count for `item_id` prior to `XML`/`JSON` execution.
///
/// # Arguments
/// * `item_id` - `DataItemId` (`u32` encoded as `i32`).
/// * `count_val` - Evaluated dynamic occurrence count.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_push_odo` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_push_odo(item_id: i32, count_val: i64) {
    if let Ok(uid) = u32::try_from(item_id) {
        let cnt = usize::try_from(count_val.max(0)).unwrap_or(0);
        with_m20_state(|st| {
            st.staged_odo.insert(uid, cnt);
        });
    }
}

/// Decoded binary tree schema node for `XML`/`JSON` record traversal.
#[derive(Clone)]
struct SchemaNode {
    /// `DataItemId`.
    field_id: u32,
    /// Original COBOL field name.
    raw_name: String,
    /// Byte offset from enclosing group/record start.
    rel_offset: usize,
    /// Single-occurrence byte length.
    elem_len: usize,
    /// Maximum static `OCCURS` count (`0` when scalar).
    max_occurs: usize,
    /// Optional ODO `DataItemId` (`None` when static).
    odo_id: Option<u32>,
    /// Payload (`Elementary` vs `Group`).
    payload: SchemaPayload,
}

/// Leaf vs group payload of [`SchemaNode`].
#[derive(Clone)]
enum SchemaPayload {
    /// Elementary field with category tag and runtime `MoveFieldDesc`.
    Elementary {
        /// `0` = `Alpha`, `1` = `AlphaRight`, `2` = `Numeric`, `3` = `NumericEdited`.
        cat_tag: u8,
        /// Decoded `MoveFieldDesc`.
        desc: MoveFieldDesc,
    },
    /// Group with ordered child nodes.
    Group(Vec<SchemaNode>),
}

/// Binary cursor for decoding M20 schema plans.
struct PlanReader<'a> {
    /// Plan bytes.
    buf: &'a [u8],
    /// Read offset.
    pos: usize,
}

impl<'a> PlanReader<'a> {
    /// Reads one `u8`.
    fn u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// Reads little-endian `u16`.
    fn u16(&mut self) -> Option<u16> {
        let b0 = self.u8()?;
        let b1 = self.u8()?;
        Some(u16::from_le_bytes([b0, b1]))
    }

    /// Reads little-endian `u32`.
    fn u32(&mut self) -> Option<u32> {
        let b0 = self.u8()?;
        let b1 = self.u8()?;
        let b2 = self.u8()?;
        let b3 = self.u8()?;
        Some(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    /// Reads little-endian `i32`.
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.u32()?.to_le_bytes()))
    }

    /// Reads a length-prefixed UTF-8 string (`u16` length).
    fn str16(&mut self) -> Option<String> {
        let len = usize::from(self.u16()?);
        let sub = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(String::from_utf8_lossy(sub).into_owned())
    }

    /// Reads a length-prefixed byte slice (`u32` length).
    fn bytes32(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        let sub = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(sub)
    }

    /// Decodes a recursive [`SchemaNode`].
    fn node(&mut self) -> Option<SchemaNode> {
        let field_id = self.u32()?;
        let raw_name = self.str16()?;
        let rel_offset = usize::try_from(self.u32()?).ok()?;
        let elem_len = usize::try_from(self.u32()?).ok()?;
        let max_occurs = usize::try_from(self.u32()?).ok()?;
        let odo_raw = self.i32()?;
        let odo_id = u32::try_from(odo_raw).ok();
        let tag = self.u8()?;
        let payload = if tag == 0 {
            let cat_tag = self.u8()?;
            let spec_bytes = self.bytes32()?;
            let spec = decode_field_spec(spec_bytes).ok()?;
            SchemaPayload::Elementary {
                cat_tag,
                desc: spec.desc,
            }
        } else {
            let count = usize::try_from(self.u32()?).ok()?;
            let mut children = Vec::with_capacity(count);
            for _ in 0..count {
                children.push(self.node()?);
            }
            SchemaPayload::Group(children)
        };
        Some(SchemaNode {
            field_id,
            raw_name,
            rel_offset,
            elem_len,
            max_occurs,
            odo_id,
            payload,
        })
    }
}

/// Materializes a [`CobolGenNode`] tree from live memory `record_bytes` + `schema`.
///
/// # Arguments
///
/// * `schema` - Decoded record schema node.
/// * `record_bytes` - Live COBOL record byte slice.
/// * `base_off` - Parent byte offset within `record_bytes`.
/// * `odo_map` - Dynamic `OCCURS DEPENDING ON` counts keyed by `DataItemId`.
/// * `bool_map` - Boolean literal mappings keyed by `field_id`.
/// * `is_root_ref` - `true` for the top-level root reference.
fn materialize_gen_node(
    schema: &SchemaNode,
    record_bytes: &[u8],
    base_off: usize,
    odo_map: &BTreeMap<u32, usize>,
    bool_map: &BTreeMap<u32, String>,
    is_root_ref: bool,
) -> CobolGenNode {
    let start = if is_root_ref {
        0
    } else {
        base_off.saturating_add(schema.rel_offset)
    };
    if !is_root_ref && schema.max_occurs > 0 {
        let active = schema
            .odo_id
            .and_then(|id| odo_map.get(&id).copied())
            .unwrap_or(schema.max_occurs)
            .min(schema.max_occurs);
        let mut occurrences = Vec::with_capacity(active);
        for idx in 0..active {
            let occ_base = start.saturating_add(idx.saturating_mul(schema.elem_len));
            occurrences.push(materialize_single_occurrence(
                schema,
                record_bytes,
                occ_base,
                odo_map,
                bool_map,
            ));
        }
        return CobolGenNode {
            field_id: schema.field_id,
            raw_name: schema.raw_name.clone(),
            kind: CobolGenNodeKind::Table { occurrences },
        };
    }
    materialize_single_occurrence(schema, record_bytes, start, odo_map, bool_map)
}

/// Materializes a single scalar/occurrence of `schema` starting at `start`.
///
/// # Arguments
///
/// * `schema` - Decoded record schema node.
/// * `record_bytes` - Live COBOL record byte slice.
/// * `start` - Starting byte offset of this occurrence.
/// * `odo_map` - Dynamic `OCCURS DEPENDING ON` counts.
/// * `bool_map` - Boolean literal mappings keyed by `field_id`.
fn materialize_single_occurrence(
    schema: &SchemaNode,
    record_bytes: &[u8],
    start: usize,
    odo_map: &BTreeMap<u32, usize>,
    bool_map: &BTreeMap<u32, String>,
) -> CobolGenNode {
    match &schema.payload {
        SchemaPayload::Elementary { cat_tag, desc } => {
            let end = start.saturating_add(schema.elem_len);
            let raw = record_bytes
                .get(start..end)
                .map_or_else(|| vec![b' '; schema.elem_len], <[u8]>::to_vec);
            if let Some(true_lit) = bool_map.get(&schema.field_id) {
                let text = String::from_utf8_lossy(&raw);
                let is_true = text.trim() == true_lit.trim();
                return CobolGenNode {
                    field_id: schema.field_id,
                    raw_name: schema.raw_name.clone(),
                    kind: CobolGenNodeKind::Elementary {
                        category: CobolGenCategory::Boolean,
                        formatted: String::from(if is_true { "true" } else { "false" }),
                        raw_bytes: raw,
                    },
                };
            }
            let (category, formatted) = decode_leaf_for_gen(*cat_tag, desc, &raw);
            CobolGenNode {
                field_id: schema.field_id,
                raw_name: schema.raw_name.clone(),
                kind: CobolGenNodeKind::Elementary {
                    category,
                    formatted,
                    raw_bytes: raw,
                },
            }
        }
        SchemaPayload::Group(children) => {
            let out_children = children
                .iter()
                .map(|ch| materialize_gen_node(ch, record_bytes, start, odo_map, bool_map, false))
                .collect();
            CobolGenNode {
                field_id: schema.field_id,
                raw_name: schema.raw_name.clone(),
                kind: CobolGenNodeKind::Group {
                    children: out_children,
                },
            }
        }
    }
}

/// Decodes a single elementary byte slice into `(CobolGenCategory, formatted_string)`.
///
/// # Arguments
///
/// * `cat_tag` - Wire category tag (`1=RightTrimAlpha, 2=Numeric, 3=NumericEdited`).
/// * `desc` - Decoded [`MoveFieldDesc`] of the elementary field.
/// * `raw` - Raw field byte slice.
fn decode_leaf_for_gen(
    cat_tag: u8,
    desc: &MoveFieldDesc,
    raw: &[u8],
) -> (CobolGenCategory, String) {
    match cat_tag {
        1 => (
            CobolGenCategory::Alphanumeric(JustifiedTrim::Right),
            String::from_utf8_lossy(raw).into_owned(),
        ),
        2 => {
            if matches!(desc.ty, HirType::Index) {
                let idx_val = match raw.len() {
                    8 => {
                        let mut b = [0_u8; 8];
                        b.copy_from_slice(raw);
                        i64::from_ne_bytes(b)
                    }
                    4 => {
                        let mut b = [0_u8; 4];
                        b.copy_from_slice(raw);
                        i64::from(i32::from_ne_bytes(b))
                    }
                    _ => 0,
                };
                (CobolGenCategory::Numeric, idx_val.to_string())
            } else {
                let formatted = decode_numeric_field(raw, desc)
                    .map_or_else(|_| String::from("0"), |d| d.to_string());
                (CobolGenCategory::Numeric, formatted)
            }
        }
        3 => (
            CobolGenCategory::NumericEdited,
            String::from_utf8_lossy(raw).into_owned(),
        ),
        _ => (
            CobolGenCategory::Alphanumeric(JustifiedTrim::Standard),
            String::from_utf8_lossy(raw).into_owned(),
        ),
    }
}

/// Stores `count` into a staged `COUNT IN` field if present.
///
/// # Arguments
///
/// * `target` - Optional staged `COUNT IN` destination descriptor.
/// * `count` - Character/byte count to store.
#[expect(
    unsafe_code,
    reason = "Writes `COUNT IN` value into validated COBOL destination pointer."
)]
fn write_staged_count(target: Option<StagedCountTarget>, count: usize) {
    let Some(t) = target else {
        return;
    };
    if t.ptr == 0 || t.len == 0 {
        return;
    }
    // SAFETY: `t.ptr` and `t.len` were validated in `coblang_rt_m20_set_count_dst`.
    let dst = unsafe { core::slice::from_raw_parts_mut(t.ptr as *mut u8, t.len) };
    let dec = FixedDecimal::from_i64(i64::try_from(count).unwrap_or(0));
    let _ = store_decimal_to_field(
        &dec,
        dst,
        &t.desc,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    );
}

/// Decoded `(name_overrides, field_suppress, generic_suppress)` tuple.
type CommonGenRules = (
    BTreeMap<u32, NameOverride>,
    BTreeMap<u32, CobolSuppressMask>,
    Vec<CobolGenericSuppress>,
);

/// Decodes common `(name_overrides, field_suppress, generic_suppress)` sections from `PlanReader`.
///
/// # Arguments
///
/// * `r` - Cursor over the binary M20 generation plan.
fn decode_common_gen_rules(r: &mut PlanReader<'_>) -> Option<CommonGenRules> {
    let nm_cnt = usize::try_from(r.u32()?).ok()?;
    let mut name_overrides = BTreeMap::new();
    for _ in 0..nm_cnt {
        let fid = r.u32()?;
        let kind = r.u8()?;
        let s = r.str16()?;
        let ov = if kind == 0 {
            NameOverride::Omitted
        } else {
            NameOverride::Rename(s)
        };
        name_overrides.insert(fid, ov);
    }
    let fs_cnt = usize::try_from(r.u32()?).ok()?;
    let mut field_suppress = BTreeMap::new();
    for _ in 0..fs_cnt {
        let fid = r.u32()?;
        let mask = r.u8()?;
        // Later rule for the same `fid` overwrites earlier rule (`0003` Test 5!).
        field_suppress.insert(fid, CobolSuppressMask::from_bits(mask));
    }
    let gs_cnt = usize::try_from(r.u32()?).ok()?;
    let mut generic_suppress = Vec::with_capacity(gs_cnt);
    for _ in 0..gs_cnt {
        let cat_b = r.u8()?;
        let plc_b = r.u8()?;
        let mask_b = r.u8()?;
        let category = match cat_b {
            1 => SuppressCategoryFilter::NumericOnly,
            2 => SuppressCategoryFilter::NonNumericOnly,
            _ => SuppressCategoryFilter::Any,
        };
        let placement = match plc_b {
            1 => SuppressPlacementFilter::ElementOnly,
            2 => SuppressPlacementFilter::AttributeOnly,
            3 => SuppressPlacementFilter::ContentOnly,
            _ => SuppressPlacementFilter::Any,
        };
        generic_suppress.push(CobolGenericSuppress {
            category,
            placement,
            mask: CobolSuppressMask::from_bits(mask_b),
        });
    }
    Some((name_overrides, field_suppress, generic_suppress))
}

/// Executes `XML GENERATE` into `(dst_ptr, dst_len)`, returning `1` on exception or `0` on success.
///
/// # Arguments
/// * `dst_ptr` - Destination XML buffer pointer.
/// * `dst_len` - Destination XML buffer byte length.
/// * `src_ptr` - Source COBOL item pointer.
/// * `plan_ptr` - Serialized `XML GENERATE` schema plan pointer.
/// * `plan_len` - Serialized schema plan byte length.
///
/// # Safety
/// All pointers must reference readable/writable memory matching their byte lengths.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_gen` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_xml_gen(
    dst_ptr: *mut u8,
    dst_len: i64,
    src_ptr: *const u8,
    plan_ptr: *const u8,
    plan_len: i64,
) -> i32 {
    // SAFETY: Caller guarantees valid `plan_ptr` and `dst_ptr`.
    let plan = unsafe { slice_in(plan_ptr, plan_len) };
    // SAFETY: Caller guarantees valid `dst_ptr`.
    let dst = unsafe { slice_out(dst_ptr, dst_len) };
    let mut r = PlanReader { buf: plan, pos: 0 };
    let Some(flags) = r.u8() else {
        return 1;
    };
    let Some(src_total_len) = r.u32() else {
        return 1;
    };
    let Some((name_overrides, field_suppress, generic_suppress)) = decode_common_gen_rules(&mut r)
    else {
        return 1;
    };
    let Some(tp_cnt_u32) = r.u32() else {
        return 1;
    };
    let mut type_overrides = BTreeMap::new();
    for _ in 0..usize::try_from(tp_cnt_u32).unwrap_or(0) {
        let (Some(fid), Some(tk)) = (r.u32(), r.u8()) else {
            return 1;
        };
        let plc = match tk {
            0 => XmlNodePlacement::Attribute,
            2 => XmlNodePlacement::Content,
            _ => XmlNodePlacement::Element,
        };
        type_overrides.insert(fid, plc);
    }
    let Some(root_schema) = r.node() else {
        return 1;
    };
    // SAFETY: `src_ptr` points to `src_total_len` readable bytes of the source COBOL item.
    let src_bytes = unsafe { slice_in(src_ptr, i64::from(src_total_len)) };

    let (ns_raw, pfx_raw, count_target, odo_map) = with_m20_state(|st| {
        (
            st.staged_ns.take(),
            st.staged_pfx.take(),
            st.staged_count.take(),
            std::mem::take(&mut st.staged_odo),
        )
    });
    let empty_bools = BTreeMap::new();
    let root_node = materialize_gen_node(&root_schema, src_bytes, 0, &odo_map, &empty_bools, true);
    let opts = CobolXmlGenOptions {
        xml_decl: if (flags & 1) != 0 {
            CobolXmlDeclMode::IncludeLf
        } else {
            CobolXmlDeclMode::Omit
        },
        attr_mode: if (flags & 2) != 0 {
            XmlAttributeMode::WithAttributes
        } else {
            XmlAttributeMode::ElementsOnly
        },
        decimal_sep: if (flags & 4) != 0 {
            DecimalSepMode::Comma
        } else {
            DecimalSepMode::Period
        },
        namespace_raw: ns_raw,
        namespace_prefix_raw: pfx_raw,
        name_overrides,
        type_overrides,
        field_suppress,
        generic_suppress,
    };
    let res = cobol_xml_generate_into_buffer(&root_node, &opts, dst);
    with_m20_state(|st| {
        M20RuntimeState::set_i32_reg(&mut st.fixed.xml_code, res.xml_code.raw());
    });
    if res.full_count > 0 || res.xml_code.is_ok() {
        write_staged_count(count_target, res.full_count);
    }
    i32::from(!res.xml_code.is_ok())
}

/// Executes `JSON GENERATE` into `(dst_ptr, dst_len)`, returning `1` on exception or `0` on success.
///
/// # Arguments
/// * `dst_ptr` - Destination JSON buffer pointer.
/// * `dst_len` - Destination JSON buffer byte length.
/// * `src_ptr` - Source COBOL item pointer.
/// * `plan_ptr` - Serialized `JSON GENERATE` schema plan pointer.
/// * `plan_len` - Serialized schema plan byte length.
///
/// # Safety
/// All pointers must reference readable/writable memory matching their byte lengths.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_json_gen` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_json_gen(
    dst_ptr: *mut u8,
    dst_len: i64,
    src_ptr: *const u8,
    plan_ptr: *const u8,
    plan_len: i64,
) -> i32 {
    // SAFETY: Caller guarantees valid `plan_ptr` and `dst_ptr`.
    let plan = unsafe { slice_in(plan_ptr, plan_len) };
    // SAFETY: Caller guarantees valid `dst_ptr`.
    let dst = unsafe { slice_out(dst_ptr, dst_len) };
    let mut r = PlanReader { buf: plan, pos: 0 };
    let Some(flags) = r.u8() else {
        return 1;
    };
    let Some(src_total_len) = r.u32() else {
        return 1;
    };
    let Some((name_overrides, field_suppress, generic_suppress)) = decode_common_gen_rules(&mut r)
    else {
        return 1;
    };
    let Some(bf_cnt) = r.u32() else {
        return 1;
    };
    let mut bool_map = BTreeMap::new();
    for _ in 0..usize::try_from(bf_cnt).unwrap_or(0) {
        let (Some(fid), Some(lit)) = (r.u32(), r.str16()) else {
            return 1;
        };
        bool_map.insert(fid, lit);
    }
    let Some(root_schema) = r.node() else {
        return 1;
    };
    // SAFETY: `src_ptr` points to `src_total_len` readable bytes.
    let src_bytes = unsafe { slice_in(src_ptr, i64::from(src_total_len)) };
    let (count_target, odo_map) =
        with_m20_state(|st| (st.staged_count.take(), std::mem::take(&mut st.staged_odo)));
    let root_node = materialize_gen_node(&root_schema, src_bytes, 0, &odo_map, &bool_map, true);
    let opts = CobolJsonGenOptions {
        decimal_sep: if (flags & 1) != 0 {
            DecimalSepMode::Comma
        } else {
            DecimalSepMode::Period
        },
        name_overrides,
        field_suppress,
        generic_suppress,
    };
    let res = cobol_json_generate_into_buffer(&root_node, &opts, dst);
    with_m20_state(|st| {
        M20RuntimeState::set_i32_reg(&mut st.fixed.json_code, res.json_code.raw());
    });
    write_staged_count(count_target, res.full_count);
    i32::from(!res.json_code.is_ok())
}

/// Executes `JSON PARSE` from `(src_ptr, src_len)` into `dst_ptr`, returning `1` on exception else `0`.
///
/// # Arguments
/// * `src_ptr` - Input JSON text pointer.
/// * `src_len` - Input JSON text byte length.
/// * `dst_ptr` - Target COBOL record pointer.
/// * `plan_ptr` - Serialized `JSON PARSE` schema plan pointer.
/// * `plan_len` - Serialized schema plan byte length.
///
/// # Safety
/// All pointers must reference readable/writable memory matching their byte lengths.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_json_parse` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_json_parse(
    src_ptr: *const u8,
    src_len: i64,
    dst_ptr: *mut u8,
    plan_ptr: *const u8,
    plan_len: i64,
) -> i32 {
    // SAFETY: Caller guarantees valid slices.
    let src_bytes = unsafe { slice_in(src_ptr, src_len) };
    // SAFETY: Caller guarantees valid `plan_ptr`.
    let plan = unsafe { slice_in(plan_ptr, plan_len) };
    let mut r = PlanReader { buf: plan, pos: 0 };
    let (Some(flags), Some(dst_total_len)) = (r.u8(), r.u32()) else {
        return 1;
    };
    let Some(opts) = decode_json_parse_opts(&mut r, flags) else {
        return 1;
    };
    let Some(root_schema) = r.node() else {
        return 1;
    };
    // SAFETY: `dst_ptr` points to `dst_total_len` writable bytes.
    let dst_bytes = unsafe { slice_out(dst_ptr, i64::from(dst_total_len)) };
    let odo_map = with_m20_state(|st| std::mem::take(&mut st.staged_odo));

    let Some(mut target_tree) = build_parse_target_node(&root_schema, &odo_map, true) else {
        return 1;
    };
    let json_text = String::from_utf8_lossy(src_bytes);
    let (jc, js) = match json_parse(&json_text, &mut target_tree, &opts) {
        Ok(out) => {
            apply_parsed_target_to_record(&root_schema, &target_tree, dst_bytes, 0, true);
            (out.json_code.raw(), out.json_status.raw())
        }
        Err(_) => (100, JsonStatus::SYNTAX_ERROR.raw()),
    };
    with_m20_state(|st| {
        M20RuntimeState::set_i32_reg(&mut st.fixed.json_code, jc);
        M20RuntimeState::set_i32_reg(&mut st.fixed.json_status, js);
    });
    i32::from(jc != 0)
}

/// Decodes [`JsonParseOptions`] from `PlanReader`.
///
/// # Arguments
///
/// * `r` - Cursor over the binary `JSON PARSE` plan.
/// * `flags` - Bitmask of parse flags (`bit 0 = WITH DETAIL`).
fn decode_json_parse_opts(r: &mut PlanReader<'_>, flags: u8) -> Option<JsonParseOptions> {
    let mut opts = JsonParseOptions::new().with_detail_mode(if (flags & 1) != 0 {
        JsonDetailMode::WithDetail
    } else {
        JsonDetailMode::Standard
    });
    let nm_cnt = usize::try_from(r.u32()?).ok()?;
    for _ in 0..nm_cnt {
        let f_name = r.str16()?;
        let kind = r.u8()?;
        let ov_s = r.str16()?;
        if let Ok(cf) = CobolFieldName::try_new(&f_name) {
            let m = if kind == 0 {
                NameMapping::new(cf, NameOverride::Omitted)
            } else {
                NameMapping::new(cf, NameOverride::Rename(ov_s))
            };
            opts = opts.with_name_mapping(m);
        }
    }
    let sup_cnt = usize::try_from(r.u32()?).ok()?;
    for _ in 0..sup_cnt {
        let f_name = r.str16()?;
        if let Ok(cf) = CobolFieldName::try_new(&f_name) {
            opts = opts.with_suppressed_field(cf);
        }
    }
    let bf_cnt = usize::try_from(r.u32()?).ok()?;
    for _ in 0..bf_cnt {
        let f_name = r.str16()?;
        let t_lit = r.str16()?;
        let f_lit = r.str16()?;
        if let Ok(cf) = CobolFieldName::try_new(&f_name) {
            opts = opts.with_boolean_mapping(JsonBooleanMapping::new(cf, t_lit, f_lit));
        }
    }
    Some(opts)
}

/// Builds a mutable [`ParseTargetNode`] hierarchy mirroring `schema`.
///
/// # Arguments
///
/// * `schema` - Decoded record schema node.
/// * `odo_map` - Dynamic `OCCURS DEPENDING ON` counts.
/// * `is_root` - `true` for the top-level root node.
fn build_parse_target_node(
    schema: &SchemaNode,
    odo_map: &BTreeMap<u32, usize>,
    is_root: bool,
) -> Option<ParseTargetNode> {
    let safe_name = if schema.raw_name.is_empty() {
        String::from("FILLER")
    } else {
        schema.raw_name.clone()
    };
    let cf = CobolFieldName::try_new(&safe_name)
        .or_else(|_| CobolFieldName::try_new("ITEM"))
        .ok()?;
    if !is_root && schema.max_occurs > 0 {
        let cap = schema
            .odo_id
            .and_then(|id| odo_map.get(&id).copied())
            .unwrap_or(schema.max_occurs)
            .min(schema.max_occurs);
        let mut slots = Vec::with_capacity(cap);
        for _ in 0..cap {
            slots.push(build_single_parse_target(cf.clone(), schema, odo_map)?);
        }
        return Some(ParseTargetNode::table(cf, slots));
    }
    build_single_parse_target(cf, schema, odo_map)
}

/// Builds a single scalar or group [`ParseTargetNode`].
///
/// # Arguments
///
/// * `cf` - Validated [`CobolFieldName`].
/// * `schema` - Decoded record schema node.
/// * `odo_map` - Dynamic `OCCURS DEPENDING ON` counts.
fn build_single_parse_target(
    cf: CobolFieldName,
    schema: &SchemaNode,
    odo_map: &BTreeMap<u32, usize>,
) -> Option<ParseTargetNode> {
    match &schema.payload {
        SchemaPayload::Elementary { cat_tag, .. } => {
            let vcat = if *cat_tag == 2 {
                ValueCategory::Numeric
            } else {
                ValueCategory::Alphanumeric
            };
            Some(ParseTargetNode::elementary(
                cf,
                vcat,
                String::new(),
                Some(schema.elem_len),
            ))
        }
        SchemaPayload::Group(children) => {
            let mut sub = Vec::with_capacity(children.len());
            for ch in children {
                sub.push(build_parse_target_node(ch, odo_map, false)?);
            }
            Some(ParseTargetNode::group(cf, sub))
        }
    }
}

/// Writes populated [`ParseTargetNode`] leaves back into `record_bytes` via `coblang_pic::execute_move`.
///
/// # Arguments
///
/// * `schema` - Decoded record schema node.
/// * `target` - Populated [`ParseTargetNode`].
/// * `record_bytes` - Mutable destination COBOL record bytes.
/// * `base_off` - Parent byte offset within `record_bytes`.
/// * `is_root` - `true` for the top-level root node.
fn apply_parsed_target_to_record(
    schema: &SchemaNode,
    target: &ParseTargetNode,
    record_bytes: &mut [u8],
    base_off: usize,
    is_root: bool,
) {
    let start = if is_root {
        0
    } else {
        base_off.saturating_add(schema.rel_offset)
    };
    match (&schema.payload, &target.kind) {
        (_, ParseTargetKind::Table { elements, .. }) => {
            for (idx, slot) in elements.iter().enumerate() {
                let occ_off = start.saturating_add(idx.saturating_mul(schema.elem_len));
                apply_single_parsed_target(schema, slot, record_bytes, occ_off);
            }
        }
        _ => apply_single_parsed_target(schema, target, record_bytes, start),
    }
}

/// Writes a single scalar or group [`ParseTargetNode`] into `record_bytes` at `start`.
///
/// # Arguments
///
/// * `schema` - Decoded record schema node.
/// * `target` - Populated [`ParseTargetNode`].
/// * `record_bytes` - Mutable destination COBOL record bytes.
/// * `start` - Starting byte offset of this occurrence.
fn apply_single_parsed_target(
    schema: &SchemaNode,
    target: &ParseTargetNode,
    record_bytes: &mut [u8],
    start: usize,
) {
    match (&schema.payload, &target.kind) {
        (
            SchemaPayload::Elementary { cat_tag, desc },
            ParseTargetKind::Elementary {
                value, assigned, ..
            },
        ) => {
            if *assigned != FieldAssignState::Assigned {
                return;
            }
            let end = start.saturating_add(schema.elem_len);
            let Some(dst_slice) = record_bytes.get_mut(start..end) else {
                return;
            };
            if *cat_tag == 2 {
                if let Ok(dec) = de_edit_to_decimal(value.as_bytes(), b'.', desc.currency_symbol) {
                    let _ = store_decimal_to_field(
                        &dec,
                        dst_slice,
                        desc,
                        RoundingMode::Truncation,
                        HighOrderPolicy::ModuloTruncate,
                    );
                }
            } else {
                let src = MoveSource::AlphanumericLiteral(value.as_bytes());
                let _ = execute_move(src, dst_slice, desc);
            }
        }
        (
            SchemaPayload::Group(s_children),
            ParseTargetKind::Group {
                children: t_children,
            },
        ) => {
            for (sc, tc) in s_children.iter().zip(t_children.iter()) {
                apply_parsed_target_to_record(sc, tc, record_bytes, start, false);
            }
        }
        _ => {}
    }
}

/// Opens a new streaming `XML PARSE` session, returning its `session_id` (`>= 1`).
///
/// # Arguments
/// * `file_ptr` - Source filename UTF-8 bytes.
/// * `file_len` - Source filename byte length.
/// * `line_no` - 1-based source line of `XML PARSE`.
/// * `mode` - `0` = `Xmlss`, `1` = `Compat`.
///
/// # Safety
/// `file_ptr` must reference `file_len` readable bytes when `file_len > 0`.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_parse_begin` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_xml_parse_begin(
    file_ptr: *const u8,
    file_len: i64,
    line_no: i32,
    mode: i32,
) -> i32 {
    // SAFETY: Caller guarantees valid filename slice.
    let fbytes = unsafe { slice_in(file_ptr, file_len) };
    let fname = String::from_utf8_lossy(fbytes).into_owned();
    let smode = if mode == 1 {
        CobolXmlStreamMode::Compat
    } else {
        CobolXmlStreamMode::Xmlss
    };
    let line_u32 = u32::try_from(line_no.max(1)).unwrap_or(1);
    with_m20_state(|st| {
        st.next_session_id = st.next_session_id.wrapping_add(1).max(1);
        let sid = st.next_session_id;
        st.xml_sessions
            .insert(sid, CobolXmlStreamParser::new(fname, line_u32, smode));
        sid
    })
}

/// Feeds an input segment into `session_id`.
///
/// # Arguments
/// * `session_id` - Session handle returned by [`coblang_rt_m20_xml_parse_begin`].
/// * `src_ptr` - Input segment byte pointer.
/// * `src_len` - Input segment byte length.
///
/// # Safety
/// `src_ptr` must reference `src_len` readable bytes when `src_len > 0`.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_parse_feed` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_xml_parse_feed(
    session_id: i32,
    src_ptr: *const u8,
    src_len: i64,
) {
    // SAFETY: Caller guarantees valid input segment slice.
    let chunk = unsafe { slice_in(src_ptr, src_len) };
    with_m20_state(|st| {
        if let Some(sess) = st.xml_sessions.get_mut(&session_id) {
            sess.feed_segment(chunk);
        }
    });
}

/// Polls the next event for `session_id`, populating all `XML-*` special registers.
///
/// Returns `1` when an event is ready for `PROCESSING PROCEDURE`, or `0` when complete.
///
/// # Arguments
/// * `session_id` - Active `XML PARSE` session handle.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_parse_next` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_xml_parse_next(session_id: i32) -> i32 {
    let mut stderr_msg: Option<String> = None;
    let ready = with_m20_state(|st| {
        let ev = st
            .xml_sessions
            .get_mut(&session_id)
            .and_then(CobolXmlStreamParser::next_event);
        let Some(ev) = ev else {
            return 0;
        };
        stderr_msg = ev.stderr_warning;
        st.set_xml_event(ev.event_name);
        M20RuntimeState::set_i32_reg(&mut st.fixed.xml_code, ev.xml_code);
        M20RuntimeState::set_i32_reg(&mut st.fixed.xml_info, ev.xml_information);
        M20RuntimeState::set_text_pair(&mut st.xml_text, &mut st.xml_ntext, &ev.xml_text);
        M20RuntimeState::set_text_pair(&mut st.xml_ns, &mut st.xml_nns, &ev.xml_namespace);
        M20RuntimeState::set_text_pair(&mut st.xml_pfx, &mut st.xml_npfx, &ev.xml_namespace_prefix);
        1
    });
    if let Some(msg) = stderr_msg {
        let mut err = io::stderr().lock();
        let _ = writeln!(err, "{msg}");
        let _ = err.flush();
    }
    ready
}

/// Evaluates `XML-CODE` after the COBOL `PROCESSING PROCEDURE` finishes an event.
///
/// Returns `0` (`Finished`), `1` (`Continue`), or `2` (`NeedMoreInput` when `XML-CODE == 1` on `END-OF-INPUT`).
///
/// # Arguments
/// * `session_id` - Active `XML PARSE` session handle.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_parse_after` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_xml_parse_after(session_id: i32) -> i32 {
    with_m20_state(|st| {
        let cur_code = M20RuntimeState::get_i32_reg(*st.fixed.xml_code);
        let Some(sess) = st.xml_sessions.get_mut(&session_id) else {
            return 0;
        };
        match sess.after_handler(cur_code) {
            CobolXmlPostAction::Finished => 0,
            CobolXmlPostAction::Continue => 1,
            CobolXmlPostAction::NeedMoreInput => {
                M20RuntimeState::set_i32_reg(&mut st.fixed.xml_code, 0);
                2
            }
        }
    })
}

/// Finalizes and removes `session_id`, returning `1` if `ON EXCEPTION` should execute else `0`.
///
/// # Arguments
/// * `session_id` - Active `XML PARSE` session handle.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_xml_parse_end` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_xml_parse_end(session_id: i32) -> i32 {
    with_m20_state(|st| {
        let Some(sess) = st.xml_sessions.remove(&session_id) else {
            return 0;
        };
        if sess.clear_event_on_exit() {
            st.set_xml_event("");
        }
        M20RuntimeState::set_i32_reg(&mut st.fixed.xml_info, 0);
        M20RuntimeState::set_text_pair(&mut st.xml_text, &mut st.xml_ntext, "");
        M20RuntimeState::set_text_pair(&mut st.xml_ns, &mut st.xml_nns, "");
        M20RuntimeState::set_text_pair(&mut st.xml_pfx, &mut st.xml_npfx, "");
        i32::from(sess.had_exception())
    })
}

/// Registers an OO COBOL class metadata record in the run-unit vtable registry.
///
/// # Arguments
/// * `cls_ptr` - External class name UTF-8 bytes.
/// * `cls_len` - External class name byte length.
/// * `parents_ptr` - NUL-separated immediate parent external names.
/// * `parents_len` - Parent list byte length.
/// * `init_ptr` - Initial instance storage template bytes.
/// * `init_len` - Initial instance storage byte length.
///
/// # Safety
/// Non-null pointers with positive lengths must reference readable memory.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_reg_class` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_oo_reg_class(
    cls_ptr: *const u8,
    cls_len: i64,
    parents_ptr: *const u8,
    parents_len: i64,
    init_ptr: *const u8,
    init_len: i64,
) {
    // SAFETY: Caller guarantees valid slices.
    let cls_key =
        String::from_utf8_lossy(unsafe { slice_in(cls_ptr, cls_len) }).to_ascii_uppercase();
    // SAFETY: Caller guarantees valid slices.
    let p_raw = unsafe { slice_in(parents_ptr, parents_len) };
    let parents: Vec<String> = p_raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_ascii_uppercase())
        .collect();
    // SAFETY: Caller guarantees valid slices.
    let init_template = unsafe { slice_in(init_ptr, init_len) }.to_vec();
    with_m20_state(|st| {
        let entry = st
            .oo_classes
            .entry(cls_key)
            .or_insert_with(|| RegisteredOoClass {
                parents: Vec::new(),
                init_template: Vec::new(),
                methods: BTreeMap::new(),
            });
        entry.parents = parents;
        entry.init_template = init_template;
    });
}

/// Registers a compiled OO method entrypoint on `cls_ptr`.
///
/// # Arguments
/// * `cls_ptr` - External class name UTF-8 bytes.
/// * `cls_len` - External class name byte length.
/// * `comp` - `0` = `Factory`, `1` = `Object`.
/// * `meth_ptr` - External method name UTF-8 bytes.
/// * `meth_len` - External method name byte length.
/// * `fn_ptr` - Compiled `extern "C" fn() -> i32` function pointer.
///
/// # Safety
/// String slices must be readable and `fn_ptr` must be a valid `extern "C" fn() -> i32` pointer.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_reg_method` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_oo_reg_method(
    cls_ptr: *const u8,
    cls_len: i64,
    comp: i32,
    meth_ptr: *const u8,
    meth_len: i64,
    fn_ptr: *const u8,
) {
    if fn_ptr.is_null() {
        return;
    }
    // SAFETY: Caller guarantees valid slices.
    let cls_key =
        String::from_utf8_lossy(unsafe { slice_in(cls_ptr, cls_len) }).to_ascii_uppercase();
    // SAFETY: Caller guarantees valid slices.
    let meth_key =
        String::from_utf8_lossy(unsafe { slice_in(meth_ptr, meth_len) }).to_ascii_uppercase();
    with_m20_state(|st| {
        let entry = st
            .oo_classes
            .entry(cls_key)
            .or_insert_with(|| RegisteredOoClass {
                parents: Vec::new(),
                init_template: Vec::new(),
                methods: BTreeMap::new(),
            });
        entry.methods.insert((comp, meth_key), fn_ptr as usize);
    });
}

/// Returns the active 64-bit `SELF` object/factory handle (`0` when outside a method).
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_self_handle` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_oo_self_handle() -> i64 {
    with_m20_state(|st| st.oo_self_stack.last().map_or(0, |&(h, _)| h))
}

/// Returns the active `SELF` instance `WORKING-STORAGE` data pointer.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_self_data_ptr` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_oo_self_data_ptr() -> *mut u8 {
    with_m20_state(|st| {
        st.oo_self_stack
            .last()
            .map_or(core::ptr::null_mut(), |&(_, p)| p as *mut u8)
    })
}

/// Begins staging arguments and optional `RETURNING` buffer for an OO `INVOKE`.
///
/// # Arguments
/// * `ret_ptr` - Optional `RETURNING` destination pointer (`null` if omitted).
/// * `ret_len` - `RETURNING` byte length.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_begin_invoke` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_oo_begin_invoke(ret_ptr: *mut u8, ret_len: i64) {
    let ret_opt = if !ret_ptr.is_null() && ret_len > 0 {
        Some((ret_ptr as usize, usize::try_from(ret_len).unwrap_or(0)))
    } else {
        None
    };
    with_m20_state(|st| {
        st.oo_staged_args.clear();
        st.oo_staged_ret = ret_opt;
    });
}

/// Appends one `USING` argument `(ptr, len)` (or `(null, 0)` for `OMITTED`) to the staged `INVOKE`.
///
/// # Arguments
/// * `arg_ptr` - Argument storage pointer (`null` when `OMITTED`).
/// * `arg_len` - Argument byte length.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_push_arg` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m20_oo_push_arg(arg_ptr: *mut u8, arg_len: i64) {
    let p = if arg_ptr.is_null() {
        0_usize
    } else {
        arg_ptr as usize
    };
    let l = usize::try_from(arg_len.max(0)).unwrap_or(0);
    with_m20_state(|st| st.oo_staged_args.push((p, l)));
}

/// Dispatches an OO COBOL `INVOKE` or inline `::` method call (`new`, factory, instance, or `SUPER`).
///
/// # Arguments
/// * `recv_kind` - `0` = `FactoryClass`, `1` = `ObjectHandle`, `2` = `Super`.
/// * `recv_handle` - 64-bit object/factory handle when `recv_kind == 1`.
/// * `cls_ptr` - Target/super external class name UTF-8 bytes.
/// * `cls_len` - Target/super class name byte length.
/// * `meth_ptr` - Method name UTF-8 bytes.
/// * `meth_len` - Method name byte length.
///
/// # Safety
/// All staged argument/return pointers and registered method entrypoints must be valid.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_oo_invoke` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_oo_invoke(
    recv_kind: i32,
    recv_handle: i64,
    cls_ptr: *const u8,
    cls_len: i64,
    meth_ptr: *const u8,
    meth_len: i64,
) -> i32 {
    // SAFETY: Caller guarantees valid slices.
    let cls_upper = String::from_utf8_lossy(unsafe { slice_in(cls_ptr, cls_len) })
        .trim()
        .to_ascii_uppercase();
    // SAFETY: Caller guarantees valid slices.
    let meth_upper = String::from_utf8_lossy(unsafe { slice_in(meth_ptr, meth_len) })
        .trim()
        .to_ascii_uppercase();

    let (args, ret_opt, dispatch_info) = with_m20_state(|st| {
        let args = std::mem::take(&mut st.oo_staged_args);
        let ret_opt = st.oo_staged_ret.take();
        let info =
            resolve_oo_dispatch(st, recv_kind, recv_handle, &cls_upper, &meth_upper, ret_opt);
        (args, ret_opt, info)
    });

    let Some((fn_addr, self_handle, self_data_ptr)) = dispatch_info else {
        return 0;
    };
    if fn_addr == 0 {
        return 0;
    }

    let mut scratch_ret = vec![0_u8; 256];
    let fallback_ret = (scratch_ret.as_mut_ptr() as usize, scratch_ret.len());
    let (ret_p, ret_l) = ret_opt.unwrap_or(fallback_ret);
    let mut param_ptrs: Vec<usize> = args.iter().map(|&(p, _)| p).collect();
    let mut param_sizes: Vec<usize> = args.iter().map(|&(_, l)| l).collect();
    param_ptrs.push(ret_p);
    param_sizes.push(ret_l);

    with_m20_state(|st| st.oo_self_stack.push((self_handle, self_data_ptr)));
    with_c_abi_state(|cst| {
        cst.module_stack.push(ActiveModuleFrame {
            module_name: meth_upper,
            param_ptrs,
            param_sizes,
            ..ActiveModuleFrame::default()
        });
    });

    // SAFETY: `fn_addr` was registered by `coblang_rt_m20_oo_reg_method` as `extern "C" fn() -> i32`.
    let callee: extern "C" fn() -> i32 = unsafe { core::mem::transmute(fn_addr) };
    let rc = callee();

    with_c_abi_state(|cst| {
        let _ = cst.module_stack.pop();
    });
    with_m20_state(|st| {
        let _ = st.oo_self_stack.pop();
    });
    rc
}

/// Resolves `(fn_addr, self_handle, self_data_ptr)` or handles `"NEW"` instance allocation.
///
/// # Arguments
///
/// * `st` - Mutable [`M20RuntimeState`].
/// * `recv_kind` - Receiver kind (`0=Factory, 1=ObjectRef, 2=Super`).
/// * `recv_handle` - Object reference handle (`recv_kind == 1`).
/// * `cls_upper` - Uppercase external class name.
/// * `meth_upper` - Uppercase method name.
/// * `ret_opt` - Optional `(ret_ptr, ret_len)` buffer for `"NEW"`.
#[expect(
    unsafe_code,
    reason = "Writes newly allocated 8-byte object handle into validated `RETURNING` buffer."
)]
fn resolve_oo_dispatch(
    st: &mut M20RuntimeState,
    recv_kind: i32,
    recv_handle: i64,
    cls_upper: &str,
    meth_upper: &str,
    ret_opt: Option<(usize, usize)>,
) -> Option<(usize, i64, usize)> {
    if recv_kind == 0 {
        if meth_upper == "NEW" {
            let tmpl = st.oo_classes.get(cls_upper).map_or_else(
                || vec![0_u8; 8],
                |c| {
                    if c.init_template.is_empty() {
                        vec![0_u8; 8]
                    } else {
                        c.init_template.clone()
                    }
                },
            );
            st.next_oo_handle = st.next_oo_handle.wrapping_add(1).max(1);
            let h = st.next_oo_handle;
            st.oo_instances.insert(
                h,
                OoHeapInstance {
                    class_ext: cls_upper.to_owned(),
                    comp: 1,
                    storage: tmpl.into_boxed_slice(),
                },
            );
            if let Some((rp, rl)) = ret_opt
                && rp != 0
                && rl >= 8
            {
                // SAFETY: Validated non-null `rp` with `rl >= 8` bytes.
                let out = unsafe { core::slice::from_raw_parts_mut(rp as *mut u8, 8) };
                out.copy_from_slice(&h.to_ne_bytes());
            }
            return Some((0, h, 0));
        }
        let fac_h = ensure_factory_singleton(st, cls_upper);
        let fn_addr = lookup_vtable_fn(&st.oo_classes, cls_upper, 0, meth_upper)?;
        return Some((fn_addr, fac_h, 0));
    }

    if recv_kind == 2 {
        let &(cur_h, cur_ptr) = st.oo_self_stack.last()?;
        let comp = st.oo_instances.get(&cur_h).map_or(1, |i| i.comp);
        let fn_addr = lookup_vtable_fn(&st.oo_classes, cls_upper, comp, meth_upper)?;
        return Some((fn_addr, cur_h, cur_ptr));
    }

    let inst = st.oo_instances.get_mut(&recv_handle)?;
    let inst_cls = inst.class_ext.clone();
    let inst_comp = inst.comp;
    let data_ptr = inst.storage.as_mut_ptr() as usize;
    let fn_addr = lookup_vtable_fn(&st.oo_classes, &inst_cls, inst_comp, meth_upper)
        .or_else(|| lookup_vtable_fn(&st.oo_classes, &inst_cls, 0, meth_upper))?;
    Some((fn_addr, recv_handle, data_ptr))
}

/// Ensures a singleton handle exists for factory receiver `cls_upper`.
///
/// # Arguments
///
/// * `st` - Mutable [`M20RuntimeState`].
/// * `cls_upper` - Uppercase external class name.
fn ensure_factory_singleton(st: &mut M20RuntimeState, cls_upper: &str) -> i64 {
    if let Some(&h) = st.oo_factory_handles.get(cls_upper) {
        return h;
    }
    st.next_oo_handle = st.next_oo_handle.wrapping_add(1).max(1);
    let h = st.next_oo_handle;
    st.oo_instances.insert(
        h,
        OoHeapInstance {
            class_ext: cls_upper.to_owned(),
            comp: 0,
            storage: vec![0_u8; 8].into_boxed_slice(),
        },
    );
    st.oo_factory_handles.insert(cls_upper.to_owned(), h);
    h
}

/// Walks `cls_upper` and its `parents` in `oo_classes` to find `(comp, meth_upper)`.
///
/// # Arguments
///
/// * `classes` - Registered OO class map.
/// * `cls_upper` - Starting uppercase class name.
/// * `comp` - Compartment (`0=Factory, 1=Object`).
/// * `meth_upper` - Uppercase method name.
fn lookup_vtable_fn(
    classes: &BTreeMap<String, RegisteredOoClass>,
    cls_upper: &str,
    comp: i32,
    meth_upper: &str,
) -> Option<usize> {
    let mut visited = BTreeSet::new();
    lookup_vtable_rec(classes, cls_upper, comp, meth_upper, &mut visited)
}

/// Recursive helper for [`lookup_vtable_fn`].
///
/// # Arguments
///
/// * `classes` - Registered OO class map.
/// * `cls_upper` - Current uppercase class name.
/// * `comp` - Compartment (`0=Factory, 1=Object`).
/// * `meth_upper` - Uppercase method name.
/// * `visited` - Cycle-guard set of visited class names.
fn lookup_vtable_rec(
    classes: &BTreeMap<String, RegisteredOoClass>,
    cls_upper: &str,
    comp: i32,
    meth_upper: &str,
    visited: &mut BTreeSet<String>,
) -> Option<usize> {
    if !visited.insert(cls_upper.to_owned()) {
        return None;
    }
    let cls = classes.get(cls_upper)?;
    if let Some(&addr) = cls.methods.get(&(comp, meth_upper.to_owned())) {
        return Some(addr);
    }
    for parent in &cls.parents {
        if let Some(addr) = lookup_vtable_rec(classes, parent, comp, meth_upper, visited) {
            return Some(addr);
        }
    }
    None
}

/// Evaluates `NUMVAL` (`mode == 0`) or `NUMVAL-C` (`mode == 1`) on `(ptr, len)` and pushes onto `DecimalEvalStack`.
///
/// # Arguments
/// * `ptr` - Input string byte pointer.
/// * `len` - Input string byte length.
/// * `mode` - `0` = `NUMVAL`, `1` = `NUMVAL-C`.
///
/// # Safety
/// When `len > 0` and `!ptr.is_null()`, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Exported `coblang_rt_m20_dec_push_numval` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m20_dec_push_numval(
    ptr: *const u8,
    len: i64,
    mode: i32,
) -> i32 {
    // SAFETY: Caller guarantees valid slice.
    let raw = unsafe { slice_in(ptr, len) };
    let parsed = if mode == 1 {
        str_numval_c(
            raw,
            None,
            CurrencyCasePolicy::ExactCase,
            IntrDecPoint::Period,
        )
    } else {
        str_numval(raw, IntrDecPoint::Period)
    };
    let dec = parsed.unwrap_or(FixedDecimal::ZERO);
    let ascii = dec.to_string();
    let ok = with_global_runtime(|rt| rt.decimal_stack_mut().push_ascii(ascii.as_bytes()).is_ok());
    if ok { 0 } else { -1 }
}
