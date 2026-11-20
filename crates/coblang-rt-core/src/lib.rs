//! Clean-room pure-Rust COBOL runtime state, display formatting, and string helpers.

/// Pure-Rust `ACCEPT` (`CONSOLE`, `DATE`, `DAY`, `TIME`, CLI args, environment) and `DISPLAY UPON ENVIRONMENT-*` engine.
pub mod accept_env;
/// Compact binary serialization for runtime `MoveFieldDesc` and `INITIALIZE` plans.
pub mod desc_codec;
/// ISO/IEC 1989:2023 `EC-*` exception hierarchy, `>>TURN` masks, and NIST `DEBUG-ITEM` state.
pub mod exceptions;
/// Pure-Rust runtime execution engine for `MOVE`, `INITIALIZE`, decimal arithmetic, comparisons, and `DISPLAY`.
pub mod field_ops;
/// Binary wire codec and ABI constants for M14/M15 file connectors.
pub mod fileio_wire;
/// Pure-Rust heap allocation ledger for `ALLOCATE`, `FREE`, `cob_malloc`, and `cob_free`.
pub mod heap;
/// Safe runtime staging state for multi-clause `STRING`, `UNSTRING`, and `INSPECT` statements.
pub mod m10_staging;
/// Character-indexed `INSPECT`, `STRING`, and `UNSTRING` execution over `NATIONAL` (`UTF-16LE`) and `UTF-8` (`PIC U`).
pub mod m17_unicode_ops;

use std::io::{BufRead, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

use accept_env::{
    AcceptEnvState, AcceptFlowOutcome, AcceptTargetDest, EnvDisplayTarget, RuntimeAcceptSource,
    execute_runtime_accept,
};
use coblang_common::{CoblangError, Result};
use exceptions::{DebugItemState, ExceptionCode, ExceptionRuntimeState, parse_cob_set_debug};
use field_ops::{DecimalEvalStack, DisplayStagingBuffer, InitStagingState};
use heap::HeapTracker;
use m10_staging::M10StringInspectStaging;

/// Target output stream for `DISPLAY` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputDevice {
    /// Standard output (`SYSOUT` / `CONSOLE`).
    Stdout,
    /// Standard error (`SYSERR` / `SYSPUNCH`).
    Stderr,
}

impl OutputDevice {
    /// Decodes a C-ABI device selector (`0` -> `Stdout`, non-zero -> `Stderr`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer device selector from generated code.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        if code == 0 {
            Self::Stdout
        } else {
            Self::Stderr
        }
    }

    /// Encodes this device into its C-ABI integer representation.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

/// Line-termination policy for `DISPLAY` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineAdvancing {
    /// Emit trailing LF (`0x0A`) after operands.
    AdvanceLine,
    /// Suppress trailing LF (`WITH NO ADVANCING`).
    NoAdvancing,
}

impl LineAdvancing {
    /// Decodes a C-ABI advancing selector (`0` -> `NoAdvancing`, non-zero -> `AdvanceLine`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer advancing selector from generated code.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        if code == 0 {
            Self::NoAdvancing
        } else {
            Self::AdvanceLine
        }
    }

    /// Encodes this advancing policy into its C-ABI integer representation.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::NoAdvancing => 0,
            Self::AdvanceLine => 1,
        }
    }
}

/// Trim side selector for `FUNCTION TRIM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrimDirection {
    /// Strip both leading and trailing ASCII spaces (`0x20`).
    Both,
    /// Strip leading ASCII spaces (`LEADING`).
    Leading,
    /// Strip trailing ASCII spaces (`TRAILING`).
    Trailing,
}

impl TrimDirection {
    /// Decodes a C-ABI trim mode (`1` -> `Leading`, `2` -> `Trailing`, otherwise `Both`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer trim mode from generated code.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::Leading,
            2 => Self::Trailing,
            _ => Self::Both,
        }
    }

    /// Encodes this trim direction into its C-ABI integer representation.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Both => 0,
            Self::Leading => 1,
            Self::Trailing => 2,
        }
    }
}

/// Runtime value category descriptor for `DISPLAY` formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldDisplayKind {
    /// Raw alphanumeric/alphabetic/literal byte slice emitted verbatim.
    RawBytes,
    /// Signed native 32-bit binary integer (`i32`, e.g. `RETURN-CODE` or 4-byte `COMP-5`).
    SignedInt32,
    /// Unsigned native 32-bit binary integer (`u32`).
    UnsignedInt32,
    /// Signed big-endian binary integer (`1`, `2`, `4`, or `8` bytes).
    SignedBigEndianInt,
    /// Unsigned big-endian binary integer (`1`, `2`, `4`, or `8` bytes).
    UnsignedBigEndianInt,
}

impl FieldDisplayKind {
    /// Decodes a C-ABI field kind byte into [`FieldDisplayKind`].
    ///
    /// # Arguments
    ///
    /// * `tag` - Raw ABI tag byte.
    #[must_use]
    pub const fn from_abi_tag(tag: u8) -> Self {
        match tag {
            1 => Self::SignedInt32,
            2 => Self::UnsignedInt32,
            3 => Self::SignedBigEndianInt,
            4 => Self::UnsignedBigEndianInt,
            _ => Self::RawBytes,
        }
    }

    /// Encodes this display kind into its C-ABI tag byte.
    #[must_use]
    pub const fn to_abi_tag(self) -> u8 {
        match self {
            Self::RawBytes => 0,
            Self::SignedInt32 => 1,
            Self::UnsignedInt32 => 2,
            Self::SignedBigEndianInt => 3,
            Self::UnsignedBigEndianInt => 4,
        }
    }
}

/// Borrowed descriptor for a single runtime `DISPLAY` operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeDisplayItem<'a> {
    /// Value category controlling byte-to-text formatting.
    pub kind: FieldDisplayKind,
    /// Decimal scale factor (`0` for integers and alphanumeric slices).
    pub scale: i16,
    /// Underlying field storage bytes.
    pub bytes: &'a [u8],
}

/// Strongly-typed external hardware/software switch state (`SWITCH-0..36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwitchState {
    /// Switch is `OFF` (`0`).
    Off,
    /// Switch is `ON` (`1`).
    On,
}

impl SwitchState {
    /// Decodes a C-ABI switch status integer (`0` = `Off`, non-zero = `On`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw ABI status integer.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        if code == 0 { Self::Off } else { Self::On }
    }

    /// Encodes `self` as `0` (`Off`) or `1` (`On`).
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
        }
    }
}

/// Parses an environment variable value (`"1"`, `"ON"`, `"TRUE"`, `"Y"`) into [`SwitchState`].
///
/// # Arguments
///
/// * `raw` - Environment string value.
#[must_use]
pub fn parse_env_switch_state(raw: &str) -> SwitchState {
    match raw.trim().to_ascii_uppercase().as_str() {
        "1" | "ON" | "TRUE" | "Y" | "YES" => SwitchState::On,
        _ => SwitchState::Off,
    }
}

/// Mutable run-unit state shared across all active COBOL programs and subprograms in a process.
#[derive(Debug, Default)]
pub struct CobolRuntimeContext {
    /// Current COBOL `RETURN-CODE` special register (`S9(9) COMP-5`).
    return_code: i32,
    /// Current `CALL` nesting depth (`0` inside top-level entry program, `>= 1` in subprograms).
    call_depth: u32,
    /// Bitmask storing external `SWITCH-0..36` states (`(switch_bits >> idx) & 1`).
    switch_bits: u64,
    /// Command-line arguments captured during `cob_init`.
    args: Vec<String>,
    /// Exact 38/76-digit decimal expression evaluation stack.
    decimal_stack: DecimalEvalStack,
    /// Staged output buffer for multi-operand `DISPLAY` statements.
    display_staging: DisplayStagingBuffer,
    /// Staged `INITIALIZE ... REPLACING` rules.
    init_staging: InitStagingState,
    /// Per-run-unit `ACCEPT` / `DISPLAY UPON ENVIRONMENT-*` / CLI argument cursor state.
    accept_env: AcceptEnvState,
    /// Staged `STRING`, `UNSTRING`, and `INSPECT` buffers.
    m10_staging: M10StringInspectStaging,
    /// Active `ALLOCATE` / `cob_malloc` heap block ledger.
    heap: HeapTracker,
    /// Structured ISO 2023 `EC-*` exception state (`>>TURN`, `LAST EXCEPTION`, declarative guard).
    exceptions: ExceptionRuntimeState,
    /// NIST CCVS-85 `DEBUG-ITEM` special register state.
    debug_item: DebugItemState,
}

impl CobolRuntimeContext {
    /// Creates a fresh initialized run-unit context with `RETURN-CODE = 0`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            return_code: 0,
            call_depth: 0,
            switch_bits: 0,
            args: Vec::new(),
            decimal_stack: DecimalEvalStack::new(),
            display_staging: DisplayStagingBuffer::new(),
            init_staging: InitStagingState::new(),
            accept_env: AcceptEnvState::new(),
            m10_staging: M10StringInspectStaging::new(),
            heap: HeapTracker::new(),
            exceptions: ExceptionRuntimeState::new(),
            debug_item: DebugItemState::new(),
        }
    }

    /// Resets run-unit state on `cob_init` and stores command-line arguments.
    ///
    /// # Arguments
    ///
    /// * `args` - Process command-line argument vector.
    pub fn initialize(&mut self, args: Vec<String>) {
        self.initialize_with_env(args, |key| std::env::var(key).ok());
    }

    /// Resets run-unit state using a custom environment lookup function for `COB_SWITCH_0..36` and `COB_SET_DEBUG`.
    ///
    /// # Arguments
    ///
    /// * `args` - Process command-line argument vector.
    /// * `env_lookup` - Callback resolving environment variable names such as `"COB_SWITCH_1"` or `"COB_SET_DEBUG"`.
    pub fn initialize_with_env<F>(&mut self, args: Vec<String>, mut env_lookup: F)
    where
        F: FnMut(&str) -> Option<String>,
    {
        self.initialize_from_dyn_env(args, &mut env_lookup);
    }

    /// Non-generic implementation of [`Self::initialize_with_env`] avoiding monomorphized duplication.
    ///
    /// # Arguments
    ///
    /// * `args` - Process command-line argument vector.
    /// * `env_lookup` - Dynamic callback resolving environment variable names.
    fn initialize_from_dyn_env(
        &mut self,
        args: Vec<String>,
        env_lookup: &mut dyn FnMut(&str) -> Option<String>,
    ) {
        self.return_code = 0;
        self.call_depth = 0;
        self.switch_bits = 0;
        for idx in 0_u8..=36_u8 {
            if let Some(val) = env_lookup(&format!("COB_SWITCH_{idx}"))
                && parse_env_switch_state(&val) == SwitchState::On
            {
                self.switch_bits |= 1_u64 << idx;
            }
        }
        self.args = args;
        self.decimal_stack.begin();
        self.display_staging.clear();
        self.init_staging.clear();
        self.accept_env.reset();
        self.m10_staging.clear();
        self.exceptions.reset();
        self.debug_item.clear();
        if let Some(dbg_env) = env_lookup("COB_SET_DEBUG") {
            self.debug_item
                .set_runtime_switch(parse_cob_set_debug(&dbg_env));
        }
    }

    /// Clears transient run-unit state on `cob_tidy`.
    pub fn tidy(&mut self) {
        self.call_depth = 0;
        self.args.clear();
        self.decimal_stack.begin();
        self.display_staging.clear();
        self.init_staging.clear();
        self.accept_env.reset();
        self.m10_staging.clear();
        self.exceptions.reset();
        self.debug_item.clear();
    }

    /// Records the most recent ISO `EC-*` exception condition name (`""` clears exception status).
    ///
    /// # Arguments
    ///
    /// * `ec_name` - Static canonical `EC-*` identifier (e.g. `"EC-BOUND-SUBSCRIPT"`).
    pub fn record_exception(&mut self, ec_name: &'static str) {
        if let Ok(code) = ExceptionCode::from_cobol_name(ec_name) {
            self.exceptions.raise_code(code);
        } else {
            self.exceptions.clear_last_exception();
        }
    }

    /// Returns shared read-only access to the run-unit [`ExceptionRuntimeState`].
    #[must_use]
    pub const fn exceptions(&self) -> &ExceptionRuntimeState {
        &self.exceptions
    }

    /// Returns mutable access to the run-unit [`ExceptionRuntimeState`].
    pub const fn exceptions_mut(&mut self) -> &mut ExceptionRuntimeState {
        &mut self.exceptions
    }

    /// Returns shared read-only access to the run-unit [`DebugItemState`].
    #[must_use]
    pub const fn debug_item(&self) -> &DebugItemState {
        &self.debug_item
    }

    /// Returns mutable access to the run-unit [`DebugItemState`].
    pub const fn debug_item_mut(&mut self) -> &mut DebugItemState {
        &mut self.debug_item
    }

    /// Returns the most recently recorded ISO `EC-*` exception condition name (`""` when clear).
    #[must_use]
    pub fn last_exception(&self) -> &'static str {
        self.exceptions
            .last_record()
            .map_or("", |rec| rec.code.cobol_name())
    }

    /// Returns mutable access to the run-unit [`DecimalEvalStack`].
    pub const fn decimal_stack_mut(&mut self) -> &mut DecimalEvalStack {
        &mut self.decimal_stack
    }

    /// Returns mutable access to the run-unit [`DisplayStagingBuffer`].
    pub const fn display_staging_mut(&mut self) -> &mut DisplayStagingBuffer {
        &mut self.display_staging
    }

    /// Returns mutable access to the run-unit [`InitStagingState`].
    pub const fn init_staging_mut(&mut self) -> &mut InitStagingState {
        &mut self.init_staging
    }

    /// Returns mutable access to the run-unit [`M10StringInspectStaging`].
    pub const fn m10_staging_mut(&mut self) -> &mut M10StringInspectStaging {
        &mut self.m10_staging
    }

    /// Returns mutable access to the run-unit [`HeapTracker`].
    pub const fn heap_mut(&mut self) -> &mut HeapTracker {
        &mut self.heap
    }

    /// Flushes staged `DISPLAY` bytes into the specified [`EnvDisplayTarget`] (`ENVIRONMENT-NAME`, `ENVIRONMENT-VALUE`, etc.).
    ///
    /// # Arguments
    ///
    /// * `target` - Target register selector.
    pub fn flush_display_to_env_target(&mut self, target: EnvDisplayTarget) {
        let bytes = self.display_staging.take_bytes();
        self.accept_env
            .apply_env_display(target, &bytes, &mut self.args);
    }

    /// Resolves an environment variable `name` against in-process `SET ENVIRONMENT` overrides first,
    /// falling back to `std::env::var`.
    ///
    /// # Arguments
    ///
    /// * `name` - Environment variable name.
    #[must_use]
    pub fn lookup_env(&self, name: &str) -> Option<String> {
        self.accept_env
            .lookup_env_with(name, &mut |k| std::env::var(k).ok())
    }

    /// Executes an `ACCEPT` statement against this run-unit context.
    ///
    /// # Arguments
    ///
    /// * `source` - [`RuntimeAcceptSource`] selector.
    /// * `dest` - [`AcceptTargetDest`] (`Omitted` or `Field`).
    /// * `stdin_in` - Console input stream.
    /// * `stderr_out` - Warning stream for invalid `COB_CURRENT_DATE`.
    /// * `env_lookup` - Host environment variable lookup closure.
    ///
    /// # Errors
    ///
    /// Returns an error if `dest` field metadata is malformed or move conversion fails.
    pub fn execute_accept(
        &mut self,
        source: RuntimeAcceptSource,
        dest: AcceptTargetDest<'_>,
        stdin_in: &mut dyn BufRead,
        stderr_out: &mut dyn Write,
        env_lookup: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Result<AcceptFlowOutcome> {
        execute_runtime_accept(
            source,
            dest,
            &mut self.accept_env,
            &self.args,
            stdin_in,
            stderr_out,
            env_lookup,
        )
    }

    /// Reads the current `RETURN-CODE` special register.
    #[must_use]
    pub const fn return_code(&self) -> i32 {
        self.return_code
    }

    /// Updates the `RETURN-CODE` special register.
    ///
    /// # Arguments
    ///
    /// * `code` - New signed 32-bit return code value.
    pub fn set_return_code(&mut self, code: i32) {
        self.return_code = code;
    }

    /// Reads the current [`SwitchState`] of `SWITCH-<idx>` (`0..=36`).
    ///
    /// # Arguments
    ///
    /// * `idx` - Zero-based switch index (`0..=36`; indices `> 36` read as `Off`).
    #[must_use]
    pub const fn get_switch(&self, idx: u8) -> SwitchState {
        if idx <= 36 && ((self.switch_bits >> idx) & 1) == 1 {
            SwitchState::On
        } else {
            SwitchState::Off
        }
    }

    /// Updates the [`SwitchState`] of `SWITCH-<idx>` (`0..=36`).
    ///
    /// # Arguments
    ///
    /// * `idx` - Zero-based switch index (`0..=36`; indices `> 36` are ignored).
    /// * `state` - Desired [`SwitchState`].
    pub fn set_switch(&mut self, idx: u8, state: SwitchState) {
        if idx <= 36 {
            let mask = 1_u64 << idx;
            match state {
                SwitchState::On => self.switch_bits |= mask,
                SwitchState::Off => self.switch_bits &= !mask,
            }
        }
    }

    /// Increments active subprogram `CALL` depth before invoking a subprogram.
    pub fn enter_call(&mut self) {
        self.call_depth = self.call_depth.saturating_add(1);
    }

    /// Decrements active subprogram `CALL` depth upon returning to caller.
    pub fn leave_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// Returns active `CALL` nesting depth (`0` at top-level program).
    #[must_use]
    pub const fn call_depth(&self) -> u32 {
        self.call_depth
    }

    /// Returns captured CLI arguments from `initialize`.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Returns a reference to the process-wide [`CobolRuntimeContext`] mutex.
#[must_use]
pub fn global_runtime() -> &'static Mutex<CobolRuntimeContext> {
    /// Lazily initialized singleton runtime state.
    static INSTANCE: OnceLock<Mutex<CobolRuntimeContext>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(CobolRuntimeContext::new()))
}

/// Acquires the process-wide [`CobolRuntimeContext`] guard, recovering cleanly if poisoned.
fn lock_global_runtime() -> MutexGuard<'static, CobolRuntimeContext> {
    match global_runtime().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Executes `op` against the locked process-wide [`CobolRuntimeContext`], recovering cleanly if poisoned.
///
/// # Arguments
///
/// * `op` - Closure receiving `&mut CobolRuntimeContext`.
pub fn with_global_runtime<R, F>(op: F) -> R
where
    F: FnOnce(&mut CobolRuntimeContext) -> R,
{
    let mut guard = lock_global_runtime();
    op(&mut guard)
}

/// Trims ASCII space (`0x20`) bytes from `bytes` according to `direction`.
///
/// # Arguments
///
/// * `bytes` - Input byte slice.
/// * `direction` - Leading, trailing, or both-sides space stripping mode.
#[must_use]
pub fn trim_ascii_spaces(bytes: &[u8], direction: TrimDirection) -> &[u8] {
    let start = match direction {
        TrimDirection::Both | TrimDirection::Leading => {
            bytes.iter().position(|b| *b != b' ').unwrap_or(bytes.len())
        }
        TrimDirection::Trailing => 0,
    };
    let after_lead = bytes.get(start..).unwrap_or(&[]);
    match direction {
        TrimDirection::Both | TrimDirection::Trailing => {
            let end = after_lead
                .iter()
                .rposition(|b| *b != b' ')
                .map_or(0, |idx| idx.saturating_add(1));
            after_lead.get(..end).unwrap_or(&[])
        }
        TrimDirection::Leading => after_lead,
    }
}

/// Decodes a big-endian unsigned integer from `1..=8` bytes.
///
/// # Arguments
///
/// * `bytes` - Big-endian integer byte slice.
#[must_use]
pub fn decode_be_u64(bytes: &[u8]) -> u64 {
    let mut acc = 0_u64;
    for &b in bytes.iter().take(8) {
        acc = (acc << 8) | u64::from(b);
    }
    acc
}

/// Decodes a big-endian two's-complement signed integer from `1..=8` bytes.
///
/// # Arguments
///
/// * `bytes` - Big-endian signed integer byte slice.
#[must_use]
pub fn decode_be_i64(bytes: &[u8]) -> i64 {
    let Some(&first) = bytes.first() else {
        return 0;
    };
    let mut acc = if (first & 0x80) != 0 { -1_i64 } else { 0_i64 };
    for &b in bytes.iter().take(8) {
        acc = (acc << 8) | i64::from(b);
    }
    acc
}

/// Formats an integer with optional fixed decimal `scale` (`scale > 0` inserts `'.'`).
///
/// # Arguments
///
/// * `is_negative` - Whether a leading `'-'` sign should be emitted (`true` only when `< 0`).
/// * `abs_val` - Magnitude of the integer value.
/// * `scale` - Decimal fractional digits (`> 0` places decimal point, `< 0` appends zeros).
#[must_use]
fn format_scaled_magnitude(is_negative: bool, abs_val: u64, scale: i16) -> Vec<u8> {
    let mut digits = abs_val.to_string().into_bytes();
    if scale < 0 && abs_val != 0 {
        let extra_zeros = usize::from(scale.unsigned_abs());
        digits.extend(core::iter::repeat_n(b'0', extra_zeros));
    }
    let mut out = Vec::with_capacity(digits.len().saturating_add(4));
    if is_negative {
        out.push(b'-');
    }
    if scale > 0 {
        let frac_len = usize::try_from(scale).unwrap_or(0);
        if digits.len() <= frac_len {
            out.extend_from_slice(b"0.");
            let pad = frac_len.saturating_sub(digits.len());
            out.extend(core::iter::repeat_n(b'0', pad));
            out.extend_from_slice(&digits);
        } else {
            let split_at = digits.len().saturating_sub(frac_len);
            out.extend(digits.iter().take(split_at).copied());
            out.push(b'.');
            out.extend(digits.iter().skip(split_at).copied());
        }
    } else {
        out.extend_from_slice(&digits);
    }
    out
}

/// Decodes a native-endian signed integer from `1`, `2`, `4`, or `8` bytes.
///
/// # Arguments
///
/// * `bytes` - Native-endian integer byte slice.
#[must_use]
pub fn decode_ne_i64(bytes: &[u8]) -> i64 {
    match bytes.len() {
        1 => bytes.first().map_or(0, |&b| i64::from(b.cast_signed())),
        2 => <[u8; 2]>::try_from(bytes).map_or(0, |a| i64::from(i16::from_ne_bytes(a))),
        4 => <[u8; 4]>::try_from(bytes).map_or(0, |a| i64::from(i32::from_ne_bytes(a))),
        8 => <[u8; 8]>::try_from(bytes).map_or(0, i64::from_ne_bytes),
        _ => 0,
    }
}

/// Decodes a native-endian unsigned integer from `1`, `2`, `4`, or `8` bytes.
///
/// # Arguments
///
/// * `bytes` - Native-endian unsigned integer byte slice.
#[must_use]
pub fn decode_ne_u64(bytes: &[u8]) -> u64 {
    match bytes.len() {
        1 => bytes.first().map_or(0, |&b| u64::from(b)),
        2 => <[u8; 2]>::try_from(bytes).map_or(0, |a| u64::from(u16::from_ne_bytes(a))),
        4 => <[u8; 4]>::try_from(bytes).map_or(0, |a| u64::from(u32::from_ne_bytes(a))),
        8 => <[u8; 8]>::try_from(bytes).map_or(0, u64::from_ne_bytes),
        _ => 0,
    }
}

/// Formats a single [`RuntimeDisplayItem`] into output bytes.
///
/// # Arguments
///
/// * `item` - Operand descriptor to format.
#[must_use]
pub fn format_display_item(item: &RuntimeDisplayItem<'_>) -> Vec<u8> {
    match item.kind {
        FieldDisplayKind::RawBytes => item.bytes.to_vec(),
        FieldDisplayKind::SignedInt32 => {
            let val = decode_ne_i64(item.bytes);
            format_scaled_magnitude(val < 0, val.unsigned_abs(), item.scale)
        }
        FieldDisplayKind::UnsignedInt32 => {
            let val = decode_ne_u64(item.bytes);
            format_scaled_magnitude(false, val, item.scale)
        }
        FieldDisplayKind::SignedBigEndianInt => {
            let val = decode_be_i64(item.bytes);
            format_scaled_magnitude(val < 0, val.unsigned_abs(), item.scale)
        }
        FieldDisplayKind::UnsignedBigEndianInt => {
            let val = decode_be_u64(item.bytes);
            format_scaled_magnitude(false, val, item.scale)
        }
    }
}

/// Writes formatted `DISPLAY` items to `writer` and flushes when needed.
///
/// # Arguments
///
/// * `items` - Sequence of operands to emit in order.
/// * `advancing` - Trailing newline policy.
/// * `writer` - Destination byte stream.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if writing or flushing `writer` fails.
pub fn write_display_items<W: Write>(
    items: &[RuntimeDisplayItem<'_>],
    advancing: LineAdvancing,
    writer: &mut W,
) -> Result<()> {
    write_display_items_dyn(items, advancing, writer)
}

/// Non-generic inner implementation of [`write_display_items`].
///
/// # Arguments
///
/// * `items` - Sequence of operands to emit in order.
/// * `advancing` - Trailing newline policy.
/// * `writer` - Destination dynamic byte stream.
fn write_display_items_dyn(
    items: &[RuntimeDisplayItem<'_>],
    advancing: LineAdvancing,
    writer: &mut dyn Write,
) -> Result<()> {
    for item in items {
        let formatted = format_display_item(item);
        writer.write_all(&formatted).map_err(CoblangError::from)?;
    }
    if advancing == LineAdvancing::AdvanceLine {
        writer.write_all(&[0x0A]).map_err(CoblangError::from)?;
    }
    writer.flush().map_err(CoblangError::from)?;
    Ok(())
}
