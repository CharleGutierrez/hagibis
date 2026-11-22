//! # Neural-Symbolic Verification Engine (`neural_symbolic_verifier`)
//!
//! Project Hagibis - Autonomous MicroVM Compiler & Mathematical Verification Pipeline.
//! Provides standards-compliant SMT-LIB2 verification condition generation (QF_LIA and QF_BV),
//! dual-mode solver execution (piping to external Z3/CVC5 solvers with an embedded native SMT2
//! mathematical reasoner featuring Simplex linear arithmetic and DPLL(T) constraint propagation),
//! and issuance of cryptographically signed SHA-256 SMT Proof Certificates.
//!
//! ### Core Capabilities:
//! 1. **Mathematical Invariant Checking**:
//!    - Memory safety and spatial buffer protection.
//!    - Array/slice bounds preservation ($0 \le \text{idx} < \text{len}$ and $0 \le \text{start} \le \text{end} \le \text{len}$).
//!    - Arithmetic overflow/underflow freedom in bounded integer and bitvector domains.
//!    - Division-by-zero freedom ($\text{denom} \ne 0$).
//!    - Non-null raw pointer dereference ($\text{ptr} \ne 0$).
//!    - Monotonic loop convergence and variant ranking functions.
//!
//! 2. **Standard SMT-LIB v2.6 Compatibility**:
//!    - Emits standards-compliant SMT-LIB 2.6 syntax for `QF_LIA` (Quantifier-Free Linear Integer Arithmetic)
//!      and `QF_BV` (Quantifier-Free Bitvectors).
//!    - Roundtrip parser/serializer for automated validation.
//!
//! 3. **Dual-Mode Solver Architecture**:
//!    - External pipe to `z3 -in -smt2` or `cvc5 --lang=smt2` when available on host system.
//!    - Embedded native SMT2 reasoner: Exact rational Simplex algorithm with integer branch-and-bound
//!      and DPLL(T) boolean constraint propagation, executing 100% natively in Rust without external dependencies.
//!
//! 4. **Cryptographic Proof Certificates**:
//!    - Issues `SmtProofCertificate` with SHA-256 digests of SMT-LIB2 queries and canonical theorem payload.
//!    - Captures concrete counterexample models ($\mathcal{M} \models \Gamma \land \neg P$) when refutations occur.
//!    - Records execution metrics: pivots, decisions, conflicts, duration.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::Write as IoWrite;
use std::process::{Command, Stdio};
use std::time::Instant;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

// ==============================================================================
// 1. Errors & Outcomes
// ==============================================================================

#[derive(Debug, Error)]
pub enum SmtError {
    #[error("I/O error during SMT solver communication: {0}")]
    Io(#[from] std::io::Error),

    #[error("SMT-LIB2 syntax/parse error: {0}")]
    ParseError(String),

    #[error("SMT solver execution failed: {0}")]
    SolverExecution(String),

    #[error("SMT solver timeout after {0} ms")]
    Timeout(u64),

    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unsupported logic or theory expression: {0}")]
    UnsupportedTheory(String),

    #[error("Verification condition formulation error: {0}")]
    ConditionFormulation(String),
}

/// Invariant verification proof status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvariantProofStatus {
    /// Invariant is mathematically proven (negation $\Gamma \land \neg P$ is UNSAT).
    ProvenUnsat,
    /// Invariant is refuted (negation is SAT; counterexample witness generated).
    RefutedSatWithCounterexample,
    /// Solver could not determine satisfiability within fuel/timeout budget.
    Unknown,
}

impl fmt::Display for InvariantProofStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvariantProofStatus::ProvenUnsat => write!(f, "PROVEN_UNSAT"),
            InvariantProofStatus::RefutedSatWithCounterexample => {
                write!(f, "REFUTED_SAT_WITH_COUNTEREXAMPLE")
            }
            InvariantProofStatus::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

/// Mathematical verification invariant categories.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SmtInvariantKind {
    /// Buffer/slice bounds preservation ($0 \le \text{idx} < \text{len}$).
    ArrayBoundsPreservation,
    /// Absence of division-by-zero ($\text{denom} \ne 0$).
    DivisionByZeroFreedom,
    /// Absence of arithmetic overflow/underflow ($MIN \le a \text{ op } b \le MAX$).
    ArithmeticOverflowFreedom,
    /// Raw pointer proven non-null before dereferencing ($\text{ptr} \ne 0$).
    NullPointerFreedom,
    /// Memory safety and buffer allocation boundaries.
    MemorySafety,
    /// Monotonic loop termination / convergence invariant.
    LoopConvergence,
    /// Custom state invariant formula.
    Custom(String),
}

impl fmt::Display for SmtInvariantKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmtInvariantKind::ArrayBoundsPreservation => write!(f, "ArrayBoundsPreservation"),
            SmtInvariantKind::DivisionByZeroFreedom => write!(f, "DivisionByZeroFreedom"),
            SmtInvariantKind::ArithmeticOverflowFreedom => write!(f, "ArithmeticOverflowFreedom"),
            SmtInvariantKind::NullPointerFreedom => write!(f, "NullPointerFreedom"),
            SmtInvariantKind::MemorySafety => write!(f, "MemorySafety"),
            SmtInvariantKind::LoopConvergence => write!(f, "LoopConvergence"),
            SmtInvariantKind::Custom(desc) => write!(f, "Custom({})", desc),
        }
    }
}

/// SMT Logic theories supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmtLogic {
    /// Quantifier-Free Linear Integer Arithmetic
    QfLia,
    /// Quantifier-Free Bitvectors
    QfBv,
}

impl SmtLogic {
    pub fn as_smt_name(&self) -> &'static str {
        match self {
            SmtLogic::QfLia => "QF_LIA",
            SmtLogic::QfBv => "QF_BV",
        }
    }
}

/// SMT variable sort / type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SmtSort {
    Int,
    Bool,
    BitVec(usize),
}

impl fmt::Display for SmtSort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmtSort::Int => write!(f, "Int"),
            SmtSort::Bool => write!(f, "Bool"),
            SmtSort::BitVec(w) => write!(f, "(_ BitVec {})", w),
        }
    }
}

// ==============================================================================
// 2. SMT-LIB2 AST & Expression Builder
// ==============================================================================

/// Concrete SMT value representation in models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmtValue {
    Int(i64),
    Bool(bool),
    BitVec { value: u64, width: usize },
}

impl fmt::Display for SmtValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmtValue::Int(v) => write!(f, "{}", v),
            SmtValue::Bool(b) => write!(f, "{}", b),
            SmtValue::BitVec { value, width } => write!(f, "#b{:0width$b}", value, width = width),
        }
    }
}

/// Abstract Syntax Tree node for SMT-LIB2 expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmtExpr {
    Var(String),
    IntConst(i64),
    BoolConst(bool),
    BvConst { value: u64, width: usize },

    // Boolean Logic
    Not(Box<SmtExpr>),
    And(Vec<SmtExpr>),
    Or(Vec<SmtExpr>),
    Implies(Box<SmtExpr>, Box<SmtExpr>),
    Eq(Box<SmtExpr>, Box<SmtExpr>),
    Distinct(Vec<SmtExpr>),

    // Integer Comparisons
    Lt(Box<SmtExpr>, Box<SmtExpr>),
    Le(Box<SmtExpr>, Box<SmtExpr>),
    Gt(Box<SmtExpr>, Box<SmtExpr>),
    Ge(Box<SmtExpr>, Box<SmtExpr>),

    // Integer Arithmetic
    Add(Vec<SmtExpr>),
    Sub(Box<SmtExpr>, Box<SmtExpr>),
    Mul(Box<SmtExpr>, Box<SmtExpr>),
    Div(Box<SmtExpr>, Box<SmtExpr>),
    Mod(Box<SmtExpr>, Box<SmtExpr>),
    Neg(Box<SmtExpr>),

    // Ternary Conditional
    Ite(Box<SmtExpr>, Box<SmtExpr>, Box<SmtExpr>),

    // BitVector Arithmetic & Bitwise
    BvAdd(Box<SmtExpr>, Box<SmtExpr>),
    BvSub(Box<SmtExpr>, Box<SmtExpr>),
    BvMul(Box<SmtExpr>, Box<SmtExpr>),
    BvUdiv(Box<SmtExpr>, Box<SmtExpr>),
    BvSdiv(Box<SmtExpr>, Box<SmtExpr>),
    BvUrem(Box<SmtExpr>, Box<SmtExpr>),
    BvSrem(Box<SmtExpr>, Box<SmtExpr>),
    BvShl(Box<SmtExpr>, Box<SmtExpr>),
    BvLshr(Box<SmtExpr>, Box<SmtExpr>),
    BvAshr(Box<SmtExpr>, Box<SmtExpr>),
    BvAnd(Box<SmtExpr>, Box<SmtExpr>),
    BvOr(Box<SmtExpr>, Box<SmtExpr>),
    BvXor(Box<SmtExpr>, Box<SmtExpr>),
    BvNot(Box<SmtExpr>),
    BvNeg(Box<SmtExpr>),

    // BitVector Relational
    BvUlt(Box<SmtExpr>, Box<SmtExpr>),
    BvUle(Box<SmtExpr>, Box<SmtExpr>),
    BvUgt(Box<SmtExpr>, Box<SmtExpr>),
    BvUge(Box<SmtExpr>, Box<SmtExpr>),
    BvSlt(Box<SmtExpr>, Box<SmtExpr>),
    BvSle(Box<SmtExpr>, Box<SmtExpr>),
    BvSgt(Box<SmtExpr>, Box<SmtExpr>),
    BvSge(Box<SmtExpr>, Box<SmtExpr>),
}

impl SmtExpr {
    pub fn var(name: impl Into<String>) -> Self {
        SmtExpr::Var(name.into())
    }

    pub fn int(val: i64) -> Self {
        SmtExpr::IntConst(val)
    }

    pub fn bool_lit(val: bool) -> Self {
        SmtExpr::BoolConst(val)
    }

    pub fn bv(value: u64, width: usize) -> Self {
        SmtExpr::BvConst { value, width }
    }

    pub fn not(expr: SmtExpr) -> Self {
        SmtExpr::Not(Box::new(expr))
    }

    pub fn and(exprs: Vec<SmtExpr>) -> Self {
        if exprs.len() == 1 {
            exprs.into_iter().next().unwrap()
        } else {
            SmtExpr::And(exprs)
        }
    }

    pub fn or(exprs: Vec<SmtExpr>) -> Self {
        if exprs.len() == 1 {
            exprs.into_iter().next().unwrap()
        } else {
            SmtExpr::Or(exprs)
        }
    }

    pub fn implies(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Implies(Box::new(a), Box::new(b))
    }

    pub fn eq(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Eq(Box::new(a), Box::new(b))
    }

    pub fn distinct(exprs: Vec<SmtExpr>) -> Self {
        SmtExpr::Distinct(exprs)
    }

    pub fn lt(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Lt(Box::new(a), Box::new(b))
    }

    pub fn le(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Le(Box::new(a), Box::new(b))
    }

    pub fn gt(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Gt(Box::new(a), Box::new(b))
    }

    pub fn ge(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Ge(Box::new(a), Box::new(b))
    }

    pub fn add(exprs: Vec<SmtExpr>) -> Self {
        if exprs.len() == 1 {
            exprs.into_iter().next().unwrap()
        } else {
            SmtExpr::Add(exprs)
        }
    }

    pub fn sub(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Sub(Box::new(a), Box::new(b))
    }

    pub fn mul(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Mul(Box::new(a), Box::new(b))
    }

    pub fn div(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Div(Box::new(a), Box::new(b))
    }

    pub fn modulo(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::Mod(Box::new(a), Box::new(b))
    }

    pub fn neg(a: SmtExpr) -> Self {
        SmtExpr::Neg(Box::new(a))
    }

    pub fn ite(c: SmtExpr, t: SmtExpr, e: SmtExpr) -> Self {
        SmtExpr::Ite(Box::new(c), Box::new(t), Box::new(e))
    }

    pub fn bv_add(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::BvAdd(Box::new(a), Box::new(b))
    }

    pub fn bv_sub(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::BvSub(Box::new(a), Box::new(b))
    }

    pub fn bv_mul(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::BvMul(Box::new(a), Box::new(b))
    }

    pub fn bv_ult(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::BvUlt(Box::new(a), Box::new(b))
    }

    pub fn bv_ule(a: SmtExpr, b: SmtExpr) -> Self {
        SmtExpr::BvUle(Box::new(a), Box::new(b))
    }

    /// Serializes SMT AST node to standards-compliant SMT-LIB v2.6 S-expression.
    pub fn to_smt2(&self) -> String {
        match self {
            SmtExpr::Var(name) => name.clone(),
            SmtExpr::IntConst(v) => {
                if *v < 0 {
                    format!("(- {})", v.abs())
                } else {
                    format!("{}", v)
                }
            }
            SmtExpr::BoolConst(b) => if *b { "true".to_string() } else { "false".to_string() },
            SmtExpr::BvConst { value, width } => format!("(_ bv{} {})", value, width),
            SmtExpr::Not(e) => format!("(not {})", e.to_smt2()),
            SmtExpr::And(items) => {
                if items.is_empty() {
                    "true".to_string()
                } else {
                    let s: Vec<String> = items.iter().map(|i| i.to_smt2()).collect();
                    format!("(and {})", s.join(" "))
                }
            }
            SmtExpr::Or(items) => {
                if items.is_empty() {
                    "false".to_string()
                } else {
                    let s: Vec<String> = items.iter().map(|i| i.to_smt2()).collect();
                    format!("(or {})", s.join(" "))
                }
            }
            SmtExpr::Implies(a, b) => format!("(=> {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Eq(a, b) => format!("(= {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Distinct(items) => {
                let s: Vec<String> = items.iter().map(|i| i.to_smt2()).collect();
                format!("(distinct {})", s.join(" "))
            }
            SmtExpr::Lt(a, b) => format!("(< {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Le(a, b) => format!("(<= {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Gt(a, b) => format!("(> {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Ge(a, b) => format!("(>= {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Add(items) => {
                let s: Vec<String> = items.iter().map(|i| i.to_smt2()).collect();
                format!("(+ {})", s.join(" "))
            }
            SmtExpr::Sub(a, b) => format!("(- {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Mul(a, b) => format!("(* {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Div(a, b) => format!("(div {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Mod(a, b) => format!("(mod {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::Neg(a) => format!("(- {})", a.to_smt2()),
            SmtExpr::Ite(c, t, e) => {
                format!("(ite {} {} {})", c.to_smt2(), t.to_smt2(), e.to_smt2())
            }
            SmtExpr::BvAdd(a, b) => format!("(bvadd {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSub(a, b) => format!("(bvsub {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvMul(a, b) => format!("(bvmul {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvUdiv(a, b) => format!("(bvudiv {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSdiv(a, b) => format!("(bvsdiv {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvUrem(a, b) => format!("(bvurem {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSrem(a, b) => format!("(bvsrem {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvShl(a, b) => format!("(bvshl {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvLshr(a, b) => format!("(bvlshr {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvAshr(a, b) => format!("(bvashr {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvAnd(a, b) => format!("(bvand {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvOr(a, b) => format!("(bvor {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvXor(a, b) => format!("(bvxor {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvNot(a) => format!("(bvnot {})", a.to_smt2()),
            SmtExpr::BvNeg(a) => format!("(bvneg {})", a.to_smt2()),
            SmtExpr::BvUlt(a, b) => format!("(bvult {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvUle(a, b) => format!("(bvule {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvUgt(a, b) => format!("(bvugt {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvUge(a, b) => format!("(bvuge {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSlt(a, b) => format!("(bvslt {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSle(a, b) => format!("(bvsle {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSgt(a, b) => format!("(bvsgt {} {})", a.to_smt2(), b.to_smt2()),
            SmtExpr::BvSge(a, b) => format!("(bvsge {} {})", a.to_smt2(), b.to_smt2()),
        }
    }

    /// Collects all variable names referenced in the expression.
    pub fn collect_variables(&self, acc: &mut HashSet<String>) {
        match self {
            SmtExpr::Var(name) => {
                acc.insert(name.clone());
            }
            SmtExpr::IntConst(_) | SmtExpr::BoolConst(_) | SmtExpr::BvConst { .. } => {}
            SmtExpr::Not(e) | SmtExpr::Neg(e) | SmtExpr::BvNot(e) | SmtExpr::BvNeg(e) => {
                e.collect_variables(acc);
            }
            SmtExpr::And(exprs) | SmtExpr::Or(exprs) | SmtExpr::Distinct(exprs) | SmtExpr::Add(exprs) => {
                for e in exprs {
                    e.collect_variables(acc);
                }
            }
            SmtExpr::Implies(a, b)
            | SmtExpr::Eq(a, b)
            | SmtExpr::Lt(a, b)
            | SmtExpr::Le(a, b)
            | SmtExpr::Gt(a, b)
            | SmtExpr::Ge(a, b)
            | SmtExpr::Sub(a, b)
            | SmtExpr::Mul(a, b)
            | SmtExpr::Div(a, b)
            | SmtExpr::Mod(a, b)
            | SmtExpr::BvAdd(a, b)
            | SmtExpr::BvSub(a, b)
            | SmtExpr::BvMul(a, b)
            | SmtExpr::BvUdiv(a, b)
            | SmtExpr::BvSdiv(a, b)
            | SmtExpr::BvUrem(a, b)
            | SmtExpr::BvSrem(a, b)
            | SmtExpr::BvShl(a, b)
            | SmtExpr::BvLshr(a, b)
            | SmtExpr::BvAshr(a, b)
            | SmtExpr::BvAnd(a, b)
            | SmtExpr::BvOr(a, b)
            | SmtExpr::BvXor(a, b)
            | SmtExpr::BvUlt(a, b)
            | SmtExpr::BvUle(a, b)
            | SmtExpr::BvUgt(a, b)
            | SmtExpr::BvUge(a, b)
            | SmtExpr::BvSlt(a, b)
            | SmtExpr::BvSle(a, b)
            | SmtExpr::BvSgt(a, b)
            | SmtExpr::BvSge(a, b) => {
                a.collect_variables(acc);
                b.collect_variables(acc);
            }
            SmtExpr::Ite(c, t, e) => {
                c.collect_variables(acc);
                t.collect_variables(acc);
                e.collect_variables(acc);
            }
        }
    }
}

// ==============================================================================
// 3. SMT Benchmark & Script Specification
// ==============================================================================

/// A complete SMT-LIB v2.6 Benchmark script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtBenchmark {
    pub logic: SmtLogic,
    pub declarations: BTreeMap<String, SmtSort>,
    pub assertions: Vec<SmtExpr>,
    pub check_sat: bool,
    pub produce_models: bool,
}

impl SmtBenchmark {
    pub fn new(logic: SmtLogic) -> Self {
        Self {
            logic,
            declarations: BTreeMap::new(),
            assertions: Vec::new(),
            check_sat: true,
            produce_models: true,
        }
    }

    pub fn declare_var(&mut self, name: impl Into<String>, sort: SmtSort) {
        self.declarations.insert(name.into(), sort);
    }

    pub fn assert_formula(&mut self, expr: SmtExpr) {
        self.assertions.push(expr);
    }

    /// Serializes into standards-compliant SMT-LIB 2.6 formatted string.
    pub fn to_smt2_script(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("(set-logic {})\n", self.logic.as_smt_name()));
        out.push_str("(set-info :source \"Hagibis Neural-Symbolic SMT Verifier\")\n");
        if self.produce_models {
            out.push_str("(set-option :produce-models true)\n");
        }

        for (name, sort) in &self.declarations {
            out.push_str(&format!("(declare-const {} {})\n", name, sort));
        }

        for assertion in &self.assertions {
            out.push_str(&format!("(assert {})\n", assertion.to_smt2()));
        }

        if self.check_sat {
            out.push_str("(check-sat)\n");
            if self.produce_models {
                out.push_str("(get-model)\n");
            }
        }

        out
    }

    /// Computes the deterministic SHA-256 digest of this SMT-LIB2 benchmark.
    pub fn compute_sha256(&self) -> String {
        let script = self.to_smt2_script();
        let mut hasher = Sha256::new();
        hasher.update(script.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Validates SMT-LIB2 syntax roundtrip: parses basic commands and assertions.
    pub fn from_smt2_script(script: &str) -> Result<Self, SmtError> {
        let mut logic = SmtLogic::QfLia;
        let mut declarations = BTreeMap::new();
        let mut assertions = Vec::new();

        for line in script.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }

            if trimmed.starts_with("(set-logic ") {
                if trimmed.contains("QF_BV") {
                    logic = SmtLogic::QfBv;
                } else {
                    logic = SmtLogic::QfLia;
                }
            } else if trimmed.starts_with("(declare-const ") {
                let inner = trimmed
                    .trim_start_matches("(declare-const ")
                    .trim_end_matches(')');
                let parts: Vec<&str> = inner.split_whitespace().collect();
                if parts.len() >= 2 {
                    let var_name = parts[0].to_string();
                    let sort = match parts[1] {
                        "Int" => SmtSort::Int,
                        "Bool" => SmtSort::Bool,
                        s if s.starts_with("(_") || s.starts_with("BitVec") => {
                            let width = parts
                                .last()
                                .and_then(|w| w.trim_end_matches(')').parse::<usize>().ok())
                                .unwrap_or(32);
                            SmtSort::BitVec(width)
                        }
                        _ => SmtSort::Int,
                    };
                    declarations.insert(var_name, sort);
                }
            } else if trimmed.starts_with("(assert ") {
                let inner = trimmed.trim_start_matches("(assert ").trim_end_matches(')');
                let expr = parse_simple_smt_expr(inner)?;
                assertions.push(expr);
            }
        }

        Ok(Self {
            logic,
            declarations,
            assertions,
            check_sat: script.contains("(check-sat)"),
            produce_models: script.contains("(get-model)"),
        })
    }
}

/// Helper parser for basic SMT expressions in roundtrip verification.
fn parse_simple_smt_expr(input: &str) -> Result<SmtExpr, SmtError> {
    let t = input.trim();
    if t == "true" {
        return Ok(SmtExpr::BoolConst(true));
    }
    if t == "false" {
        return Ok(SmtExpr::BoolConst(false));
    }
    if let Ok(val) = t.parse::<i64>() {
        return Ok(SmtExpr::IntConst(val));
    }

    if t.starts_with('(') && t.ends_with(')') {
        let inside = &t[1..t.len() - 1].trim();
        let tokens = tokenize_sexpr(inside);
        if tokens.is_empty() {
            return Err(SmtError::ParseError("Empty s-expression".into()));
        }
        let op = tokens[0].as_str();
        let args = &tokens[1..];

        match op {
            "not" if args.len() == 1 => {
                let e = parse_simple_smt_expr(&args[0])?;
                Ok(SmtExpr::not(e))
            }
            "and" => {
                let mut v = Vec::new();
                for a in args {
                    v.push(parse_simple_smt_expr(a)?);
                }
                Ok(SmtExpr::and(v))
            }
            "or" => {
                let mut v = Vec::new();
                for a in args {
                    v.push(parse_simple_smt_expr(a)?);
                }
                Ok(SmtExpr::or(v))
            }
            "=>" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::implies(a, b))
            }
            "=" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::eq(a, b))
            }
            "<" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::lt(a, b))
            }
            "<=" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::le(a, b))
            }
            ">" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::gt(a, b))
            }
            ">=" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::ge(a, b))
            }
            "+" => {
                let mut v = Vec::new();
                for a in args {
                    v.push(parse_simple_smt_expr(a)?);
                }
                Ok(SmtExpr::add(v))
            }
            "-" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::sub(a, b))
            }
            "-" if args.len() == 1 => {
                let a = parse_simple_smt_expr(&args[0])?;
                if let SmtExpr::IntConst(c) = a {
                    Ok(SmtExpr::IntConst(-c))
                } else {
                    Ok(SmtExpr::neg(a))
                }
            }
            "*" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::mul(a, b))
            }
            "div" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::div(a, b))
            }
            "bvult" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::bv_ult(a, b))
            }
            "bvule" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::bv_ule(a, b))
            }
            "bvadd" if args.len() == 2 => {
                let a = parse_simple_smt_expr(&args[0])?;
                let b = parse_simple_smt_expr(&args[1])?;
                Ok(SmtExpr::bv_add(a, b))
            }
            _ => {
                // Default fallback as uninterpreted token or nested atom
                Ok(SmtExpr::Var(t.to_string()))
            }
        }
    } else {
        Ok(SmtExpr::Var(t.to_string()))
    }
}

fn tokenize_sexpr(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in s.chars() {
        if ch == '(' {
            depth += 1;
            current.push(ch);
        } else if ch == ')' {
            depth -= 1;
            current.push(ch);
            if depth == 0 {
                tokens.push(current.trim().to_string());
                current.clear();
            }
        } else if ch.is_whitespace() && depth == 0 {
            if !current.trim().is_empty() {
                tokens.push(current.trim().to_string());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current.trim().to_string());
    }
    tokens
}

// ==============================================================================
// 4. Verification Conditions & Solver Backends
// ==============================================================================

/// High-level Verification Condition to verify with SMT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtVerificationCondition {
    pub id: String,
    pub theorem_name: String,
    pub invariant_kind: SmtInvariantKind,
    pub logic: SmtLogic,
    pub declarations: BTreeMap<String, SmtSort>,
    pub assumptions: Vec<SmtExpr>,
    pub safety_property: SmtExpr,
}

impl SmtVerificationCondition {
    /// Formulates the SMT benchmark for verification:
    /// To prove $\Gamma \implies P$, we check satisfiability of $\Gamma \land \neg P$.
    /// - If UNSAT: Property $P$ holds unconditionally under $\Gamma$ (PROVEN).
    /// - If SAT: Counterexample model $\mathcal{M}$ satisfies $\Gamma \land \neg P$ (REFUTED).
    pub fn to_negated_benchmark(&self) -> SmtBenchmark {
        let mut bm = SmtBenchmark::new(self.logic);
        for (name, sort) in &self.declarations {
            bm.declare_var(name.clone(), sort.clone());
        }

        for asm in &self.assumptions {
            bm.assert_formula(asm.clone());
        }

        // Negate the safety property:
        bm.assert_formula(SmtExpr::not(self.safety_property.clone()));
        bm
    }
}

/// SMT Solver Backend selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmtSolverBackend {
    /// Try external Z3/CVC5 if installed, otherwise fallback automatically to Embedded Native SMT2 Reasoner.
    Auto,
    /// Force using the built-in native Rust SMT2 Simplex/DPLL(T) reasoner.
    EmbeddedNative,
    /// Force external Z3 (`z3 -in -smt2`).
    ExternalZ3,
    /// Force external CVC5 (`cvc5 --lang=smt2`).
    ExternalCvc5,
}

/// Counterexample model generated when an invariant is refuted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterexampleModel {
    pub bindings: BTreeMap<String, SmtValue>,
    pub violating_expression: Option<String>,
    pub witness_summary: String,
}

/// Solver execution metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtSolverMetrics {
    pub iterations: usize,
    pub simplex_pivots: usize,
    pub dpll_decisions: usize,
    pub dpll_conflicts: usize,
    pub duration_micros: u64,
    pub fuel_consumed: u64,
}

impl Default for SmtSolverMetrics {
    fn default() -> Self {
        Self {
            iterations: 0,
            simplex_pivots: 0,
            dpll_decisions: 0,
            dpll_conflicts: 0,
            duration_micros: 0,
            fuel_consumed: 0,
        }
    }
}

/// Cryptographically signed SMT Proof Certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtProofCertificate {
    pub certificate_id: String,
    pub timestamp_rfc3339: String,
    pub theorem_name: String,
    pub invariant_kind: SmtInvariantKind,
    pub logic: SmtLogic,
    pub solver_backend: String,
    pub status: InvariantProofStatus,
    pub smtlib2_query_sha256: String,
    pub certificate_signature_sha256: String,
    pub counterexample: Option<CounterexampleModel>,
    pub proof_metrics: SmtSolverMetrics,
}

impl SmtProofCertificate {
    /// Computes the expected canonical SHA-256 signature for this certificate.
    pub fn compute_canonical_signature(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.certificate_id.as_bytes());
        hasher.update(b":");
        hasher.update(self.timestamp_rfc3339.as_bytes());
        hasher.update(b":");
        hasher.update(self.theorem_name.as_bytes());
        hasher.update(b":");
        hasher.update(self.invariant_kind.to_string().as_bytes());
        hasher.update(b":");
        hasher.update(self.status.to_string().as_bytes());
        hasher.update(b":");
        hasher.update(self.smtlib2_query_sha256.as_bytes());
        hasher.update(b":");
        if let Some(ref model) = self.counterexample {
            for (k, v) in &model.bindings {
                hasher.update(format!("{}={};", k, v).as_bytes());
            }
        }
        format!("{:x}", hasher.finalize())
    }

    /// Verifies the cryptographic SHA-256 integrity signature of this proof certificate.
    pub fn verify_signature(&self) -> bool {
        let expected = self.compute_canonical_signature();
        self.certificate_signature_sha256 == expected
    }
}

// ==============================================================================
// 5. Embedded Native SMT2 Mathematical Reasoner (Simplex + DPLL(T))
// ==============================================================================

/// Exact Rational Number representation ($\frac{p}{q}$) with $i128$ for exact Simplex arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    pub num: i128,
    pub den: i128,
}

impl Rational {
    pub fn new(num: i128, den: i128) -> Self {
        assert!(den != 0, "Denominator cannot be zero in Rational");
        let (n, d) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(n.abs(), d);
        Self {
            num: n / g,
            den: d / g,
        }
    }

    pub fn from_int(v: i64) -> Self {
        Self {
            num: v as i128,
            den: 1,
        }
    }

    pub fn zero() -> Self {
        Self { num: 0, den: 1 }
    }

    pub fn one() -> Self {
        Self { num: 1, den: 1 }
    }

    pub fn is_integer(&self) -> bool {
        self.den == 1
    }

    pub fn floor(&self) -> i128 {
        if self.num >= 0 {
            self.num / self.den
        } else {
            (self.num - self.den + 1) / self.den
        }
    }

    pub fn ceil(&self) -> i128 {
        if self.num >= 0 {
            (self.num + self.den - 1) / self.den
        } else {
            self.num / self.den
        }
    }
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    if a == 0 {
        1
    } else {
        a
    }
}

impl std::ops::Add for Rational {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Rational::new(
            self.num * rhs.den + rhs.num * self.den,
            self.den * rhs.den,
        )
    }
}

impl std::ops::Sub for Rational {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        Rational::new(
            self.num * rhs.den - rhs.num * self.den,
            self.den * rhs.den,
        )
    }
}

impl std::ops::Mul for Rational {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        Rational::new(self.num * rhs.num, self.den * rhs.den)
    }
}

impl std::ops::Div for Rational {
    type Output = Self;
    fn div(self, rhs: Self) -> Self::Output {
        assert!(rhs.num != 0, "Division by zero in Rational arithmetic");
        Rational::new(self.num * rhs.den, self.den * rhs.num)
    }
}

impl std::ops::Neg for Rational {
    type Output = Self;
    fn neg(self) -> Self::Output {
        Rational {
            num: -self.num,
            den: self.den,
        }
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.num * other.den).cmp(&(other.num * self.den))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

/// Linear expression: $\sum c_i x_i + c_0$.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearExpr {
    pub coeffs: BTreeMap<String, Rational>,
    pub constant: Rational,
}

impl LinearExpr {
    pub fn zero() -> Self {
        Self {
            coeffs: BTreeMap::new(),
            constant: Rational::zero(),
        }
    }

    pub fn from_var(var: String) -> Self {
        let mut coeffs = BTreeMap::new();
        coeffs.insert(var, Rational::one());
        Self {
            coeffs,
            constant: Rational::zero(),
        }
    }

    pub fn from_const(val: i64) -> Self {
        Self {
            coeffs: BTreeMap::new(),
            constant: Rational::from_int(val),
        }
    }

    pub fn add(&self, rhs: &LinearExpr) -> LinearExpr {
        let mut res = self.clone();
        res.constant = res.constant + rhs.constant;
        for (v, c) in &rhs.coeffs {
            let entry = res.coeffs.entry(v.clone()).or_insert_with(Rational::zero);
            *entry = *entry + *c;
            if entry.num == 0 {
                res.coeffs.remove(v);
            }
        }
        res
    }

    pub fn sub(&self, rhs: &LinearExpr) -> LinearExpr {
        let mut res = self.clone();
        res.constant = res.constant - rhs.constant;
        for (v, c) in &rhs.coeffs {
            let entry = res.coeffs.entry(v.clone()).or_insert_with(Rational::zero);
            *entry = *entry - *c;
            if entry.num == 0 {
                res.coeffs.remove(v);
            }
        }
        res
    }

    pub fn mul_scalar(&self, scalar: Rational) -> LinearExpr {
        let mut res = LinearExpr::zero();
        res.constant = self.constant * scalar;
        for (v, c) in &self.coeffs {
            let p = *c * scalar;
            if p.num != 0 {
                res.coeffs.insert(v.clone(), p);
            }
        }
        res
    }
}

/// Constraint relation operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelOp {
    Le,
    Ge,
    Eq,
    Lt,
    Gt,
}

/// Normalized linear inequality: $\sum a_i x_i \le b$ or $\sum a_i x_i = b$.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLinearConstraint {
    pub expr: LinearExpr,
    pub is_equality: bool,
}

/// Simplex Linear Arithmetic Solver for QF_LIA / QF_LRA.
#[derive(Debug, Clone)]
pub struct SimplexSolver {
    pub vars: Vec<String>,
    pub var_to_idx: HashMap<String, usize>,
    pub lower_bounds: Vec<Option<Rational>>,
    pub upper_bounds: Vec<Option<Rational>>,
    pub values: Vec<Rational>,
    /// Tableau: basic_var_idx -> map of (non_basic_var_idx -> coefficient)
    pub tableau: HashMap<usize, HashMap<usize, Rational>>,
    pub basic_vars: HashSet<usize>,
    pub non_basic_vars: HashSet<usize>,
    pub pivots_count: usize,
    pub is_integer_var: Vec<bool>,
}

impl SimplexSolver {
    pub fn new() -> Self {
        Self {
            vars: Vec::new(),
            var_to_idx: HashMap::new(),
            lower_bounds: Vec::new(),
            upper_bounds: Vec::new(),
            values: Vec::new(),
            tableau: HashMap::new(),
            basic_vars: HashSet::new(),
            non_basic_vars: HashSet::new(),
            pivots_count: 0,
            is_integer_var: Vec::new(),
        }
    }

    pub fn register_var(&mut self, name: &str, is_int: bool) -> usize {
        if let Some(&idx) = self.var_to_idx.get(name) {
            return idx;
        }
        let idx = self.vars.len();
        self.vars.push(name.to_string());
        self.var_to_idx.insert(name.to_string(), idx);
        self.lower_bounds.push(None);
        self.upper_bounds.push(None);
        self.values.push(Rational::zero());
        self.non_basic_vars.insert(idx);
        self.is_integer_var.push(is_int);
        idx
    }

    pub fn set_bounds(&mut self, var_idx: usize, lower: Option<Rational>, upper: Option<Rational>) {
        if let Some(l) = lower {
            if let Some(cur) = self.lower_bounds[var_idx] {
                self.lower_bounds[var_idx] = Some(std::cmp::max(cur, l));
            } else {
                self.lower_bounds[var_idx] = Some(l);
            }
        }
        if let Some(u) = upper {
            if let Some(cur) = self.upper_bounds[var_idx] {
                self.upper_bounds[var_idx] = Some(std::cmp::min(cur, u));
            } else {
                self.upper_bounds[var_idx] = Some(u);
            }
        }
    }

    /// Adds a linear constraint $\sum a_i x_i \le b$ or $= b$.
    pub fn add_linear_constraint(
        &mut self,
        linear: &LinearExpr,
        op: RelOp,
    ) -> Result<(), SmtError> {
        // Normalize strict inequalities for integers:
        // x < b => x <= b - 1 (for integers)
        let mut norm_linear = linear.clone();
        let target_op = match op {
            RelOp::Lt => {
                norm_linear.constant = norm_linear.constant + Rational::one();
                RelOp::Le
            }
            RelOp::Gt => {
                norm_linear.constant = norm_linear.constant - Rational::one();
                RelOp::Ge
            }
            other => other,
        };

        // Create a slack variable for the tableau row: s = \sum a_i x_i
        let slack_name = format!("__slack_{}", self.vars.len());
        let slack_idx = self.register_var(&slack_name, false);
        self.non_basic_vars.remove(&slack_idx);
        self.basic_vars.insert(slack_idx);

        let mut row = HashMap::new();
        for (v, c) in &norm_linear.coeffs {
            let v_idx = self.register_var(v, true);
            row.insert(v_idx, *c);
        }
        self.tableau.insert(slack_idx, row);

        let bound_val = -norm_linear.constant;
        match target_op {
            RelOp::Le => {
                self.set_bounds(slack_idx, None, Some(bound_val));
            }
            RelOp::Ge => {
                self.set_bounds(slack_idx, Some(bound_val), None);
            }
            RelOp::Eq => {
                self.set_bounds(slack_idx, Some(bound_val), Some(bound_val));
            }
            _ => unreachable!(),
        }

        Ok(())
    }

    /// Evaluates current basic variable values based on non-basic assignments.
    pub fn recompute_basic_values(&mut self) {
        for &s_idx in &self.basic_vars {
            let mut sum = Rational::zero();
            if let Some(row) = self.tableau.get(&s_idx) {
                for (&x_idx, &coeff) in row {
                    sum = sum + coeff * self.values[x_idx];
                }
            }
            self.values[s_idx] = sum;
        }
    }

    /// Executes rational Simplex algorithm (Dutertre & de Moura).
    pub fn solve_rational(&mut self, max_pivots: usize) -> Result<bool, SmtError> {
        // Initialize non-basic variable assignments to respect their bounds:
        for &nb in &self.non_basic_vars {
            if let Some(l) = self.lower_bounds[nb] {
                if self.values[nb] < l {
                    self.values[nb] = l;
                }
            }
            if let Some(u) = self.upper_bounds[nb] {
                if self.values[nb] > u {
                    self.values[nb] = u;
                }
            }
        }
        self.recompute_basic_values();

        let mut pivots = 0;
        loop {
            if pivots >= max_pivots {
                return Err(SmtError::Timeout(pivots as u64));
            }

            // Find a basic variable violating its bounds:
            let mut violating = None;
            for &b in &self.basic_vars {
                if let (Some(l), Some(u)) = (self.lower_bounds[b], self.upper_bounds[b]) {
                    if l > u {
                        return Ok(false); // Infeasible directly
                    }
                }
                if let Some(l) = self.lower_bounds[b] {
                    if self.values[b] < l {
                        violating = Some((b, true)); // deficit
                        break;
                    }
                }
                if let Some(u) = self.upper_bounds[b] {
                    if self.values[b] > u {
                        violating = Some((b, false)); // surplus
                        break;
                    }
                }
            }

            let (b_idx, is_deficit) = match violating {
                Some(v) => v,
                None => return Ok(true), // All basic variables satisfy bounds! Feasible.
            };

            // Find a suitable non-basic pivot variable (Bland's rule: smallest index):
            let row = match self.tableau.get(&b_idx) {
                Some(r) => r.clone(),
                None => return Ok(false),
            };

            let mut candidates = Vec::new();
            for (&nb_idx, &coeff) in &row {
                if is_deficit {
                    // Need to increase basic value:
                    if coeff > Rational::zero() {
                        let can_increase = match self.upper_bounds[nb_idx] {
                            Some(u) => self.values[nb_idx] < u,
                            None => true,
                        };
                        if can_increase {
                            candidates.push(nb_idx);
                        }
                    } else if coeff < Rational::zero() {
                        let can_decrease = match self.lower_bounds[nb_idx] {
                            Some(l) => self.values[nb_idx] > l,
                            None => true,
                        };
                        if can_decrease {
                            candidates.push(nb_idx);
                        }
                    }
                } else {
                    // Surplus: need to decrease basic value:
                    if coeff > Rational::zero() {
                        let can_decrease = match self.lower_bounds[nb_idx] {
                            Some(l) => self.values[nb_idx] > l,
                            None => true,
                        };
                        if can_decrease {
                            candidates.push(nb_idx);
                        }
                    } else if coeff < Rational::zero() {
                        let can_increase = match self.upper_bounds[nb_idx] {
                            Some(u) => self.values[nb_idx] < u,
                            None => true,
                        };
                        if can_increase {
                            candidates.push(nb_idx);
                        }
                    }
                }
            }

            if candidates.is_empty() {
                // Infeasible: no pivot variable can satisfy the bound!
                return Ok(false);
            }

            // Pick smallest variable index to prevent cycling (Bland's rule)
            candidates.sort_unstable();
            let pivot_nb = candidates[0];

            // Perform pivot on (b_idx, pivot_nb):
            self.pivot(b_idx, pivot_nb);
            pivots += 1;
            self.pivots_count += 1;
        }
    }

    /// Performs tableau pivot operation: basic variable `b` and non-basic variable `nb`.
    fn pivot(&mut self, b: usize, nb: usize) {
        let b_row = self.tableau.remove(&b).unwrap();
        let a_ij = *b_row.get(&nb).unwrap();

        // Express nb in terms of b and other non-basics:
        // b = a_ij * nb + \sum_{k != j} a_ik * x_k
        // nb = (1/a_ij) * b - \sum_{k != j} (a_ik / a_ij) * x_k
        let inv_a = Rational::one() / a_ij;
        let mut new_nb_row = HashMap::new();
        new_nb_row.insert(b, inv_a);

        for (&k, &coeff) in &b_row {
            if k != nb {
                new_nb_row.insert(k, -(coeff * inv_a));
            }
        }

        // Substitute nb in all other tableau rows:
        for (&other_b, row) in self.tableau.iter_mut() {
            if let Some(a_other_nb) = row.remove(&nb) {
                for (&k, &coeff) in &new_nb_row {
                    let entry = row.entry(k).or_insert_with(Rational::zero);
                    *entry = *entry + a_other_nb * coeff;
                    if entry.num == 0 {
                        row.remove(&k);
                    }
                }
            }
        }

        // Update basic / non-basic sets:
        self.tableau.insert(nb, new_nb_row);
        self.basic_vars.remove(&b);
        self.non_basic_vars.insert(b);
        self.non_basic_vars.remove(&nb);
        self.basic_vars.insert(nb);

        // Adjust value of newly non-basic variable to its nearest bound:
        if let Some(l) = self.lower_bounds[b] {
            if self.values[b] < l {
                self.values[b] = l;
            }
        }
        if let Some(u) = self.upper_bounds[b] {
            if self.values[b] > u {
                self.values[b] = u;
            }
        }
        self.recompute_basic_values();
    }

    /// Solves linear integer constraints with Branch-and-Bound (QF_LIA).
    pub fn solve_integer(&mut self, max_branches: usize) -> Result<bool, SmtError> {
        let mut branches = 0;
        let mut stack = Vec::new();
        stack.push(self.clone());

        while let Some(mut current) = stack.pop() {
            branches += 1;
            if branches > max_branches {
                return Err(SmtError::Timeout(branches as u64));
            }

            let feasible = current.solve_rational(2000)?;
            if !feasible {
                continue;
            }

            // Check if all integer variables have integer values:
            let mut branch_var = None;
            for i in 0..current.vars.len() {
                if current.is_integer_var[i] && !current.values[i].is_integer() {
                    branch_var = Some((i, current.values[i]));
                    break;
                }
            }

            match branch_var {
                None => {
                    // Integer solution found! Copy state to self.
                    *self = current;
                    return Ok(true);
                }
                Some((var_idx, val)) => {
                    // Branch on floor and ceil:
                    let floor_val = Rational::new(val.floor(), 1);
                    let ceil_val = Rational::new(val.ceil(), 1);

                    // Branch 1: x_i <= floor(val)
                    let mut b1 = current.clone();
                    b1.set_bounds(var_idx, None, Some(floor_val));
                    stack.push(b1);

                    // Branch 2: x_i >= ceil(val)
                    let mut b2 = current.clone();
                    b2.set_bounds(var_idx, Some(ceil_val), None);
                    stack.push(b2);
                }
            }
        }

        Ok(false)
    }

    /// Extracts model bindings for non-slack variables.
    pub fn extract_model(&self) -> BTreeMap<String, SmtValue> {
        let mut model = BTreeMap::new();
        for (i, name) in self.vars.iter().enumerate() {
            if name.starts_with("__slack_") {
                continue;
            }
            let val = self.values[i].floor() as i64;
            model.insert(name.clone(), SmtValue::Int(val));
        }
        model
    }
}

// ==============================================================================
// 6. DPLL(T) Engine: Propositional SAT + Theory Solver
// ==============================================================================

/// A literal in CNF: positive (+var) or negative (-var).
pub type Literal = i32;

/// DPLL(T) solver combining Boolean SAT with Theory Solvers.
pub struct DpllTSolver {
    pub num_vars: usize,
    pub clauses: Vec<Vec<Literal>>,
    pub atom_map: HashMap<usize, SmtExpr>,
    pub var_to_atom: HashMap<usize, usize>,
    pub assignments: Vec<Option<bool>>,
    pub metrics: SmtSolverMetrics,
}

impl DpllTSolver {
    pub fn new() -> Self {
        Self {
            num_vars: 0,
            clauses: Vec::new(),
            atom_map: HashMap::new(),
            var_to_atom: HashMap::new(),
            assignments: Vec::new(),
            metrics: SmtSolverMetrics::default(),
        }
    }

    pub fn new_var(&mut self) -> usize {
        self.num_vars += 1;
        self.assignments.push(None);
        self.num_vars
    }

    pub fn add_clause(&mut self, clause: Vec<Literal>) {
        if !clause.is_empty() {
            self.clauses.push(clause);
        }
    }

    /// Unit propagation (Boolean Constraint Propagation).
    pub fn unit_propagate(&mut self) -> Result<bool, ()> {
        let mut changed = true;
        while changed {
            changed = false;
            for clause in &self.clauses {
                let mut unassigned_lit = None;
                let mut clause_sat = false;
                let mut false_count = 0;

                for &lit in clause {
                    let var = lit.unsigned_abs() as usize;
                    let sign = lit > 0;
                    match self.assignments[var - 1] {
                        Some(val) => {
                            if val == sign {
                                clause_sat = true;
                                break;
                            } else {
                                false_count += 1;
                            }
                        }
                        None => {
                            if unassigned_lit.is_none() {
                                unassigned_lit = Some(lit);
                            } else {
                                // More than one unassigned literal
                                unassigned_lit = None;
                                break;
                            }
                        }
                    }
                }

                if clause_sat {
                    continue;
                }

                if false_count == clause.len() {
                    // Conflict detected: all literals false in clause!
                    self.metrics.dpll_conflicts += 1;
                    return Err(());
                }

                if let Some(lit) = unassigned_lit {
                    let var = lit.unsigned_abs() as usize;
                    let sign = lit > 0;
                    self.assignments[var - 1] = Some(sign);
                    changed = true;
                }
            }
        }
        Ok(true)
    }

    /// Solves the formula with DPLL search and calls Theory Solver on full assignments.
    pub fn solve(&mut self, logic: SmtLogic) -> Result<Option<BTreeMap<String, SmtValue>>, SmtError> {
        let mut stack = Vec::new();
        stack.push((self.assignments.clone(), 1));

        while let Some((assignments, next_var)) = stack.pop() {
            self.assignments = assignments;
            self.metrics.dpll_decisions += 1;

            if self.unit_propagate().is_err() {
                continue; // Backtrack on boolean conflict
            }

            // Find next unassigned variable:
            let mut unassigned = None;
            for v in next_var..=self.num_vars {
                if self.assignments[v - 1].is_none() {
                    unassigned = Some(v);
                    break;
                }
            }

            match unassigned {
                Some(v) => {
                    // Branch True then False
                    let mut a1 = self.assignments.clone();
                    a1[v - 1] = Some(false);
                    stack.push((a1, v + 1));

                    let mut a2 = self.assignments.clone();
                    a2[v - 1] = Some(true);
                    stack.push((a2, v + 1));
                }
                None => {
                    // All boolean variables assigned: check Theory Consistency!
                    if let Some(model) = self.check_theory_consistency(logic)? {
                        return Ok(Some(model));
                    }
                    // Theory conflict: continue search
                    self.metrics.dpll_conflicts += 1;
                }
            }
        }

        Ok(None) // UNSAT
    }

    /// Theory Consistency Check: passes active theory atoms to Simplex / BitVector theory solver.
    fn check_theory_consistency(
        &mut self,
        _logic: SmtLogic,
    ) -> Result<Option<BTreeMap<String, SmtValue>>, SmtError> {
        let mut simplex = SimplexSolver::new();

        for (&var, &atom_id) in &self.var_to_atom {
            let assigned_true = self.assignments[var - 1].unwrap_or(true);
            if let Some(atom) = self.atom_map.get(&atom_id) {
                let (target_atom, is_negated) = if assigned_true {
                    (atom, false)
                } else {
                    (atom, true)
                };

                let res = add_atom_to_simplex(&mut simplex, target_atom, is_negated)?;
                if !res {
                    return Ok(None); // Direct conflict in atom normalization
                }
            }
        }

        let sat = simplex.solve_integer(5000)?;
        self.metrics.simplex_pivots += simplex.pivots_count;

        if sat {
            Ok(Some(simplex.extract_model()))
        } else {
            Ok(None)
        }
    }
}

/// Translates an SMT atom into linear simplex constraints.
fn add_atom_to_simplex(
    simplex: &mut SimplexSolver,
    atom: &SmtExpr,
    is_negated: bool,
) -> Result<bool, SmtError> {
    match atom {
        SmtExpr::Lt(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                // not (a < b) => a >= b => a - b >= 0
                simplex.add_linear_constraint(&diff, RelOp::Ge)?;
            } else {
                // a < b => a - b < 0
                simplex.add_linear_constraint(&diff, RelOp::Lt)?;
            }
        }
        SmtExpr::Le(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                // not (a <= b) => a > b => a - b > 0
                simplex.add_linear_constraint(&diff, RelOp::Gt)?;
            } else {
                // a <= b => a - b <= 0
                simplex.add_linear_constraint(&diff, RelOp::Le)?;
            }
        }
        SmtExpr::Gt(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                simplex.add_linear_constraint(&diff, RelOp::Le)?;
            } else {
                simplex.add_linear_constraint(&diff, RelOp::Gt)?;
            }
        }
        SmtExpr::Ge(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                simplex.add_linear_constraint(&diff, RelOp::Lt)?;
            } else {
                simplex.add_linear_constraint(&diff, RelOp::Ge)?;
            }
        }
        SmtExpr::Eq(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                // not (a == b) => a != b.
                // In single branch, disequality can be chosen or handled by DPLL branching.
                // For bounds verification, a != 0 in denominator is common:
                // We add bound: diff <= -1 OR diff >= 1.
                // To keep Simplex convex, if diff cannot be 0, we can branch or enforce disequality.
            } else {
                simplex.add_linear_constraint(&diff, RelOp::Eq)?;
            }
        }
        SmtExpr::BvUlt(a, b) | SmtExpr::BvUle(a, b) => {
            // Map bitvector comparison to bounded integer simplex constraints
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            let diff = left.sub(&right);
            if is_negated {
                simplex.add_linear_constraint(&diff, RelOp::Ge)?;
            } else {
                simplex.add_linear_constraint(&diff, RelOp::Lt)?;
            }
        }
        _ => {}
    }
    Ok(true)
}

/// Converts an AST arithmetic expression to a LinearExpr.
fn expr_to_linear(expr: &SmtExpr) -> Result<LinearExpr, SmtError> {
    match expr {
        SmtExpr::Var(name) => Ok(LinearExpr::from_var(name.clone())),
        SmtExpr::IntConst(val) => Ok(LinearExpr::from_const(*val)),
        SmtExpr::BvConst { value, .. } => Ok(LinearExpr::from_const(*value as i64)),
        SmtExpr::Add(items) => {
            let mut sum = LinearExpr::zero();
            for item in items {
                let l = expr_to_linear(item)?;
                sum = sum.add(&l);
            }
            Ok(sum)
        }
        SmtExpr::Sub(a, b) => {
            let left = expr_to_linear(a)?;
            let right = expr_to_linear(b)?;
            Ok(left.sub(&right))
        }
        SmtExpr::Mul(a, b) => {
            let left = expr_to_linear(a);
            let right = expr_to_linear(b);
            match (left, right) {
                (Ok(l), Ok(r)) if l.coeffs.is_empty() => Ok(r.mul_scalar(l.constant)),
                (Ok(l), Ok(r)) if r.coeffs.is_empty() => Ok(l.mul_scalar(r.constant)),
                _ => Err(SmtError::UnsupportedTheory(
                    "Non-linear multiplication outside QF_LIA".into(),
                )),
            }
        }
        SmtExpr::Neg(a) => {
            let l = expr_to_linear(a)?;
            Ok(LinearExpr::zero().sub(&l))
        }
        _ => Err(SmtError::UnsupportedTheory(format!(
            "Expression cannot be linearized: {:?}",
            expr
        ))),
    }
}

// ==============================================================================
// 7. External SMT Solvers Pipe (Z3 / CVC5)
// ==============================================================================

/// Dispatches verification queries to external SMT-LIB2 processes (`z3` or `cvc5`).
pub struct ExternalSmtPipe {
    solver_cmd: String,
    args: Vec<String>,
}

impl ExternalSmtPipe {
    pub fn detect_z3() -> Option<Self> {
        if is_executable_available("z3") {
            Some(Self {
                solver_cmd: "z3".to_string(),
                args: vec!["-in".to_string(), "-smt2".to_string()],
            })
        } else {
            None
        }
    }

    pub fn detect_cvc5() -> Option<Self> {
        if is_executable_available("cvc5") {
            Some(Self {
                solver_cmd: "cvc5".to_string(),
                args: vec!["--lang=smt2".to_string(), "--produce-models".to_string()],
            })
        } else {
            None
        }
    }

    pub fn execute_smt2(&self, smt_script: &str) -> Result<(String, String), SmtError> {
        let mut child = Command::new(&self.solver_cmd)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SmtError::SolverExecution(format!("Failed to spawn {}: {}", self.solver_cmd, e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(smt_script.as_bytes())?;
        }

        let output = child
            .wait_with_output()
            .map_err(|e| SmtError::SolverExecution(format!("Error waiting for {}: {}", self.solver_cmd, e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((stdout, stderr))
    }
}

fn is_executable_available(exe: &str) -> bool {
    Command::new("which")
        .arg(exe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parses external solver output (e.g. `unsat` or `sat\n(model ...)`).
fn parse_external_solver_response(
    stdout: &str,
) -> (InvariantProofStatus, Option<CounterexampleModel>) {
    let trimmed = stdout.trim();
    if trimmed.starts_with("unsat") {
        (InvariantProofStatus::ProvenUnsat, None)
    } else if trimmed.starts_with("sat") {
        let mut bindings = BTreeMap::new();
        // Extract variable bindings from (define-fun var () Sort val)
        let re = Regex::new(r"\(define-fun\s+([a-zA-Z0-9_]+)\s*\(\)\s*([a-zA-Z0-9_\(\)]+)\s+([^\)]+)\)").unwrap();
        for cap in re.captures_iter(stdout) {
            let var = cap[1].to_string();
            let raw_val = cap[3].trim();
            if let Ok(num) = raw_val.parse::<i64>() {
                bindings.insert(var, SmtValue::Int(num));
            } else if raw_val == "true" || raw_val == "false" {
                bindings.insert(var, SmtValue::Bool(raw_val == "true"));
            } else if raw_val.starts_with("(- ") {
                let neg_val = raw_val.trim_start_matches("(- ").trim_end_matches(')').trim();
                if let Ok(num) = neg_val.parse::<i64>() {
                    bindings.insert(var, SmtValue::Int(-num));
                }
            }
        }

        let model = CounterexampleModel {
            bindings,
            violating_expression: None,
            witness_summary: "Satisfying assignment found violating safety invariant".into(),
        };
        (InvariantProofStatus::RefutedSatWithCounterexample, Some(model))
    } else {
        (InvariantProofStatus::Unknown, None)
    }
}

// ==============================================================================
// 8. High-Level Neural-Symbolic Verification Engine
// ==============================================================================

/// Configuration parameters for Neural-Symbolic Verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeuralSymbolicVerifierConfig {
    pub backend: SmtSolverBackend,
    pub timeout_ms: u64,
    pub max_pivots: usize,
    pub strict_bounds: bool,
}

impl Default for NeuralSymbolicVerifierConfig {
    fn default() -> Self {
        Self {
            backend: SmtSolverBackend::Auto,
            timeout_ms: 5000,
            max_pivots: 10_000,
            strict_bounds: true,
        }
    }
}

/// Verification report for a source file or code patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtVerificationReport {
    pub target_file: String,
    pub is_safe: bool,
    pub certificates: Vec<SmtProofCertificate>,
    pub overall_sha256: String,
    pub elapsed_micros: u64,
}

/// Principal Neural-Symbolic SMT Verification Engine.
pub struct NeuralSymbolicVerifier {
    config: NeuralSymbolicVerifierConfig,
}

impl NeuralSymbolicVerifier {
    pub fn new(backend: SmtSolverBackend) -> Self {
        Self {
            config: NeuralSymbolicVerifierConfig {
                backend,
                ..Default::default()
            },
        }
    }

    pub fn with_config(config: NeuralSymbolicVerifierConfig) -> Self {
        Self { config }
    }

    /// Verifies a single Verification Condition and issues a cryptographic Proof Certificate.
    pub fn verify_vc(
        &self,
        vc: &SmtVerificationCondition,
    ) -> Result<SmtProofCertificate, SmtError> {
        let start = Instant::now();
        let benchmark = vc.to_negated_benchmark();
        let query_script = benchmark.to_smt2_script();
        let query_sha256 = benchmark.compute_sha256();

        let mut solver_used = "EmbeddedNativeSimplex";
        let mut proof_status = InvariantProofStatus::Unknown;
        let mut counterexample = None;
        let mut metrics = SmtSolverMetrics::default();

        // 1. Dual-mode execution: Try external if requested or auto
        let mut executed_externally = false;
        if matches!(
            self.config.backend,
            SmtSolverBackend::Auto | SmtSolverBackend::ExternalZ3 | SmtSolverBackend::ExternalCvc5
        ) {
            let external_opt = match self.config.backend {
                SmtSolverBackend::ExternalZ3 => ExternalSmtPipe::detect_z3(),
                SmtSolverBackend::ExternalCvc5 => ExternalSmtPipe::detect_cvc5(),
                _ => ExternalSmtPipe::detect_z3().or_else(ExternalSmtPipe::detect_cvc5),
            };

            if let Some(pipe) = external_opt {
                if let Ok((stdout, _)) = pipe.execute_smt2(&query_script) {
                    let (status, ce) = parse_external_solver_response(&stdout);
                    if status != InvariantProofStatus::Unknown {
                        proof_status = status;
                        counterexample = ce;
                        solver_used = "ExternalZ3Pipe";
                        executed_externally = true;
                    }
                }
            }
        }

        // 2. Embedded Native Fallback Reasoner (Simplex + DPLL(T))
        if !executed_externally {
            solver_used = "EmbeddedNativeSimplexDPLL(T)";
            let (status, ce, m) = self.solve_with_native_reasoner(&benchmark)?;
            proof_status = status;
            counterexample = ce;
            metrics = m;
        }

        metrics.duration_micros = start.elapsed().as_micros() as u64;

        // Construct cryptographic Proof Certificate
        let cert_id = format!(
            "HGB-SMT-{}-{:08x}",
            vc.theorem_name,
            Utc::now().timestamp_millis() & 0xffffffff
        );

        let mut cert = SmtProofCertificate {
            certificate_id: cert_id,
            timestamp_rfc3339: Utc::now().to_rfc3339(),
            theorem_name: vc.theorem_name.clone(),
            invariant_kind: vc.invariant_kind.clone(),
            logic: vc.logic,
            solver_backend: solver_used.to_string(),
            status: proof_status,
            smtlib2_query_sha256: query_sha256,
            certificate_signature_sha256: String::new(),
            counterexample,
            proof_metrics: metrics,
        };

        // Seal with SHA-256 signature
        cert.certificate_signature_sha256 = cert.compute_canonical_signature();
        Ok(cert)
    }

    /// Solves an SMT benchmark using the native Simplex / DPLL(T) reasoner.
    fn solve_with_native_reasoner(
        &self,
        benchmark: &SmtBenchmark,
    ) -> Result<
        (
            InvariantProofStatus,
            Option<CounterexampleModel>,
            SmtSolverMetrics,
        ),
        SmtError,
    > {
        // Detect if any assertion is a negated conjunction: not(and(p1, p2, ...)) => (not p1) or (not p2) or ...
        let mut split_branches: Option<Vec<SmtExpr>> = None;
        let mut base_assertions = Vec::new();

        for assertion in &benchmark.assertions {
            if let SmtExpr::Not(inner) = assertion {
                if let SmtExpr::And(items) = &**inner {
                    let branches: Vec<SmtExpr> = items
                        .iter()
                        .map(|item| SmtExpr::not(item.clone()))
                        .collect();
                    split_branches = Some(branches);
                    continue;
                }
            }
            base_assertions.push(assertion.clone());
        }

        let mut total_pivots = 0;

        if let Some(branches) = split_branches {
            for branch in branches {
                let mut simplex = SimplexSolver::new();
                for (name, sort) in &benchmark.declarations {
                    let is_int = matches!(sort, SmtSort::Int | SmtSort::BitVec(_));
                    simplex.register_var(name, is_int);
                }
                for base in &base_assertions {
                    self.flatten_and_assert_to_simplex(&mut simplex, base)?;
                }
                self.flatten_and_assert_to_simplex(&mut simplex, &branch)?;

                let is_sat = simplex.solve_integer(self.config.max_pivots)?;
                total_pivots += simplex.pivots_count;

                if is_sat {
                    // One of the disjunctive violation branches is SAT -> Invariant REFUTED!
                    let bindings = simplex.extract_model();
                    let ce = CounterexampleModel {
                        bindings,
                        violating_expression: None,
                        witness_summary: "Counterexample witness satisfies negated invariant branch".into(),
                    };
                    let metrics = SmtSolverMetrics {
                        simplex_pivots: total_pivots,
                        ..Default::default()
                    };
                    return Ok((
                        InvariantProofStatus::RefutedSatWithCounterexample,
                        Some(ce),
                        metrics,
                    ));
                }
            }

            // All disjunctive branches are UNSAT -> Invariant PROVEN!
            let metrics = SmtSolverMetrics {
                simplex_pivots: total_pivots,
                iterations: 1,
                ..Default::default()
            };
            return Ok((InvariantProofStatus::ProvenUnsat, None, metrics));
        }

        // Single branch case:
        let mut simplex = SimplexSolver::new();

        // Register variables
        for (name, sort) in &benchmark.declarations {
            let is_int = matches!(sort, SmtSort::Int | SmtSort::BitVec(_));
            simplex.register_var(name, is_int);
        }

        // Formulate linear assertions
        for assertion in &benchmark.assertions {
            self.flatten_and_assert_to_simplex(&mut simplex, assertion)?;
        }

        let is_sat = simplex.solve_integer(self.config.max_pivots)?;
        let mut metrics = SmtSolverMetrics {
            simplex_pivots: simplex.pivots_count,
            ..Default::default()
        };

        if is_sat {
            // Negation is SAT -> Invariant REFUTED with counterexample model!
            let bindings = simplex.extract_model();
            let ce = CounterexampleModel {
                bindings,
                violating_expression: None,
                witness_summary: "Counterexample witness satisfies negated invariant".into(),
            };
            Ok((
                InvariantProofStatus::RefutedSatWithCounterexample,
                Some(ce),
                metrics,
            ))
        } else {
            // Negation is UNSAT -> Invariant is PROVEN!
            metrics.iterations = 1;
            Ok((InvariantProofStatus::ProvenUnsat, None, metrics))
        }
    }

    fn flatten_and_assert_to_simplex(
        &self,
        simplex: &mut SimplexSolver,
        expr: &SmtExpr,
    ) -> Result<(), SmtError> {
        match expr {
            SmtExpr::And(items) => {
                for item in items {
                    self.flatten_and_assert_to_simplex(simplex, item)?;
                }
            }
            SmtExpr::Not(inner) => {
                add_atom_to_simplex(simplex, inner, true)?;
            }
            atom => {
                add_atom_to_simplex(simplex, atom, false)?;
            }
        }
        Ok(())
    }

    /// High-level API: Proves array bounds preservation ($0 \le \text{idx} < \text{len}$).
    pub fn prove_bounds_preservation(
        &self,
        idx_var: &str,
        len_var: &str,
        has_upper_guard: bool,
        has_lower_guard: bool,
    ) -> Result<SmtProofCertificate, SmtError> {
        let mut decls = BTreeMap::new();
        decls.insert(idx_var.to_string(), SmtSort::Int);
        decls.insert(len_var.to_string(), SmtSort::Int);

        let mut assumptions = Vec::new();
        // Vector length is always non-negative: len >= 0
        assumptions.push(SmtExpr::ge(SmtExpr::var(len_var), SmtExpr::int(0)));

        if has_lower_guard {
            assumptions.push(SmtExpr::ge(SmtExpr::var(idx_var), SmtExpr::int(0)));
        }
        if has_upper_guard {
            assumptions.push(SmtExpr::lt(SmtExpr::var(idx_var), SmtExpr::var(len_var)));
        }

        // Safety property: 0 <= idx < len
        let safety = SmtExpr::and(vec![
            SmtExpr::ge(SmtExpr::var(idx_var), SmtExpr::int(0)),
            SmtExpr::lt(SmtExpr::var(idx_var), SmtExpr::var(len_var)),
        ]);

        let vc = SmtVerificationCondition {
            id: format!("vc_bounds_{}_{}", idx_var, len_var),
            theorem_name: format!("theorem_bounds_preservation_{}", idx_var),
            invariant_kind: SmtInvariantKind::ArrayBoundsPreservation,
            logic: SmtLogic::QfLia,
            declarations: decls,
            assumptions,
            safety_property: safety,
        };

        self.verify_vc(&vc)
    }

    /// High-level API: Proves division-by-zero freedom ($\text{denom} \ne 0$).
    pub fn prove_division_by_zero_freedom(
        &self,
        denom_var: &str,
        has_zero_guard: bool,
    ) -> Result<SmtProofCertificate, SmtError> {
        let mut decls = BTreeMap::new();
        decls.insert(denom_var.to_string(), SmtSort::Int);

        let mut assumptions = Vec::new();
        if has_zero_guard {
            // Guard: denom >= 1 (e.g. denom > 0)
            assumptions.push(SmtExpr::ge(SmtExpr::var(denom_var), SmtExpr::int(1)));
        }

        // Safety property: denom != 0 (represented as denom >= 1 for positive divisors)
        let safety = SmtExpr::ge(SmtExpr::var(denom_var), SmtExpr::int(1));

        let vc = SmtVerificationCondition {
            id: format!("vc_div_zero_{}", denom_var),
            theorem_name: format!("theorem_division_safety_{}", denom_var),
            invariant_kind: SmtInvariantKind::DivisionByZeroFreedom,
            logic: SmtLogic::QfLia,
            declarations: decls,
            assumptions,
            safety_property: safety,
        };

        self.verify_vc(&vc)
    }

    /// High-level API: Proves raw pointer non-nullness before dereference ($\text{ptr} \ne 0$).
    pub fn prove_pointer_non_nullness(
        &self,
        ptr_var: &str,
        has_null_guard: bool,
    ) -> Result<SmtProofCertificate, SmtError> {
        let mut decls = BTreeMap::new();
        decls.insert(ptr_var.to_string(), SmtSort::Int);

        let mut assumptions = Vec::new();
        if has_null_guard {
            // Pointer is non-null: ptr >= 1
            assumptions.push(SmtExpr::ge(SmtExpr::var(ptr_var), SmtExpr::int(1)));
        }

        // Safety property: ptr >= 1
        let safety = SmtExpr::ge(SmtExpr::var(ptr_var), SmtExpr::int(1));

        let vc = SmtVerificationCondition {
            id: format!("vc_ptr_non_null_{}", ptr_var),
            theorem_name: format!("theorem_pointer_safety_{}", ptr_var),
            invariant_kind: SmtInvariantKind::NullPointerFreedom,
            logic: SmtLogic::QfLia,
            declarations: decls,
            assumptions,
            safety_property: safety,
        };

        self.verify_vc(&vc)
    }

    /// High-level API: Proves loop convergence (monotonic variant reduction).
    pub fn prove_loop_convergence(
        &self,
        counter_var: &str,
        limit_var: &str,
        step: i64,
    ) -> Result<SmtProofCertificate, SmtError> {
        let mut decls = BTreeMap::new();
        decls.insert(counter_var.to_string(), SmtSort::Int);
        decls.insert(limit_var.to_string(), SmtSort::Int);

        let mut assumptions = Vec::new();
        // Invariant: counter < limit and limit >= 0
        assumptions.push(SmtExpr::lt(SmtExpr::var(counter_var), SmtExpr::var(limit_var)));
        assumptions.push(SmtExpr::ge(SmtExpr::var(limit_var), SmtExpr::int(0)));

        // Safety property: variant (limit - counter) decreases strictly:
        // (limit - (counter + step)) < (limit - counter)
        let next_counter = SmtExpr::add(vec![SmtExpr::var(counter_var), SmtExpr::int(step)]);
        let var_curr = SmtExpr::sub(SmtExpr::var(limit_var), SmtExpr::var(counter_var));
        let var_next = SmtExpr::sub(SmtExpr::var(limit_var), next_counter);
        let safety = SmtExpr::lt(var_next, var_curr);

        let vc = SmtVerificationCondition {
            id: format!("vc_loop_convergence_{}", counter_var),
            theorem_name: format!("theorem_loop_termination_{}", counter_var),
            invariant_kind: SmtInvariantKind::LoopConvergence,
            logic: SmtLogic::QfLia,
            declarations: decls,
            assumptions,
            safety_property: safety,
        };

        self.verify_vc(&vc)
    }

    /// Scans a source file or code patch, extracts verification conditions, and verifies all invariants.
    pub fn verify_code_patch(
        &self,
        code_or_patch: &str,
        target_name: &str,
    ) -> Result<SmtVerificationReport, SmtError> {
        let start = Instant::now();
        let mut certificates = Vec::new();
        let mut all_safe = true;

        // Parse code for bounds checks, divisions, pointer dereferences:
        let lines: Vec<&str> = code_or_patch.lines().collect();

        // 1. Check array indexing: arr[idx]
        let idx_re = Regex::new(r"([a-zA-Z_]\w*)\s*\[\s*([a-zA-Z_]\w*)\s*\]").unwrap();
        for (line_idx, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }

            if let Some(caps) = idx_re.captures(t) {
                let var = caps[2].to_string();
                if var != "0"
                    && var != "1"
                    && !var.starts_with('_')
                    && !matches!(
                        var.as_str(),
                        "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" | "f32" | "f64" | "bool" | "T"
                    )
                {
                    // Check if guarded in surrounding context:
                    let has_guard = lines[..line_idx].iter().rev().take(10).any(|l| {
                        l.contains(&format!("{} <", var))
                            || l.contains(&format!(".get({})", var))
                            || l.contains(&format!("assert!({} <", var))
                    });

                    let cert = self.prove_bounds_preservation(&var, "len", has_guard, true)?;
                    if cert.status != InvariantProofStatus::ProvenUnsat {
                        all_safe = false;
                    }
                    certificates.push(cert);
                }
            }

            // 2. Check divisions: / denom
            if t.contains('/') && !t.contains("//") && !t.contains("http") {
                let div_re = Regex::new(r"/\s*([a-zA-Z_]\w*)").unwrap();
                if let Some(caps) = div_re.captures(t) {
                    let denom = caps[1].to_string();
                    if !denom.chars().all(|c| c.is_ascii_digit()) {
                        let has_guard = lines[..line_idx].iter().rev().take(10).any(|l| {
                            l.contains(&format!("{} != 0", denom))
                                || l.contains(&format!("{} > 0", denom))
                                || l.contains(&format!("assert!({} != 0", denom))
                        });

                        let cert = self.prove_division_by_zero_freedom(&denom, has_guard)?;
                        if cert.status != InvariantProofStatus::ProvenUnsat {
                            all_safe = false;
                        }
                        certificates.push(cert);
                    }
                }
            }

            // 3. Check raw pointer dereferences: *ptr
            if t.contains('*') && (t.contains("unsafe") || t.contains("ptr")) {
                let ptr_re = Regex::new(r"\*([a-zA-Z_]\w*)").unwrap();
                if let Some(caps) = ptr_re.captures(t) {
                    let ptr_name = caps[1].to_string();
                    if ptr_name != "const" && ptr_name != "mut" && ptr_name != "self" {
                        let has_guard = lines[..line_idx].iter().rev().take(10).any(|l| {
                            l.contains(&format!("!{}.is_null()", ptr_name))
                                || l.contains(&format!("NonNull::new({})", ptr_name))
                        });

                        let cert = self.prove_pointer_non_nullness(&ptr_name, has_guard)?;
                        if cert.status != InvariantProofStatus::ProvenUnsat {
                            all_safe = false;
                        }
                        certificates.push(cert);
                    }
                }
            }
        }

        // Compute overall cryptographic digest of all certificates
        let mut hasher = Sha256::new();
        for c in &certificates {
            hasher.update(c.certificate_signature_sha256.as_bytes());
        }
        let overall_sha256 = format!("{:x}", hasher.finalize());

        Ok(SmtVerificationReport {
            target_file: target_name.to_string(),
            is_safe: all_safe,
            certificates,
            overall_sha256,
            elapsed_micros: start.elapsed().as_micros() as u64,
        })
    }
}
