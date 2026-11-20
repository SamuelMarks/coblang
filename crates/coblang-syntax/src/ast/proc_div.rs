//! Typed AST nodes for the COBOL `PROCEDURE DIVISION`, expressions, conditions, and all 47 verbs.

use coblang_common::{SourceSpan, SymbolId};

use crate::ast::data_div::{GlobalVisibility, KeyOrder, ValueAtom};

/// Parameter passing convention (`BY REFERENCE`, `BY CONTENT`, `BY VALUE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PassingMode {
    /// Pass address of storage (`BY REFERENCE`, default).
    ByReference,
    /// Pass address of temporary copy (`BY CONTENT`).
    ByContent,
    /// Pass scalar value in register/stack (`BY VALUE`).
    ByValue,
}

/// Parameter optionality in `PROCEDURE DIVISION USING [OPTIONAL] param`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamOptionality {
    /// Argument must be supplied (`EC-PROGRAM-ARG-OMITTED` otherwise).
    Required,
    /// Caller may pass `OMITTED` (`IS OMITTED` testable).
    Optional,
}

/// Formal parameter in `PROCEDURE DIVISION USING ...` or `ENTRY ... USING ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsingParam {
    /// Parameter passing mode.
    pub mode: PassingMode,
    /// Parameter optionality.
    pub optionality: ParamOptionality,
    /// `LINKAGE SECTION` item symbol.
    pub name: SymbolId,
    /// Source span.
    pub span: SourceSpan,
}

/// `PROCEDURE DIVISION [USING ...] [RETURNING ...] [CHAINING ...]` header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcedureHeader {
    /// `USING` formal parameters.
    pub using_params: Vec<UsingParam>,
    /// `CHAINING` formal parameters.
    pub chaining_params: Vec<SymbolId>,
    /// Optional `RETURNING` / `GIVING` target item symbol.
    pub returning: Option<SymbolId>,
}

/// Monitored subject in a `USE FOR DEBUGGING ON ...` declarative clause (NIST `DB` module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugUseTarget {
    /// `ALL PROCEDURES`.
    AllProcedures(SourceSpan),
    /// `ALL [REFERENCES] [OF] identifier`.
    AllReferencesOf(DataRef),
    /// Named procedure, file, or identifier (`proc-name`, `file-name`, or `identifier`).
    Target(DataRef),
}

/// `USE` declarative trigger specification inside `DECLARATIVES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseClause {
    /// `USE [GLOBAL] AFTER STANDARD {EXCEPTION | ERROR} PROCEDURE ON {INPUT | OUTPUT | I-O | EXTEND | files/ec}`.
    AfterException {
        /// `GLOBAL` scope flag.
        global: GlobalVisibility,
        /// Target open-mode keywords, file names, or `EC-*` names.
        targets: Vec<SymbolId>,
    },
    /// `USE [GLOBAL] BEFORE REPORTING group-name`.
    BeforeReporting {
        /// `GLOBAL` scope flag.
        global: GlobalVisibility,
        /// Report group identifier.
        group: SymbolId,
        /// Source span of the `USE BEFORE REPORTING` clause.
        use_span: SourceSpan,
    },
    /// `USE FOR DEBUGGING ON ...` (NIST `DB` module).
    ForDebugging {
        /// Monitored procedure, file, `[ALL REFERENCES OF] identifier`, or `ALL PROCEDURES` targets.
        targets: Vec<DebugUseTarget>,
    },
}

/// Single declarative section inside `DECLARATIVES. ... END DECLARATIVES.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarativeSection {
    /// Section name.
    pub name: SymbolId,
    /// `USE` trigger clause.
    pub use_clause: UseClause,
    /// Sentences preceding the first named paragraph in the section.
    pub initial_sentences: Vec<Sentence>,
    /// Named paragraphs inside the declarative section.
    pub paragraphs: Vec<Paragraph>,
    /// Source span.
    pub span: SourceSpan,
}

/// Qualified COBOL identifier (`A IN B OF C`), ordered from innermost name `base` to outermost qualifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    /// Unqualified leaf identifier (`A`).
    pub base: SymbolId,
    /// Enclosing group/file/section qualifiers (`[B, C]`).
    pub qualifiers: Vec<SymbolId>,
    /// Source span.
    pub span: SourceSpan,
}

/// Subscript expression (`expr`, `ALL`, or relative index `IDX + 2` / `IDX - 1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptItem {
    /// Standard numeric/index expression or relative index arithmetic (`IDX + 1` parsed as `Expr::Add`).
    Expr(Expr),
    /// Intrinsic table slice wildcard (`ALL`).
    AllWildcard(SourceSpan),
}

/// 1-based `identifier(offset : [length])` reference modification slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefModSpec {
    /// 1-based starting character/byte offset expression.
    pub offset: Box<Expr>,
    /// Optional character/byte length expression.
    pub length: Option<Box<Expr>>,
    /// Source span.
    pub span: SourceSpan,
}

/// Prefix modifier on a [`DataRef`] (`Plain`, `ADDRESS OF`, `LENGTH OF`, `BYTE-LENGTH OF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataRefModifier {
    /// Direct storage reference.
    Plain,
    /// `ADDRESS OF identifier` pointer value/target.
    AddressOf,
    /// `LENGTH OF identifier` special register expression.
    LengthOf,
}

/// Qualified, subscripted, and/or reference-modified COBOL data reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRef {
    /// Optional `ADDRESS OF` / `LENGTH OF` modifier.
    pub modifier: DataRefModifier,
    /// Base and `IN`/`OF` qualifier chain.
    pub name: QualifiedName,
    /// Optional table subscripts.
    pub subscripts: Vec<SubscriptItem>,
    /// Optional `(offset : [length])` reference modification.
    pub refmod: Option<RefModSpec>,
    /// Source span.
    pub span: SourceSpan,
}

/// Binary arithmetic or concatenation operator in [`Expr::Binary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// Addition (`+`).
    Add,
    /// Subtraction (`-`).
    Sub,
    /// Multiplication (`*`).
    Mul,
    /// Division (`/`).
    Div,
    /// Exponentiation (`**`).
    Pow,
    /// String concatenation (`&`).
    Concat,
}

/// Unary arithmetic operator in [`Expr::Unary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// Unary `+`.
    Plus,
    /// Unary `-`.
    Minus,
}

/// Arithmetic, string, intrinsic, UDF, or inline-method COBOL expression AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Data item, special register, or `78` constant reference.
    Data(DataRef),
    /// Literal or figurative constant value.
    Literal {
        /// Literal value atom.
        atom: ValueAtom,
        /// Source span.
        span: SourceSpan,
    },
    /// `FUNCTION name([arg ...])` or bare repository-imported intrinsic/UDF call.
    FunctionCall {
        /// Function name symbol.
        name: SymbolId,
        /// Explicit `FUNCTION` keyword presence (`ExplicitKeyword` vs `BareRepository`).
        syntax: FunctionCallSyntax,
        /// Call arguments (`SubscriptItem` supports `ALL` table slices!).
        args: Vec<SubscriptItem>,
        /// Optional reference modification on alphanumeric/national function result!
        refmod: Option<RefModSpec>,
        /// Source span.
        span: SourceSpan,
    },
    /// Inline OO method call `receiver::"method-name"([args])`.
    InlineInvoke {
        /// Target object or class receiver.
        receiver: Box<Expr>,
        /// Method name symbol or string literal symbol.
        method: SymbolId,
        /// Arguments.
        args: Vec<CallArg>,
        /// Source span.
        span: SourceSpan,
    },
    /// Unary `+expr` or `-expr`.
    Unary {
        /// Operator (`Plus` or `Minus`).
        op: UnaryOp,
        /// Inner operand.
        operand: Box<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// Binary arithmetic or `&` concatenation expression.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
        /// Source span.
        span: SourceSpan,
    },
}

/// Whether a function call used the `FUNCTION` keyword or bare `REPOSITORY` syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionCallSyntax {
    /// `FUNCTION UPPER-CASE(X)`.
    ExplicitKeyword,
    /// `UPPER-CASE(X)` via `REPOSITORY. FUNCTION ALL INTRINSIC`.
    BareRepository,
}

impl Expr {
    /// Returns the [`SourceSpan`] of any [`Expr`] variant.
    #[must_use]
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::Data(d) => d.span,
            Self::Literal { span, .. }
            | Self::FunctionCall { span, .. }
            | Self::InlineInvoke { span, .. }
            | Self::Unary { span, .. }
            | Self::Binary { span, .. } => *span,
        }
    }
}

/// Relational comparison operator (`=`, `<>`, `<`, `<=`, `>`, `>=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelOp {
    /// `=` / `EQUAL TO`.
    Eq,
    /// `<>` / `NOT =` / `NOT EQUAL TO`.
    Ne,
    /// `<` / `LESS THAN`.
    Lt,
    /// `<=` / `LESS THAN OR EQUAL TO` / `NOT >`.
    Le,
    /// `>` / `GREATER THAN`.
    Gt,
    /// `>=` / `GREATER THAN OR EQUAL TO` / `NOT <`.
    Ge,
}

/// Polarity (`IS` vs `IS NOT`) on class, sign, and omitted conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NegationMode {
    /// Unnegated (`IS NUMERIC`).
    Positive,
    /// Negated (`IS NOT NUMERIC`).
    Negated,
}

/// Built-in or user-defined character class in `expr IS [NOT] <class>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClassCheckKind {
    /// `NUMERIC`.
    Numeric,
    /// `ALPHABETIC`.
    Alphabetic,
    /// `ALPHABETIC-LOWER`.
    AlphabeticLower,
    /// `ALPHABETIC-UPPER`.
    AlphabeticUpper,
    /// `KANJI` / `DBCS`.
    Kanji,
    /// User-defined `CLASS` name from `SPECIAL-NAMES`.
    Custom(SymbolId),
}

/// Algebraic sign check in `expr IS [NOT] {POSITIVE | NEGATIVE | ZERO}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignCheckKind {
    /// `> 0`.
    Positive,
    /// `< 0`.
    Negative,
    /// `= 0`.
    Zero,
}

/// Right-hand tree of an ANSI-85 abbreviated combined relation (`IF A = B OR > C AND NOT D`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbbrevRhs {
    /// Leaf `[NOT] [rel_op] expr` inheriting subject (and optional `rel_op`) from enclosing relation.
    Leaf {
        /// Optional `NOT` prefix before operator/object.
        negated: NegationMode,
        /// Optional explicit relational operator (inherits prior `RelOp` when `None`).
        op: Option<RelOp>,
        /// Right-hand comparison expression.
        object: Expr,
        /// Source span.
        span: SourceSpan,
    },
    /// Parenthesized or `AND`/`OR` combined abbreviated subtree.
    Binary {
        /// Logical connective (`And` or `Or`).
        logic: AbbrevLogicOp,
        /// Left abbreviated branch.
        lhs: Box<AbbrevRhs>,
        /// Right abbreviated branch.
        rhs: Box<AbbrevRhs>,
    },
}

/// Logical connective (`AND` vs `OR`) inside [`AbbrevRhs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AbbrevLogicOp {
    /// `AND`.
    And,
    /// `OR`.
    Or,
}

/// Conditional boolean expression AST (`IF`, `EVALUATE`, `PERFORM UNTIL`, `WHEN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Standard or abbreviated relation condition (`subject rel_op rhs` or `subject rel_op AbbrevRhs`).
    Relation {
        /// Left-hand subject expression.
        subject: Expr,
        /// Initial relational operator.
        op: RelOp,
        /// Right-hand expression or abbreviated `AND`/`OR` continuation tree.
        rhs: AbbrevRhs,
        /// Source span.
        span: SourceSpan,
    },
    /// Class condition (`expr IS [NOT] NUMERIC | ALPHABETIC | custom-class`).
    ClassCheck {
        /// Tested expression.
        subject: Expr,
        /// `NOT` polarity.
        polarity: NegationMode,
        /// Character class.
        class: ClassCheckKind,
        /// Source span.
        span: SourceSpan,
    },
    /// Sign condition (`expr IS [NOT] POSITIVE | NEGATIVE | ZERO`).
    SignCheck {
        /// Tested arithmetic expression.
        subject: Expr,
        /// `NOT` polarity.
        polarity: NegationMode,
        /// Sign category.
        sign: SignCheckKind,
        /// Source span.
        span: SourceSpan,
    },
    /// Formal parameter omission test (`param IS [NOT] OMITTED`).
    OmittedCheck {
        /// Tested `LINKAGE` item.
        target: DataRef,
        /// `NOT` polarity.
        polarity: NegationMode,
        /// Source span.
        span: SourceSpan,
    },
    /// Level-`88` condition-name or `SPECIAL-NAMES` switch-status condition reference.
    ConditionRef(DataRef),
    /// Boolean literal (`TRUE` / `FALSE` inside `EVALUATE`).
    BooleanConst {
        /// Truth value (`TruthValue::True` or `TruthValue::False`).
        value: TruthValue,
        /// Source span.
        span: SourceSpan,
    },
    /// Logical negation (`NOT cond`).
    Not {
        /// Inner condition.
        inner: Box<Condition>,
        /// Source span.
        span: SourceSpan,
    },
    /// Logical conjunction (`lhs AND rhs`).
    And {
        /// Left condition.
        lhs: Box<Condition>,
        /// Right condition.
        rhs: Box<Condition>,
        /// Source span.
        span: SourceSpan,
    },
    /// Logical disjunction (`lhs OR rhs`).
    Or {
        /// Left condition.
        lhs: Box<Condition>,
        /// Right condition.
        rhs: Box<Condition>,
        /// Source span.
        span: SourceSpan,
    },
}

/// Strongly-typed boolean constant (`TRUE` vs `FALSE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthValue {
    /// `TRUE`.
    True,
    /// `FALSE`.
    False,
}

/// Target variable of an arithmetic verb with optional `ROUNDED [MODE IS ...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundedTarget {
    /// Destination storage reference.
    pub target: DataRef,
    /// Rounding specification.
    pub rounding: RoundingSpec,
}

/// Rounding clause on an arithmetic destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingSpec {
    /// Truncate toward zero (or `DEFAULT ROUNDED` if set).
    Unrounded,
    /// `ROUNDED [MODE IS mode-sym]`.
    Rounded(Option<SymbolId>),
}

/// Paired `[ON] SIZE ERROR` / `NOT [ON] SIZE ERROR` imperative statement blocks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SizeErrorHandlers {
    /// Statements executed on overflow / division-by-zero.
    pub on_size_error: Vec<Stmt>,
    /// Statements executed when no size error occurred.
    pub not_on_size_error: Vec<Stmt>,
}

/// Paired `[ON] EXCEPTION` / `NOT [ON] EXCEPTION` or `OVERFLOW` / `AT END` / `INVALID KEY` handlers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventHandlers {
    /// Primary exception / overflow / at-end / invalid-key branch.
    pub on_event: Vec<Stmt>,
    /// Complementary `NOT ON ...` branch.
    pub not_on_event: Vec<Stmt>,
}

/// `CORRESPONDING` / `CORR` mode on `MOVE`, `ADD`, and `SUBTRACT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorrespondingMode {
    /// Standard elementary/group operation.
    Standard,
    /// `CORRESPONDING` / `CORR` by-name group operation.
    Corresponding,
}

/// `CALL` / `INVOKE` actual argument (`BY REFERENCE/CONTENT/VALUE expr` or `OMITTED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallArg {
    /// Value/reference argument with passing mode and optional `LENGTH OF` / `SIZE` modifier.
    Value {
        /// Passing mode (`ByReference`, `ByContent`, `ByValue`).
        mode: PassingMode,
        /// Argument expression.
        expr: Expr,
    },
    /// Explicit `OMITTED` placeholder.
    Omitted(SourceSpan),
}

/// Loop condition check position (`WITH TEST BEFORE` vs `WITH TEST AFTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerformTestPosition {
    /// Evaluate `UNTIL` before each iteration (`TEST BEFORE`, default).
    Before,
    /// Evaluate `UNTIL` after each iteration (`TEST AFTER`).
    After,
}

/// Single `VARYING` or `AFTER` counter clause in `PERFORM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerformVaryingClause {
    /// Loop variable or index target.
    pub counter: DataRef,
    /// Initial `FROM` expression.
    pub from: Expr,
    /// Step `BY` expression.
    pub by: Expr,
    /// Termination `UNTIL` condition.
    pub until: Condition,
}

/// Loop iteration control on inline or out-of-line `PERFORM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerformControl {
    /// Execute once (`PERFORM para-a [THRU para-b]` or `PERFORM ... END-PERFORM`).
    Once,
    /// Execute `n TIMES`.
    Times(Expr),
    /// Execute `[WITH TEST BEFORE|AFTER] UNTIL cond`.
    Until {
        /// Test position.
        test_pos: PerformTestPosition,
        /// Termination condition.
        condition: Condition,
    },
    /// Execute `[WITH TEST BEFORE|AFTER] VARYING ... [AFTER ...]`.
    Varying {
        /// Test position.
        test_pos: PerformTestPosition,
        /// Primary `VARYING` clause followed by zero or more `AFTER` clauses.
        clauses: Vec<PerformVaryingClause>,
    },
}

/// Selection subject in `EVALUATE subject [ALSO subject ...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluateSubject {
    /// Value expression (`EVALUATE WS-CODE`).
    Expr(Expr),
    /// Truth expression (`EVALUATE TRUE` / `EVALUATE FALSE` / `EVALUATE A > B`).
    Condition(Condition),
}

/// Single test object inside `WHEN obj [ALSO obj ...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluateObject {
    /// `ANY` wildcard matching any subject value.
    Any,
    /// `[NOT] low [THRU high]` value or range comparison.
    Range {
        /// Optional `NOT` negation.
        negated: NegationMode,
        /// Single value or lower bound.
        low: Expr,
        /// Optional inclusive `THRU` upper bound.
        high: Option<Expr>,
    },
    /// Condition predicate (`WHEN TRUE`, `WHEN FALSE`, `WHEN cond-88`, `WHEN X > 5`).
    Condition(Condition),
}

/// `WHEN` arm inside `EVALUATE` (one or more stacked `WHEN` tuples sharing a body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluateWhenBranch {
    /// Stacked `WHEN` tuple alternatives (`Vec<EvaluateObject>` matches `subjects.len()`).
    pub alternatives: Vec<Vec<EvaluateObject>>,
    /// Imperative body statements.
    pub body: Vec<Stmt>,
}

/// Source kind of an `ACCEPT` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptSource {
    /// Console / `SYSIN` / device mnemonic (`Option<SymbolId>`).
    InputDevice(Option<SymbolId>),
    /// System date/time/day register (`DATE`, `DATE YYYYMMDD`, `DAY`, `DAY YYYYDDD`, `DAY-OF-WEEK`, `TIME`, `MICROSECOND-TIME`).
    SystemClock(SymbolId),
    /// Command-line / environment (`ARGUMENT-NUMBER`, `ARGUMENT-VALUE`, `COMMAND-LINE`, `ENVIRONMENT-NAME`, `ENVIRONMENT-VALUE`).
    ProcessInfo(SymbolId),
    /// `ACCEPT screen-name` TUI form.
    Screen,
}

/// Line advancing policy on `DISPLAY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayAdvancing {
    /// Append newline (`WITH ADVANCING`, default).
    AdvanceLine,
    /// Suppress trailing newline (`WITH NO ADVANCING`).
    NoAdvancing,
}

/// Variant of an `EXIT` verb (`PROGRAM`, `PARAGRAPH`, `SECTION`, `PERFORM [CYCLE]`, `FUNCTION`, `METHOD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitKind {
    /// Bare `EXIT.` no-op endpoint paragraph marker.
    PlainEndpoint,
    /// `EXIT PROGRAM [RETURNING/GIVING expr | RAISING ...]`.
    Program(Option<Expr>),
    /// `EXIT PARAGRAPH`.
    Paragraph,
    /// `EXIT SECTION`.
    Section,
    /// `EXIT PERFORM` (break innermost inline `PERFORM`).
    PerformBreak,
    /// `EXIT PERFORM CYCLE` (continue next iteration of inline `PERFORM`).
    PerformCycle,
    /// `EXIT FUNCTION` (return from UDF).
    Function,
    /// `EXIT METHOD` (return from OO method).
    Method,
}

/// Optional `RETURNING` / `GIVING` destination in `CALL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallReturning {
    /// No `RETURNING` clause (`RETURN-CODE` updated normally).
    Implicit,
    /// `RETURNING OMITTED` / `RETURNING NOTHING` (preserves caller's `RETURN-CODE`).
    Omitted,
    /// `RETURNING target-ref`.
    Target(DataRef),
}

/// File open mode in `OPEN {INPUT | OUTPUT | I-O | EXTEND} file ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenMode {
    /// Read-only (`INPUT`).
    Input,
    /// Create/truncate write (`OUTPUT`).
    Output,
    /// Read-write in-place (`I-O`).
    Io,
    /// Append (`EXTEND`).
    Extend,
}

/// Sub-operation inside `SET` (`TO`, `UP BY`, `DOWN BY`, `TO TRUE`, `TO FALSE`, `TO ADDRESS OF`, `TO NULL`, `TO ENTRY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetAction {
    /// `SET target ... TO expr`.
    ToValue {
        /// Destination index/pointer/data references.
        targets: Vec<DataRef>,
        /// Source expression.
        value: Expr,
    },
    /// `SET prog-ptr ... TO {ENTRY | ADDRESS OF PROGRAM | ADDRESS OF ENTRY} expr`.
    ToEntry {
        /// Destination program-pointer or function-pointer references.
        targets: Vec<DataRef>,
        /// Entry module name expression (string literal or alphanumeric identifier).
        entry: Expr,
    },
    /// `SET idx ... {UP | DOWN} BY expr`.
    StepIndex {
        /// Destination index references.
        targets: Vec<DataRef>,
        /// Step direction (`Up` or `Down`).
        direction: StepDirection,
        /// Step amount expression.
        amount: Expr,
    },
    /// `SET cond-88 ... TO {TRUE | FALSE}`.
    ConditionState {
        /// Level-`88` condition references.
        targets: Vec<DataRef>,
        /// Desired boolean state.
        state: TruthValue,
    },
    /// `SET sw IS {ON | OFF}` hardware/software switch assignment.
    SwitchState {
        /// Switch mnemonic symbol.
        switch_name: SymbolId,
        /// `ON` (`True`) vs `OFF` (`False`).
        state: TruthValue,
    },
    /// `SET LAST EXCEPTION TO OFF` clearing active runtime exception state.
    LastExceptionOff,
    /// `SET ENVIRONMENT name [WITH] TO value` runtime environment variable mutation.
    Environment {
        /// Environment variable name expression.
        name: Expr,
        /// Desired environment variable value expression.
        value: Expr,
    },
    /// `SET screen-item ATTRIBUTE <attr> {ON | OFF} ...` (`GnuCOBOL` M19 screen attribute mutation).
    ScreenAttributes {
        /// Target `SCREEN SECTION` item reference.
        target: DataRef,
        /// Ordered attribute toggle specifications.
        toggles: Vec<super::report_screen::SetScreenAttrToggleAst>,
    },
}

/// Index step direction in `SET idx UP BY / DOWN BY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepDirection {
    /// `UP BY`.
    Up,
    /// `DOWN BY`.
    Down,
}

/// Allocation target unit on `ALLOCATE` (`ALLOCATE based-item` vs `ALLOCATE expr CHARACTERS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllocateTargetUnit {
    /// No explicit `CHARACTERS` keyword (`ALLOCATE based-item` or numeric expression).
    BasedOrDefault,
    /// Explicit `CHARACTERS` keyword (`ALLOCATE expr CHARACTERS`).
    ExplicitCharacters,
}

/// Initialization policy on `ALLOCATE` (`Uninitialized`, `INITIALIZED`, or `INITIALIZED TO expr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocateInitClause {
    /// No `INITIALIZED` clause (ISO 2023 Rules 8–9).
    Uninitialized,
    /// `INITIALIZED` without `TO` (ISO 2023 Rules 6–7).
    InitializedDefault,
    /// `INITIALIZED TO expr` (`GnuCOBOL` character-fill extension).
    InitializedTo(Expr),
}

/// Source group inside `STRING src ... DELIMITED BY {SIZE | delim}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringSourceGroup {
    /// Source operands concatenated under this delimiter.
    pub sources: Vec<Expr>,
    /// `None` for `DELIMITED BY SIZE`, `Some(expr)` for `DELIMITED BY expr`.
    pub delimiter: Option<Expr>,
}

/// Delimiter specification in `UNSTRING ... DELIMITED BY [ALL] expr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstringDelimiter {
    /// Whether `ALL` consecutive occurrences collapse as a single delimiter.
    pub all_occurrences: DuplicateMatchMode,
    /// Delimiter pattern expression.
    pub pattern: Expr,
}

/// Whether `ALL` consecutive delimiters are collapsed in `UNSTRING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicateMatchMode {
    /// Single delimiter match.
    Single,
    /// `ALL` consecutive matches collapsed.
    AllRepeated,
}

/// Destination receiver triplet in `UNSTRING ... INTO dst [DELIMITER IN del] [COUNT IN cnt]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstringTarget {
    /// Destination field.
    pub into: DataRef,
    /// Optional `DELIMITER IN` field.
    pub delimiter_in: Option<DataRef>,
    /// Optional `COUNT IN` field.
    pub count_in: Option<DataRef>,
}

/// Match qualifier in `INSPECT` (`CHARACTERS`, `ALL`, `LEADING`, `FIRST`, `TRAILING`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InspectQualifier {
    /// `CHARACTERS`.
    Characters,
    /// `ALL`.
    All,
    /// `LEADING`.
    Leading,
    /// `FIRST`.
    First,
    /// `TRAILING`.
    Trailing,
}

/// `BEFORE` / `AFTER INITIAL expr` window bound in `INSPECT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectBound {
    /// `Before` vs `After`.
    pub side: BoundSide,
    /// Bound pattern expression.
    pub pattern: Expr,
}

/// Side of an `INSPECT` window (`BEFORE INITIAL` vs `AFTER INITIAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundSide {
    /// `BEFORE [INITIAL]`.
    Before,
    /// `AFTER [INITIAL]`.
    After,
}

/// Sub-clause of an `INSPECT` verb (`TALLYING`, `REPLACING`, `CONVERTING`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectOperation {
    /// `TALLYING counter FOR {CHARACTERS | {ALL|LEADING|TRAILING} pat} [BEFORE/AFTER ...]`.
    Tallying {
        /// Counter destination.
        counter: DataRef,
        /// Match mode.
        qualifier: InspectQualifier,
        /// Pattern (`None` when `Characters`).
        pattern: Option<Expr>,
        /// Window bounds.
        bounds: Vec<InspectBound>,
    },
    /// `REPLACING {CHARACTERS BY rep | {ALL|LEADING|FIRST|TRAILING} pat BY rep} [BEFORE/AFTER ...]`.
    Replacing {
        /// Match mode.
        qualifier: InspectQualifier,
        /// Target pattern (`None` when `Characters`).
        pattern: Option<Expr>,
        /// Replacement expression.
        by: Expr,
        /// Window bounds.
        bounds: Vec<InspectBound>,
    },
    /// `CONVERTING from-chars TO to-chars [BEFORE/AFTER ...]`.
    Converting {
        /// Source alphabet string.
        from: Expr,
        /// Replacement alphabet string.
        to: Expr,
        /// Window bounds.
        bounds: Vec<InspectBound>,
    },
}

/// `SEARCH` vs `SEARCH ALL` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchMode {
    /// Sequential linear `SEARCH`.
    Serial,
    /// Binary `SEARCH ALL`.
    BinaryAll,
}

/// `WITH FILLER` mode on `INITIALIZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitializeFillerMode {
    /// Leave `FILLER` items untouched (default).
    ExcludeFiller,
    /// `WITH FILLER` initializes `FILLER` items too.
    IncludeFiller,
}

/// `TO VALUE` vs default category zero/space mode on `INITIALIZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InitializeValueMode {
    /// Category default (`0` / `SPACES`).
    CategoryDefault,
    /// `ALL TO VALUE` / `<cat> TO VALUE` restores Data Division `VALUE` clauses.
    DefinedValue,
}

/// Exhaustive COBOL statement AST covering all 47 ISO/IEC 1989:2023 + dialect verbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `ACCEPT dst [FROM ...]`
    Accept {
        /// Target variable (or screen item).
        target: DataRef,
        /// Source specification.
        source: AcceptSource,
        /// Optional extended `SCREEN` / CRT positioning and attribute clauses.
        screen_ext: Option<Box<super::report_screen::ScreenAcceptStmtAst>>,
        /// Optional `[NOT] ON EXCEPTION` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `ACCEPT OMITTED ...` single-key wait without target field (`Milestone M19`).
    AcceptOmitted(Box<super::report_screen::ScreenAcceptStmtAst>),
    /// `ADD a ... TO dst ...` / `ADD a ... TO b GIVING dst ...` / `ADD CORR grp1 TO grp2`
    Add {
        /// `Standard` vs `Corresponding`.
        mode: CorrespondingMode,
        /// Summand operands prior to `TO` / `GIVING`.
        operands: Vec<Expr>,
        /// Optional `TO` operand when `GIVING` is also used (`ADD A TO B GIVING C`).
        to_giving_mid: Option<Expr>,
        /// In-place `TO` targets (when `giving` is empty).
        to_targets: Vec<RoundedTarget>,
        /// `GIVING` result targets.
        giving_targets: Vec<RoundedTarget>,
        /// `ON SIZE ERROR` handlers.
        size_error: SizeErrorHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `ALLOCATE {expr CHARACTERS | data-name} [INITIALIZED [TO expr]] [RETURNING ptr]`
    Allocate {
        /// Character count expression or `BASED` item expression.
        target: Expr,
        /// Whether `CHARACTERS` keyword was explicitly specified (`ExplicitCharacters`) vs `BasedOrDefault`.
        unit: AllocateTargetUnit,
        /// Initialization clause (`Uninitialized`, `InitializedDefault`, or `InitializedTo(Expr)`).
        initialized: AllocateInitClause,
        /// Optional `RETURNING` pointer target.
        returning: Option<DataRef>,
        /// Source span.
        span: SourceSpan,
    },
    /// `ALTER para-1 TO [PROCEED TO] para-2 ...`
    Alter {
        /// `(altered_paragraph, new_target_paragraph)` pairs.
        pairs: Vec<(QualifiedName, QualifiedName)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `CALL target [USING ...] [RETURNING ...] [ON EXCEPTION/OVERFLOW ...]`
    Call {
        /// Static literal or dynamic identifier callee expression.
        callee: Expr,
        /// Argument list.
        args: Vec<CallArg>,
        /// `RETURNING` / `GIVING` disposition (`Implicit`, `Omitted`, or `Target`).
        returning: CallReturning,
        /// `ON EXCEPTION` / `ON OVERFLOW` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `CANCEL target ...`
    Cancel {
        /// Module name expressions to cancel.
        targets: Vec<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `CLOSE file ...`
    Close {
        /// Closed file connector symbols.
        files: Vec<SymbolId>,
        /// Per-file close disposition parallel to `files`.
        dispositions: Vec<CloseDispositionSpec>,
        /// Source span.
        span: SourceSpan,
    },
    /// `COMMIT`
    Commit {
        /// Source span.
        span: SourceSpan,
    },
    /// `COMPUTE dst [ROUNDED] ... = expr [ON SIZE ERROR ...]`
    Compute {
        /// Destination targets.
        targets: Vec<RoundedTarget>,
        /// Evaluated arithmetic expression.
        expr: Expr,
        /// `ON SIZE ERROR` handlers.
        size_error: SizeErrorHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `CONTINUE [AFTER expr SECONDS]`
    Continue {
        /// Optional GnuCOBOL/ISO delay expression.
        after_seconds: Option<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `DELETE file [RECORD] [INVALID KEY ...]` or `DELETE FILE file ...`
    Delete {
        /// Target file connector(s).
        files: Vec<SymbolId>,
        /// `Record` vs `PhysicalFile` (`DELETE FILE`).
        kind: DeleteKind,
        /// `INVALID KEY` / `EXCEPTION` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `DISPLAY expr ... [UPON dev] [WITH NO ADVANCING]`
    Display {
        /// Displayed expressions.
        operands: Vec<Expr>,
        /// Optional `UPON` device/mnemonic symbol.
        upon: Option<SymbolId>,
        /// Newline policy.
        advancing: DisplayAdvancing,
        /// Optional extended `SCREEN` / CRT positioning and per-item clauses.
        screen_ext: Option<Box<super::report_screen::ScreenDisplayStmtAst>>,
        /// Optional `ON EXCEPTION` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `DIVIDE a {INTO | BY} b [GIVING ...] [REMAINDER ...] [ON SIZE ERROR ...]`
    Divide {
        /// `Into` (`b / a`) vs `By` (`a / b`).
        orientation: DivideOrientation,
        /// First operand (`a`).
        lhs: Expr,
        /// Second operand (`b`, when `GIVING` is present).
        rhs: Option<Expr>,
        /// In-place `INTO` targets (when `GIVING` is absent).
        into_targets: Vec<RoundedTarget>,
        /// `GIVING` quotient targets.
        giving_targets: Vec<RoundedTarget>,
        /// Optional `REMAINDER` target.
        remainder: Option<DataRef>,
        /// `ON SIZE ERROR` handlers.
        size_error: SizeErrorHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `ENTRY "name" [USING ...]`
    Entry {
        /// Entry point literal symbol.
        entry_name: SymbolId,
        /// Formal parameters.
        using_params: Vec<UsingParam>,
        /// Source span.
        span: SourceSpan,
    },
    /// `EVALUATE subject [ALSO ...] WHEN ... [WHEN OTHER ...] END-EVALUATE`
    Evaluate {
        /// Evaluated subjects.
        subjects: Vec<EvaluateSubject>,
        /// `WHEN` branches.
        branches: Vec<EvaluateWhenBranch>,
        /// Optional `WHEN OTHER` fallback statements.
        when_other: Vec<Stmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// `EXHIBIT [NAMED] [CHANGED] expr ...`
    Exhibit {
        /// Exhibited expressions.
        operands: Vec<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `EXIT [PROGRAM | PARAGRAPH | SECTION | PERFORM [CYCLE] | FUNCTION | METHOD]`
    Exit {
        /// Exit target flavor.
        kind: ExitKind,
        /// Source span.
        span: SourceSpan,
    },
    /// `FREE ptr ...`
    Free {
        /// Pointer or `BASED` item targets to deallocate.
        targets: Vec<DataRef>,
        /// Source span.
        span: SourceSpan,
    },
    /// `GENERATE report-or-detail-name`
    Generate {
        /// Report or detail group identifier.
        target: SymbolId,
        /// Source span.
        span: SourceSpan,
    },
    /// `GO TO [proc ...] [DEPENDING ON expr]`
    GoTo {
        /// Target procedure names (0 for `ALTER`able bare `GO TO.`, 1 for unconditional, N for `DEPENDING ON`).
        targets: Vec<QualifiedName>,
        /// Optional 1-based `DEPENDING ON` selector expression.
        depending_on: Option<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `GOBACK [RETURNING expr]`
    Goback {
        /// Optional return status expression.
        returning: Option<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `IF cond [THEN] ... [ELSE ...] [END-IF]`
    If {
        /// Condition predicate.
        condition: Condition,
        /// `THEN` statements (`NEXT SENTENCE` represented as `Stmt::NextSentence`).
        then_body: Vec<Stmt>,
        /// `ELSE` statements.
        else_body: Vec<Stmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// `INITIALIZE target ... [WITH FILLER] [REPLACING ...] [TO VALUE]`
    Initialize {
        /// Target items.
        targets: Vec<DataRef>,
        /// `WITH FILLER` setting.
        filler: InitializeFillerMode,
        /// `CategoryDefault` vs `DefinedValue`.
        value_mode: InitializeValueMode,
        /// Optional `(category_sym, replacement_expr)` rules.
        replacing: Vec<(SymbolId, Expr)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `INITIATE report-name ...`
    Initiate {
        /// Report descriptors to initiate.
        reports: Vec<SymbolId>,
        /// Source span.
        span: SourceSpan,
    },
    /// `INSPECT target TALLYING / REPLACING / CONVERTING ...`
    Inspect {
        /// Inspected data item.
        target: DataRef,
        /// Ordered `TALLYING` / `REPLACING` / `CONVERTING` operations.
        operations: Vec<InspectOperation>,
        /// Source span.
        span: SourceSpan,
    },
    /// `INVOKE receiver "method" [USING ...] [RETURNING ...]`
    Invoke {
        /// Object or class receiver.
        receiver: Expr,
        /// Method name expression/literal.
        method: Expr,
        /// Arguments.
        args: Vec<CallArg>,
        /// Optional `RETURNING` target.
        returning: Option<DataRef>,
        /// Source span.
        span: SourceSpan,
    },
    /// `JSON GENERATE dst FROM src [COUNT IN cnt] ... [ON EXCEPTION ...]`
    JsonGenerate {
        /// Output JSON buffer.
        destination: DataRef,
        /// Source COBOL group/item.
        source: DataRef,
        /// Optional character/byte count target.
        count_in: Option<DataRef>,
        /// `NAME [OF] item [IS] {literal | OMITTED}` overrides.
        name_mappings: Vec<MarkupNameClause>,
        /// `SUPPRESS ...` clauses.
        suppress_clauses: Vec<MarkupSuppressClause>,
        /// `CONVERTING item TO [JSON] BOOLEAN ...` clauses.
        converting_clauses: Vec<JsonBoolClause>,
        /// Exception handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `JSON PARSE src INTO dst ... [ON EXCEPTION ...]`
    JsonParse {
        /// Input JSON text item.
        source: Expr,
        /// Target COBOL group/item.
        destination: DataRef,
        /// `[WITH] DETAIL` anomaly reporting mode.
        detail: JsonDetailPresence,
        /// `NAME [OF] item [IS] {literal | OMITTED}` overrides.
        name_mappings: Vec<MarkupNameClause>,
        /// `SUPPRESS ...` clauses.
        suppress_clauses: Vec<MarkupSuppressClause>,
        /// `CONVERTING item FROM [JSON] BOOLEAN ...` clauses.
        converting_clauses: Vec<JsonBoolClause>,
        /// Exception handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `MERGE sd-file ON {ASCENDING|DESCENDING} KEY ... USING f1 f2 ... {GIVING ... | OUTPUT PROCEDURE ...}`
    Merge {
        /// `SD` sort-merge file symbol.
        sd_file: SymbolId,
        /// Key specifications `(order, key_refs)`.
        keys: Vec<(KeyOrder, Vec<DataRef>)>,
        /// Optional `COLLATING SEQUENCE` alphabet.
        collating_sequence: Option<SymbolId>,
        /// Input `USING` files.
        using_files: Vec<SymbolId>,
        /// Output `GIVING` files.
        giving_files: Vec<SymbolId>,
        /// Optional `OUTPUT PROCEDURE` `(start, optional_thru)`.
        output_proc: Option<(QualifiedName, Option<QualifiedName>)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `MOVE [CORRESPONDING] src TO dst ...`
    Move {
        /// `Standard` vs `Corresponding`.
        mode: CorrespondingMode,
        /// Source expression or group.
        source: Expr,
        /// Destination target(s).
        targets: Vec<DataRef>,
        /// Source span.
        span: SourceSpan,
    },
    /// `MULTIPLY a BY b ... [GIVING dst ...] [ON SIZE ERROR ...]`
    Multiply {
        /// Multiplicand (`a`).
        lhs: Expr,
        /// Multiplier (`b`, when `GIVING` is present).
        rhs: Option<Expr>,
        /// In-place `BY` targets (when `GIVING` is empty).
        by_targets: Vec<RoundedTarget>,
        /// `GIVING` product targets.
        giving_targets: Vec<RoundedTarget>,
        /// `ON SIZE ERROR` handlers.
        size_error: SizeErrorHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `NEXT SENTENCE`
    NextSentence {
        /// Source span.
        span: SourceSpan,
    },
    /// `OPEN {INPUT | OUTPUT | I-O | EXTEND} file ...`
    Open {
        /// `(open_mode, file_name)` pairs.
        entries: Vec<(OpenMode, SymbolId)>,
        /// Per-entry open options parallel to `entries`.
        options: Vec<OpenOptionSpec>,
        /// Source span.
        span: SourceSpan,
    },
    /// Inline `PERFORM [control] body... END-PERFORM`
    PerformInline {
        /// Iteration control (`Once`, `Times`, `Until`, `Varying`).
        control: PerformControl,
        /// Loop body statements.
        body: Vec<Stmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// Out-of-line `PERFORM proc-1 [{THRU|THROUGH} proc-2] [control]`
    PerformOutofLine {
        /// Starting paragraph or section.
        start: QualifiedName,
        /// Optional inclusive ending paragraph or section.
        thru: Option<QualifiedName>,
        /// Iteration control.
        control: PerformControl,
        /// Source span.
        span: SourceSpan,
    },
    /// `RAISE [EXCEPTION ec-name | obj]`
    Raise {
        /// Optional raised exception or object symbol/expr.
        target: Option<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `READ file [NEXT | PREVIOUS] [RECORD] [INTO dst] [KEY IS key] [AT END / INVALID KEY ...]`
    Read {
        /// File connector symbol.
        file: SymbolId,
        /// `NextOrDefault`, `ExplicitNext`, `Previous`, or `Random`.
        direction: ReadDirection,
        /// Optional `INTO` working-storage destination.
        into: Option<DataRef>,
        /// Optional `KEY IS` reference.
        key: Option<DataRef>,
        /// Explicit record lock modifier (`WITH LOCK`, `WITH NO LOCK`, etc.).
        lock_mode: ReadLockSpec,
        /// `AT END` / `INVALID KEY` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `RELEASE record-name [FROM src]`
    Release {
        /// `SD` record item.
        record: DataRef,
        /// Optional `FROM` source expression.
        from: Option<Expr>,
        /// Source span.
        span: SourceSpan,
    },
    /// `RESUME [AT NEXT STATEMENT | AT proc-name]`
    Resume {
        /// Optional resume target procedure (`None` = `NEXT STATEMENT`).
        target: Option<QualifiedName>,
        /// Source span.
        span: SourceSpan,
    },
    /// `RETURN sd-file [RECORD] [INTO dst] AT END ... [NOT AT END ...]`
    Return {
        /// `SD` file connector symbol.
        sd_file: SymbolId,
        /// Optional `INTO` destination.
        into: Option<DataRef>,
        /// `AT END` / `NOT AT END` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `REWRITE record-name [FROM src] [INVALID KEY ...]` or `REWRITE FILE file-name FROM src`
    Rewrite {
        /// File record item (or file name when `target_mode == WriteTargetMode::FileName`).
        record: DataRef,
        /// Whether `REWRITE FILE file-name` syntax was used.
        target_mode: WriteTargetMode,
        /// Optional `FROM` source expression.
        from: Option<Expr>,
        /// `INVALID KEY` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `ROLLBACK`
    Rollback {
        /// Source span.
        span: SourceSpan,
    },
    /// `SEARCH [ALL] table [VARYING idx] [AT END ...] WHEN cond ...`
    Search {
        /// `Serial` vs `BinaryAll`.
        mode: SearchMode,
        /// Table item reference.
        table: DataRef,
        /// Optional `VARYING` index/identifier.
        varying: Option<DataRef>,
        /// Optional `AT END` statements.
        at_end: Vec<Stmt>,
        /// `(condition, statements)` `WHEN` arms.
        when_arms: Vec<(Condition, Vec<Stmt>)>,
        /// Source span.
        span: SourceSpan,
    },
    /// `SET ...` (indexes, pointers, `88` condition-names, switches, environment).
    Set {
        /// Sub-operations executed left-to-right.
        actions: Vec<SetAction>,
        /// Source span.
        span: SourceSpan,
    },
    /// File `SORT sd-file ON KEY ... {USING ... | INPUT PROCEDURE ...} {GIVING ... | OUTPUT PROCEDURE ...}`
    SortFile {
        /// `SD` file symbol.
        sd_file: SymbolId,
        /// Key specifications.
        keys: Vec<(KeyOrder, Vec<DataRef>)>,
        /// `WITH DUPLICATES IN ORDER` flag (`TruthValue::True` when specified).
        duplicates_in_order: TruthValue,
        /// Optional `COLLATING SEQUENCE` alphabet.
        collating_sequence: Option<SymbolId>,
        /// Input `USING` files.
        using_files: Vec<SymbolId>,
        /// Optional `INPUT PROCEDURE` `(start, optional_thru)`.
        input_proc: Option<(QualifiedName, Option<QualifiedName>)>,
        /// Output `GIVING` files.
        giving_files: Vec<SymbolId>,
        /// Optional `OUTPUT PROCEDURE` `(start, optional_thru)`.
        output_proc: Option<(QualifiedName, Option<QualifiedName>)>,
        /// Source span.
        span: SourceSpan,
    },
    /// In-memory table `SORT table-name [ON {ASCENDING|DESCENDING} KEY ...] [WITH DUPLICATES] [[COLLATING] SEQUENCE IS alpha]`
    SortTable {
        /// Table item reference.
        table: DataRef,
        /// Optional explicit keys (uses `OCCURS` keys when empty).
        keys: Vec<(KeyOrder, Vec<DataRef>)>,
        /// `WITH DUPLICATES IN ORDER`.
        duplicates_in_order: TruthValue,
        /// Optional `[COLLATING] SEQUENCE` alphabet/code-name symbol.
        collating_sequence: Option<SymbolId>,
        /// Source span.
        span: SourceSpan,
    },
    /// `START file [FIRST | LAST | KEY IS rel_op key [WITH SIZE/LENGTH n]] [INVALID KEY ...]`
    Start {
        /// File connector symbol.
        file: SymbolId,
        /// Optional `(RelOp, key_ref)` positioning relation.
        key_rel: Option<(RelOp, DataRef)>,
        /// Positioning mode (`KeyRelation`, `First`, or `Last`).
        position: StartPositionSpec,
        /// Optional `WITH SIZE/LENGTH expr` partial key byte length.
        key_length: Option<Expr>,
        /// `INVALID KEY` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `STOP {RUN [RETURNING/GIVING expr] | literal}`
    Stop {
        /// `StopRun(optional_status)` vs `StopLiteral(expr)`.
        kind: StopKind,
        /// Source span.
        span: SourceSpan,
    },
    /// `STRING src ... DELIMITED BY ... INTO dst [WITH POINTER ptr] [ON OVERFLOW ...]`
    StringVerb {
        /// Source groups.
        groups: Vec<StringSourceGroup>,
        /// Destination field.
        into: DataRef,
        /// Optional `WITH POINTER` 1-based position variable.
        pointer: Option<DataRef>,
        /// `ON OVERFLOW` / `NOT ON OVERFLOW` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `SUBTRACT a ... FROM dst ... [GIVING ...] [ON SIZE ERROR ...]` or `SUBTRACT CORR a FROM b`
    Subtract {
        /// `Standard` vs `Corresponding`.
        mode: CorrespondingMode,
        /// Subtrahend expressions.
        subtrahends: Vec<Expr>,
        /// Minuend expression when `GIVING` is used (`SUBTRACT A FROM B GIVING C`).
        from_giving_minuend: Option<Expr>,
        /// In-place `FROM` targets (when `GIVING` is empty).
        from_targets: Vec<RoundedTarget>,
        /// `GIVING` difference targets.
        giving_targets: Vec<RoundedTarget>,
        /// `ON SIZE ERROR` handlers.
        size_error: SizeErrorHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `SUPPRESS [PRINTING]`
    Suppress {
        /// Source span.
        span: SourceSpan,
    },
    /// `TERMINATE report-name ...`
    Terminate {
        /// Report descriptors to terminate.
        reports: Vec<SymbolId>,
        /// Source span.
        span: SourceSpan,
    },
    /// `TRANSFORM target CHARACTERS FROM from-alpha TO to-alpha`
    Transform {
        /// Transformed field.
        target: DataRef,
        /// Source alphabet expression.
        from: Expr,
        /// Replacement alphabet expression.
        to: Expr,
        /// Source span.
        span: SourceSpan,
    },
    /// `UNLOCK file [RECORD | RECORDS]`
    Unlock {
        /// Unlocked file connector symbol.
        file: SymbolId,
        /// Source span.
        span: SourceSpan,
    },
    /// `UNSTRING src [DELIMITED BY ...] INTO ... [WITH POINTER ptr] [TALLYING IN tally] [ON OVERFLOW ...]`
    Unstring {
        /// Source string field.
        source: DataRef,
        /// Delimiter patterns.
        delimiters: Vec<UnstringDelimiter>,
        /// Destination receiver fields.
        targets: Vec<UnstringTarget>,
        /// Optional `WITH POINTER` position variable.
        pointer: Option<DataRef>,
        /// Optional `TALLYING IN` field counter.
        tallying: Option<DataRef>,
        /// `ON OVERFLOW` / `NOT ON OVERFLOW` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `VALIDATE target ...`
    Validate {
        /// Validated data items.
        targets: Vec<DataRef>,
        /// Source span.
        span: SourceSpan,
    },
    /// `WRITE [FILE] record-or-file [FROM src] [{BEFORE|AFTER} ADVANCING ...] [AT EOP / INVALID KEY ...]`
    Write {
        /// Record data reference (or file name when `target_mode == WriteTargetMode::FileName`).
        record: DataRef,
        /// Whether `WRITE FILE file-name` syntax was used.
        target_mode: WriteTargetMode,
        /// Optional `FROM` source expression.
        from: Option<Expr>,
        /// Optional advancing specification `(Before|After, expr_or_page)`.
        advancing: Option<(BoundSide, Expr)>,
        /// Distinguishes `AT END-OF-PAGE` from `INVALID KEY` handler clauses.
        handler_kind: WriteHandlerKind,
        /// `AT END-OF-PAGE` / `INVALID KEY` handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `XML GENERATE dst FROM src [COUNT IN cnt] ... [ON EXCEPTION ...]`
    XmlGenerate {
        /// Output XML buffer.
        destination: DataRef,
        /// Source COBOL group/item.
        source: DataRef,
        /// Optional count output variable.
        count_in: Option<DataRef>,
        /// Optional `[WITH] ENCODING expr` specification.
        encoding: Option<Box<Expr>>,
        /// `[WITH] XML-DECLARATION` presence.
        xml_declaration: XmlDeclPresence,
        /// `[WITH] ATTRIBUTES` presence.
        attributes: XmlAttrPresence,
        /// Optional `NAMESPACE [IS] expr` URI.
        namespace: Option<Box<Expr>>,
        /// Optional `NAMESPACE-PREFIX [IS] expr` prefix.
        namespace_prefix: Option<Box<Expr>>,
        /// `NAME [OF] item [IS] {literal | OMITTED}` overrides.
        name_mappings: Vec<MarkupNameClause>,
        /// `TYPE [OF] item [IS] {ATTRIBUTE | ELEMENT | CONTENT}` overrides.
        type_mappings: Vec<XmlTypeClause>,
        /// `SUPPRESS ...` clauses.
        suppress_clauses: Vec<MarkupSuppressClause>,
        /// Exception handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
    /// `XML PARSE src PROCESSING PROCEDURE proc-1 [THRU proc-2] [ON EXCEPTION ...]`
    XmlParse {
        /// Input XML document expression.
        source: Expr,
        /// Optional `[WITH] ENCODING expr` specification.
        encoding: Option<Expr>,
        /// Optional `RETURNING NATIONAL` mode.
        returning_national: XmlNationalReturn,
        /// Optional `VALIDATING [WITH] [FILE] expr` schema target.
        validating: Option<Expr>,
        /// Processing procedure `(start, optional_thru)`.
        processing_proc: (QualifiedName, Option<QualifiedName>),
        /// Exception handlers.
        handlers: EventHandlers,
        /// Source span.
        span: SourceSpan,
    },
}

impl Stmt {
    /// Returns the enclosing [`SourceSpan`] of any [`Stmt`] variant.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Accept { span, .. }
            | Self::Add { span, .. }
            | Self::Allocate { span, .. }
            | Self::Alter { span, .. }
            | Self::Call { span, .. }
            | Self::Cancel { span, .. }
            | Self::Close { span, .. }
            | Self::Commit { span }
            | Self::Compute { span, .. }
            | Self::Continue { span, .. }
            | Self::Delete { span, .. }
            | Self::Display { span, .. }
            | Self::Divide { span, .. }
            | Self::Entry { span, .. }
            | Self::Evaluate { span, .. }
            | Self::Exhibit { span, .. }
            | Self::Exit { span, .. }
            | Self::Free { span, .. }
            | Self::Generate { span, .. }
            | Self::GoTo { span, .. }
            | Self::Goback { span, .. }
            | Self::If { span, .. }
            | Self::Initialize { span, .. }
            | Self::Initiate { span, .. }
            | Self::Inspect { span, .. }
            | Self::Invoke { span, .. }
            | Self::JsonGenerate { span, .. }
            | Self::JsonParse { span, .. }
            | Self::Merge { span, .. }
            | Self::Move { span, .. }
            | Self::Multiply { span, .. }
            | Self::NextSentence { span }
            | Self::Open { span, .. }
            | Self::PerformInline { span, .. }
            | Self::PerformOutofLine { span, .. }
            | Self::Raise { span, .. }
            | Self::Read { span, .. }
            | Self::Release { span, .. }
            | Self::Resume { span, .. }
            | Self::Return { span, .. }
            | Self::Rewrite { span, .. }
            | Self::Rollback { span }
            | Self::Search { span, .. }
            | Self::Set { span, .. }
            | Self::SortFile { span, .. }
            | Self::SortTable { span, .. }
            | Self::Start { span, .. }
            | Self::Stop { span, .. }
            | Self::StringVerb { span, .. }
            | Self::Subtract { span, .. }
            | Self::Suppress { span }
            | Self::Terminate { span, .. }
            | Self::Transform { span, .. }
            | Self::Unlock { span, .. }
            | Self::Unstring { span, .. }
            | Self::Validate { span, .. }
            | Self::Write { span, .. }
            | Self::XmlGenerate { span, .. }
            | Self::XmlParse { span, .. } => *span,
            Self::AcceptOmitted(spec) => spec.span,
        }
    }
}

/// `[WITH] XML-DECLARATION` presence on `XML GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlDeclPresence {
    /// XML declaration prolog omitted.
    Omit,
    /// `[WITH] XML-DECLARATION` specified.
    Include,
}

/// `[WITH] ATTRIBUTES` presence on `XML GENERATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlAttrPresence {
    /// Elementary children default to child elements.
    ElementsOnly,
    /// `[WITH] ATTRIBUTES` specified.
    WithAttributes,
}

/// `RETURNING NATIONAL` mode on `XML PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlNationalReturn {
    /// Default alphanumeric event delivery.
    Alphanumeric,
    /// `RETURNING NATIONAL` specified.
    National,
}

/// `[WITH] DETAIL` presence on `JSON PARSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonDetailPresence {
    /// Standard anomaly handling.
    Standard,
    /// `[WITH] DETAIL` specified.
    WithDetail,
}

/// Node placement override in `XML GENERATE ... TYPE OF item IS ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XmlNodeTypeClause {
    /// `ATTRIBUTE`.
    Attribute,
    /// `ELEMENT`.
    Element,
    /// `CONTENT`.
    Content,
}

/// Replacement target in `NAME [OF] item [IS] {literal | OMITTED}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkupNameTarget {
    /// Renamed XML/JSON tag/property string (`SymbolId` in interner).
    Literal(SymbolId),
    /// `OMITTED` anonymous wrapper suppression.
    Omitted,
}

/// Single `NAME [OF] item [IS] {literal | OMITTED}` mapping entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkupNameClause {
    /// Referenced COBOL data item.
    pub target: DataRef,
    /// Replacement literal or `OMITTED`.
    pub override_name: MarkupNameTarget,
    /// Source span of the mapping entry.
    pub span: SourceSpan,
}

/// Single `TYPE [OF] item [IS] {ATTRIBUTE | ELEMENT | CONTENT}` entry on `XML GENERATE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTypeClause {
    /// Referenced COBOL data item.
    pub target: DataRef,
    /// Requested XML node placement.
    pub node_type: XmlNodeTypeClause,
    /// Source span of the mapping entry.
    pub span: SourceSpan,
}

/// Figurative predicate atom in `SUPPRESS ... WHEN ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkupSuppressCond {
    /// `ZERO` / `ZEROS` / `ZEROES`.
    Zero,
    /// `SPACE` / `SPACES`.
    Space,
    /// `LOW-VALUE` / `LOW-VALUES`.
    LowValue,
    /// `HIGH-VALUE` / `HIGH-VALUES`.
    HighValue,
}

/// Category selector in `SUPPRESS EVERY [NUMERIC | NONNUMERIC] ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkupSuppressGenericKind {
    /// Both numeric and non-numeric items.
    Any,
    /// `NUMERIC` items only.
    Numeric,
    /// `NONNUMERIC` items only.
    Nonnumeric,
}

/// XML node placement filter in `SUPPRESS EVERY ... [ELEMENT | ATTRIBUTE | CONTENT]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkupSuppressNodeKind {
    /// Any XML/JSON node kind.
    Any,
    /// `ELEMENT` only.
    Element,
    /// `ATTRIBUTE` only.
    Attribute,
    /// `CONTENT` only.
    Content,
}

/// Subject of a `SUPPRESS` subclause (`item` vs generic `EVERY ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkupSuppressSubject {
    /// Explicit field `SUPPRESS item [WHEN ...]`.
    Field(DataRef),
    /// Generic `SUPPRESS [EVERY [NUMERIC|NONNUMERIC] [ELEMENT|ATTRIBUTE|CONTENT]] WHEN ...`.
    Generic {
        /// Category filter.
        category: MarkupSuppressGenericKind,
        /// Node placement filter.
        node_kind: MarkupSuppressNodeKind,
    },
}

/// Single `SUPPRESS` subclause on `XML GENERATE`, `JSON GENERATE`, or `JSON PARSE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkupSuppressClause {
    /// Specific field or generic `EVERY` filter.
    pub subject: MarkupSuppressSubject,
    /// `OR`-combined figurative conditions (empty means unconditional suppression!).
    pub conditions: Vec<MarkupSuppressCond>,
    /// Source span of the subclause.
    pub span: SourceSpan,
}

/// Single `CONVERTING item TO/FROM [JSON] BOOLEAN [USING cond [AND false-lit]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonBoolClause {
    /// Referenced COBOL boolean/flag item.
    pub target: DataRef,
    /// Optional level-88 condition or truth literal symbol.
    pub cond_sym: Option<SymbolId>,
    /// Optional explicit false literal symbol following `AND`.
    pub false_sym: Option<SymbolId>,
    /// Source span of the entry.
    pub span: SourceSpan,
}

/// `DELETE record` vs `DELETE FILE file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeleteKind {
    /// `DELETE file [RECORD]`.
    Record,
    /// `DELETE FILE file ...`.
    PhysicalFile,
}

/// `DIVIDE a INTO b` vs `DIVIDE a BY b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DivideOrientation {
    /// `DIVIDE divisor INTO dividend`.
    Into,
    /// `DIVIDE dividend BY divisor`.
    By,
}

/// `READ` traversal mode (`NextOrDefault`, `ExplicitNext`, `Previous`, or `Random`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadDirection {
    /// Omitted `NEXT`/`PREVIOUS`/`KEY` (`READ file`).
    NextOrDefault,
    /// Explicit `READ file NEXT`.
    ExplicitNext,
    /// Explicit `READ file PREVIOUS`.
    Previous,
    /// Explicit random read (`KEY IS`).
    Random,
}

/// Per-statement record lock modifier on `READ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadLockSpec {
    /// Governed by the connector's `LOCK MODE`.
    Default,
    /// `READ ... WITH LOCK`.
    WithLock,
    /// `READ ... WITH KEPT LOCK`.
    WithKeptLock,
    /// `READ ... WITH NO LOCK`.
    WithNoLock,
    /// `READ ... IGNORING LOCK`.
    IgnoreLock,
}

/// Per-entry option on `OPEN`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpenOptionSpec {
    /// Standard `OPEN`.
    Standard,
    /// `OPEN ... WITH NO REWIND` (`"07"` on non-reel disk files).
    WithNoRewind,
    /// `OPEN INPUT ... REVERSED`.
    Reversed,
    /// `OPEN ... WITH LOCK` (exclusive file lock).
    WithLock,
    /// `OPEN ... SHARING WITH ALL OTHER`.
    SharingAllOther,
    /// `OPEN ... SHARING WITH NO OTHER` (exclusive file lock).
    SharingNoOther,
    /// `OPEN ... SHARING WITH READ ONLY`.
    SharingReadOnly,
}

/// Per-file disposition on `CLOSE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseDispositionSpec {
    /// Standard `CLOSE file`.
    Standard,
    /// `CLOSE file WITH LOCK` (permanent run-unit lock `"38"`).
    WithLock,
    /// `CLOSE file WITH NO REWIND` (`"07"` on non-reel disk files).
    WithNoRewind,
    /// `CLOSE file REEL/UNIT` (`"07"` on non-reel disk files).
    ReelUnit,
    /// `CLOSE file REEL/UNIT FOR REMOVAL` (`"07"` on non-reel disk files).
    ReelUnitForRemoval,
}

/// Positioning selector on `START`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartPositionSpec {
    /// Standard `START file [KEY IS rel_op key]`.
    KeyRelation,
    /// ISO 2023 `START file FIRST`.
    First,
    /// ISO 2023 `START file LAST`.
    Last,
}

/// Target subject kind of `WRITE` or `REWRITE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteTargetMode {
    /// Standard ISO `WRITE record-name` / `REWRITE record-name`.
    RecordName,
    /// GnuCOBOL/MF `WRITE FILE file-name FROM ...` / `REWRITE FILE file-name FROM ...`.
    FileName,
}

/// Distinguishes `AT END-OF-PAGE` from `INVALID KEY` handler clauses on `WRITE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteHandlerKind {
    /// No conditional handler clause specified.
    None,
    /// `[NOT] AT END-OF-PAGE` / `EOP`.
    EndOfPage,
    /// `[NOT] INVALID KEY`.
    InvalidKey,
}

/// `STOP RUN [RETURNING expr]` vs obsolete `STOP literal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopKind {
    /// `STOP RUN [RETURNING / WITH NORMAL|ERROR STATUS expr]`.
    Run(Option<Expr>),
    /// Obsolete `STOP literal` operator pause.
    Literal(Expr),
}

/// Period-terminated COBOL sentence (`stmt ... .`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    /// Ordered statements inside the sentence.
    pub statements: Vec<Stmt>,
    /// Source span of the sentence.
    pub span: SourceSpan,
}

/// Named paragraph inside `PROCEDURE DIVISION` or a `ProcedureSection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    /// Paragraph identifier.
    pub name: SymbolId,
    /// Sentences inside the paragraph.
    pub sentences: Vec<Sentence>,
    /// Source span of the paragraph header.
    pub span: SourceSpan,
}

/// Named section (`name SECTION [segment-no].`) inside `PROCEDURE DIVISION`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureSection {
    /// Section identifier.
    pub name: SymbolId,
    /// Optional COBOL-85 segment priority number (`00`..`99`).
    pub segment_priority: Option<u8>,
    /// Sentences preceding the first named paragraph in this section.
    pub initial_sentences: Vec<Sentence>,
    /// Named paragraphs within this section.
    pub paragraphs: Vec<Paragraph>,
    /// Source span of the section header.
    pub span: SourceSpan,
}

/// Complete `PROCEDURE DIVISION` AST node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcedureDivision {
    /// `USING` / `RETURNING` / `CHAINING` header.
    pub header: ProcedureHeader,
    /// `DECLARATIVES` sections.
    pub declaratives: Vec<DeclarativeSection>,
    /// Unsectioned top-level sentences preceding the first paragraph or section.
    pub initial_sentences: Vec<Sentence>,
    /// Unsectioned top-level paragraphs preceding the first section.
    pub paragraphs: Vec<Paragraph>,
    /// Named sections.
    pub sections: Vec<ProcedureSection>,
    /// Source span if explicitly present.
    pub span: Option<SourceSpan>,
}
