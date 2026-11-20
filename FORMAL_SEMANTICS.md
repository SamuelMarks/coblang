# Formal Semantics & Core Lowering Invariants of `coblang`

This document specifies the formal mathematical models, memory-layout equations, fixed-point rounding operators, and control-flow lowering invariants implemented in `coblang`. It serves both as an authoritative specification for compiler contributors (`crates/coblang-*`) and as the formal foundation for the systems paper outlined in [`PAPER_OUTLINE.md`](PAPER_OUTLINE.md).

---

## 1. Provenance-Preserving Two-Phase Source-Text Algebra (`coblang-diag`, `coblang-pp`)

Let $\mathcal{F} = \{f_0, f_1, \dots, f_k\}$ be the indexed set of source files and copybooks (`FileId`). Each token $t \in \mathcal{T}$ carries a 4-tuple provenance span:

$$\mathrm{span}(t) = (\mathrm{file} \in \mathrm{FileId},\; b_{\mathrm{start}} \in \mathbb{N},\; b_{\mathrm{len}} \in \mathbb{N},\; e \in \mathrm{ExpansionId})$$

where `ExpansionId` forms a rooted tree $(E, \mathrm{parent}, e_{\mathrm{root}} = 0)$ with edge labels $\kappa \in \{\mathsf{Copybook}, \mathsf{ReplaceStmt}, \mathsf{CdfDefine}, \mathsf{CdfCond}\}$.

### Two-Phase Rewrite Composition
Per ISO/IEC 1989:2023 §7, source manipulation is **not** a single macro pass; it is the strict sequential composition:

$$\mathcal{S}_{\mathrm{expanded}} = \Phi_{\mathrm{REPLACE}}(\Phi_{\mathrm{CDF}}(\Phi_{\mathrm{COPY}}(\mathcal{S}_{\mathrm{raw}})))$$

1. **Reference-Format Projection**: For `SourceFormat::Fixed` with text margin $M$ (default $M = 72$) and tab stop $W$ (default $W = 8$), each physical line $\ell$ is projected onto `(Seq[1..6], Ind[7], AreaA[8..11], AreaB[12..M])` while maintaining an injective byte-offset map $\pi_\ell : \mathrm{Col} \longrightarrow \mathrm{ByteOffset}$ so diagnostics point to pre-tab raw bytes.
2. **Pseudo-Text Equivalence (`==...==`)**: Two token sequences $\alpha = (u_1, \dots, u_m)$ and $\beta = (v_1, \dots, v_m)$ match under COBOL text-word equivalence ($\alpha \equiv_{\mathrm{cob}} \beta$) iff $m$ is equal after eliding separator spaces, comments, and blank lines, and $\forall i \in [1, m]$, $\mathrm{fold}(u_i) = \mathrm{fold}(v_i)$ (preserving literal case while case-folding COBOL words).

---

## 2. Hierarchical Storage Layout, Aliasing, & `OCCURS DEPENDING ON` (`coblang-sema/src/layout.rs`)

A COBOL record hierarchy (`01` root with children at normalized levels `02..49`, plus `66 RENAMES` and `88` conditions) is a tree of storage descriptors.

### 2.1 Static Alignment & Zero-Copy Aliasing
For an elementary item $x$ with byte width $w(x)$ and alignment requirement $a(x) \in \{1, 2, 4, 8\}$ (induced by `SYNCHRONIZED` and `DialectConfig::binary_size_policy`), its aligned offset relative to current cursor $c$ is:

$$\mathrm{align}(c, a) = \mathrm{ceil}(c / a) \cdot a, \qquad \mathrm{slack}(c, a) = \mathrm{align}(c, a) - c$$

- **`REDEFINES` Invariant**: If entry $y$ specifies `REDEFINES` $x$, then $\mathrm{offset}(y) = \mathrm{offset}(x)$, and the parent cursor advances to $\max(\mathrm{end}(x), \mathrm{end}(y))$ under permissive `Dialect::Gnu` (or rejects $w(y) > w(x)$ on `01`/subordinate levels according to `DialectConfig`). Neither `REDEFINES` nor `66 RENAMES a THRU b` allocates backing bytes; both compile to zero-copy `cobol.field_view` slices $[\mathrm{offset}(a), \mathrm{end}(b))$ over the enclosing record buffer.

### 2.2 Complex `OCCURS DEPENDING ON` (`ODO`) Address & Length Equations
Let an access path (`HirPlace`) traverse $d$ table dimensions with 1-based subscripts $(i_1, \dots, i_d)$. In simple ISO `ODO`, element strides $s_k$ are compile-time constants and only the table's upper bound $N_{\mathrm{odo}}$ is dynamic:

$$\mathrm{addr}_{\mathrm{simple}}(B, o_0, \vec{i}) = B + o_0 + \sum_{k=1}^{d} (i_k - 1) \cdot s_k, \qquad \mathrm{valid\;iff\;} \forall k:\; 1 \le i_k \le N_k$$

Under `OdoComplexity::ComplexAllowed` (subordinate item following a variably-located `ODO` table or nested `ODO`), the byte size of group $G$ parameterized by runtime `ODO` object values $\vec{v}$ is a linear combination:

$$\mathrm{size}(G, \vec{v}) = C_0 + \sum_{j=1}^{r} C_j \cdot \mathrm{clamp}(v_j, L_j, U_j)$$

which `coblang-mlir` lowers directly into SSA `arith.muli` / `arith.addi` expressions feeding `cobol.field_view`.

---

## 3. Exact 38/76-Digit Fixed-Point Algebra & ISO 2023 Rounding (`coblang-decimal`)

### 3.1 Representation & Intermediate Precision
Every numeric decimal value is represented in `FixedDecimal` as $(m, s)$ where:

$$\mathrm{val}(m, s) = m \cdot 10^{-s}, \qquad m \in \mathrm{SignedInt256} \;(|m| < 2^{256} \approx 1.1579 \cdot 10^{77}), \quad s \in [-38, 38]$$

Because $10^{76} < 2^{256}$, the product of any two 38-digit COBOL operands ($|m_1|, |m_2| < 10^{38} \implies |m_1 \cdot m_2| < 10^{76}$) and power-of-10 scale alignment ($|m_1 \cdot 10^{\Delta s}| < 10^{76}$ for $\Delta s \le 38$) execute **exact, overflow-free, zero-heap-allocation arithmetic** in 4 limb words (`[u64; 4]`).

### 3.2 Soundness of the `i128` MLIR Fast-Path (`opt_fastpath.rs`)
Since $2^{127} - 1 \approx 1.7014 \cdot 10^{38} > 10^{38} - 1$, any signed decimal intermediate requiring $P \le 38$ decimal digits fits strictly inside a signed `i128` (`P <= 18` fits inside `i64`). `opt_fastpath.rs` statically proves when scale-aligned `ADD`/`SUBTRACT`/`MULTIPLY`/`DIVIDE` intermediates satisfy $P_{\mathrm{int}} \le 37$, lowering directly to LLVM `i64`/`i128` instructions with `@llvm.s{add,sub,mul}.with.overflow` hardware flag checks.

### 3.3 Formal Definition of All 8 ISO/IEC 1989:2023 Rounding Modes
When rescaling exact signed value $x = \mathrm{sgn}(x) \cdot ((Q \cdot D + R) / D)$ by positive divisor $D = 10^k$ ($Q \ge 0$, $0 \le R < D$), the rounded magnitude $Q'$ under `RoundingMode` $\mu$ is:

| ISO 2023 Mode (`RoundingMode`) | Magnitude Rule $Q'(Q, R, D, \mathrm{sgn})$ |
| :--- | :--- |
| `Truncation` | $Q$ |
| `AwayFromZero` | $Q + \mathbb{I}[R > 0]$ |
| `NearestAwayFromZero` (default `ROUNDED`) | $Q + \mathbb{I}[2R \ge D]$ |
| `NearestEven` | $Q + \mathbb{I}[2R > D \;\lor\; (2R = D \land Q \equiv 1 \pmod 2)]$ |
| `NearestTowardZero` | $Q + \mathbb{I}[2R > D]$ |
| `TowardGreater` ($+\infty$) | $Q + \mathbb{I}[R > 0 \land \mathrm{sgn} > 0]$ |
| `TowardLesser` ($-\infty$) | $Q + \mathbb{I}[R > 0 \land \mathrm{sgn} < 0]$ |
| `Prohibited` | $Q$ if $R = 0$; raises `SizeErrorStatus::Overflowed` (`EC-SIZE-TRUNCATION`) if $R > 0$ |

Post-rounding target storage check: if $Q' \ge 10^{p_{\mathrm{target}}}$ (where $p_{\mathrm{target}} \in [1, 38]$ is the target `PICTURE` digit count under `TruncateToPic`), `ON SIZE ERROR` fires and the destination buffer remains **unmodified** per standard atomic target semantics.

---

## 4. Operational Semantics of `PERFORM ... THRU`, Fallthrough, & `ALTER` (`cfg_perform.rs`)

Let a `PROCEDURE DIVISION` consist of an ordered sequence of paragraphs $P_0, P_1, \dots, P_{n-1}$. Unlike structured functions, each paragraph $P_i$ has **two** outgoing control edges at its epilogue:
1. If an active `PERFORM` range ends at $P_i$, control returns to the caller's continuation site $C_k$.
2. Otherwise, control **falls through sequentially** to $P_{i+1}$ (or implicit `GOBACK` after $P_{n-1}$).

### 4.1 Continuation-Slot & Hybrid Stack Machine
`coblang-mlir/src/cfg_perform.rs` lowers non-recursive / non-overlapping `PERFORM P_a THRU P_b` at call site $k$ with **zero heap allocation**:
- Each paragraph $P_b$ that appears as the range end of $M_b \ge 1$ `PERFORM` sites is assigned a local integer continuation slot $\sigma_b \in \{0, 1, \dots, M_b\}$ (`0` = inactive / fallthrough).
- Before branching to $P_a$, site $k$ stores $\sigma_b \leftarrow k$.
- At the epilogue of $P_b$:
  - Under **`PerformReturnStyle::OsVsContinuation`** (IBM OS/VS semantics): read $c \leftarrow \sigma_b$; if $c > 0$, reset $\sigma_b \leftarrow 0$ and `cf.switch` to return label $L_c$; else fall through to $P_{b+1}$.
  - Under **`PerformReturnStyle::IsoStack`** (ISO 2023 / recursive stack semantics): push frame $(b, k)$ onto a stack-allocated `PerformStack` $\Sigma$; at the epilogue of $P_i$, inspect $\mathrm{top}(\Sigma)$ — iff $\mathrm{top}(\Sigma).\mathrm{end\_para} = i$, pop $\Sigma$ and dispatch to $L_k$; else fall through to $P_{i+1}$.
- **`ALTER P_x TO PROCEED TO P_y`**: Paragraph $P_x$ (an alterable unconditional `GO TO` paragraph) holds a mutable target index $\alpha_x$ initialized to its syntactic target; `ALTER` assigns $\alpha_x \leftarrow y$, and $P_x$ dispatches via `cf.switch(%alpha_x)` in $O(1)$ time without indirect code modification.
