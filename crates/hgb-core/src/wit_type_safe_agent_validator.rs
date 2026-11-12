//! # Compile-Time & Runtime WIT Type Safety for Autonomous Agent Workflows
//!
//! WebAssembly Interface Types (WIT) specification validator enforcing strict type
//! boundaries on LLM agent tool call parameters and returns.
//!
//! Eliminates blind trust in raw LLM JSON text generation by validating AST against
//! exact WIT world, interface, record, variant, enum, flags, and function definitions
//! before reaching storage, IPC, or microVM execution layers.
//!
//! When an agent hallucinates an invalid field or type, the validator immediately triggers
//! a structured `WitBoundaryTrap` containing the exact path, expected WIT type, actual value,
//! and surgical corrective guidance so the agent can self-repair without host crashes.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 1. Algebraic Data Types & WIT AST Model
// ---------------------------------------------------------------------------

/// WIT Primitive scalar types supported by the WebAssembly Component Model
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WitPrimitive {
    Bool,
    S8,
    U8,
    S16,
    U16,
    S32,
    U32,
    S64,
    U64,
    Float32,
    Float64,
    Char,
    String,
}

impl fmt::Display for WitPrimitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Bool => "bool",
            Self::S8 => "s8",
            Self::U8 => "u8",
            Self::S16 => "s16",
            Self::U16 => "u16",
            Self::S32 => "s32",
            Self::U32 => "u32",
            Self::S64 => "s64",
            Self::U64 => "u64",
            Self::Float32 => "float32",
            Self::Float64 => "float64",
            Self::Char => "char",
            Self::String => "string",
        };
        write!(f, "{}", s)
    }
}

/// A named field within a WIT record
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitField {
    pub name: String,
    pub ty: WitType,
    pub docs: Option<String>,
}

impl WitField {
    pub fn new(name: impl Into<String>, ty: WitType) -> Self {
        Self {
            name: name.into(),
            ty,
            docs: None,
        }
    }

    pub fn with_docs(mut self, docs: impl Into<String>) -> Self {
        self.docs = Some(docs.into());
        self
    }
}

/// A case (variant branch) within a WIT variant
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitCase {
    pub name: String,
    pub ty: Option<WitType>,
    pub docs: Option<String>,
}

impl WitCase {
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: None,
            docs: None,
        }
    }

    pub fn with_type(name: impl Into<String>, ty: WitType) -> Self {
        Self {
            name: name.into(),
            ty: Some(ty),
            docs: None,
        }
    }

    pub fn with_docs(mut self, docs: impl Into<String>) -> Self {
        self.docs = Some(docs.into());
        self
    }
}

/// Comprehensive WebAssembly Interface Type (WIT) algebraic data model
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WitType {
    // Primitives
    Bool,
    S8,
    U8,
    S16,
    U16,
    S32,
    U32,
    S64,
    U64,
    Float32,
    Float64,
    Char,
    String,

    // Compound
    List(Box<WitType>),
    Option(Box<WitType>),
    Result {
        ok: Option<Box<WitType>>,
        err: Option<Box<WitType>>,
    },
    Tuple(Vec<WitType>),

    // Named Structures
    Record(Vec<WitField>),
    Variant(Vec<WitCase>),
    Enum(Vec<String>),
    Flags(Vec<String>),

    // Named Type Alias or Reference
    Named(String),
}

impl WitType {
    pub fn list(inner: WitType) -> Self {
        Self::List(Box::new(inner))
    }

    pub fn option(inner: WitType) -> Self {
        Self::Option(Box::new(inner))
    }

    pub fn result(ok: Option<WitType>, err: Option<WitType>) -> Self {
        Self::Result {
            ok: ok.map(Box::new),
            err: err.map(Box::new),
        }
    }

    pub fn result_ok(ok: WitType) -> Self {
        Self::result(Some(ok), None)
    }

    pub fn result_ok_err(ok: WitType, err: WitType) -> Self {
        Self::result(Some(ok), Some(err))
    }

    pub fn tuple(items: Vec<WitType>) -> Self {
        Self::Tuple(items)
    }

    pub fn record(fields: Vec<WitField>) -> Self {
        Self::Record(fields)
    }

    pub fn variant(cases: Vec<WitCase>) -> Self {
        Self::Variant(cases)
    }

    pub fn wit_enum(variants: Vec<impl Into<String>>) -> Self {
        Self::Enum(variants.into_iter().map(Into::into).collect())
    }

    pub fn flags(flags: Vec<impl Into<String>>) -> Self {
        Self::Flags(flags.into_iter().map(Into::into).collect())
    }

    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    /// Formats the canonical WIT specification string for this type
    pub fn type_name(&self) -> String {
        match self {
            Self::Bool => "bool".into(),
            Self::S8 => "s8".into(),
            Self::U8 => "u8".into(),
            Self::S16 => "s16".into(),
            Self::U16 => "u16".into(),
            Self::S32 => "s32".into(),
            Self::U32 => "u32".into(),
            Self::S64 => "s64".into(),
            Self::U64 => "u64".into(),
            Self::Float32 => "float32".into(),
            Self::Float64 => "float64".into(),
            Self::Char => "char".into(),
            Self::String => "string".into(),
            Self::List(inner) => format!("list<{}>", inner.type_name()),
            Self::Option(inner) => format!("option<{}>", inner.type_name()),
            Self::Result { ok, err } => match (ok, err) {
                (Some(o), Some(e)) => format!("result<{}, {}>", o.type_name(), e.type_name()),
                (Some(o), None) => format!("result<{}>", o.type_name()),
                (None, Some(e)) => format!("result<_, {}>", e.type_name()),
                (None, None) => "result".into(),
            },
            Self::Tuple(items) => {
                let parts: Vec<String> = items.iter().map(|t| t.type_name()).collect();
                format!("tuple<{}>", parts.join(", "))
            }
            Self::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, f.ty.type_name()))
                    .collect();
                format!("record {{ {} }}", parts.join(", "))
            }
            Self::Variant(cases) => {
                let parts: Vec<String> = cases
                    .iter()
                    .map(|c| match &c.ty {
                        Some(t) => format!("{}({})", c.name, t.type_name()),
                        None => c.name.clone(),
                    })
                    .collect();
                format!("variant {{ {} }}", parts.join(", "))
            }
            Self::Enum(cases) => format!("enum {{ {} }}", cases.join(", ")),
            Self::Flags(flags) => format!("flags {{ {} }}", flags.join(", ")),
            Self::Named(name) => name.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Interfaces, Functions, and Worlds
// ---------------------------------------------------------------------------

/// Parameter definition for a WIT function
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitParam {
    pub name: String,
    pub ty: WitType,
}

impl WitParam {
    pub fn new(name: impl Into<String>, ty: WitType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// Return type specification for a WIT function
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WitFunctionResult {
    Anon(WitType),
    Named(Vec<WitParam>),
    None,
}

/// Function definition in a WIT interface
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitFunction {
    pub name: String,
    pub params: Vec<WitParam>,
    pub results: WitFunctionResult,
    pub docs: Option<String>,
}

impl WitFunction {
    pub fn new(
        name: impl Into<String>,
        params: Vec<WitParam>,
        results: WitFunctionResult,
    ) -> Self {
        Self {
            name: name.into(),
            params,
            results,
            docs: None,
        }
    }

    pub fn with_docs(mut self, docs: impl Into<String>) -> Self {
        self.docs = Some(docs.into());
        self
    }
}

/// Interface definition containing types and exported functions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WitInterface {
    pub name: String,
    pub types: HashMap<String, WitType>,
    pub functions: HashMap<String, WitFunction>,
    pub docs: Option<String>,
}

impl WitInterface {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            types: HashMap::new(),
            functions: HashMap::new(),
            docs: None,
        }
    }

    pub fn add_type(&mut self, name: impl Into<String>, ty: WitType) {
        self.types.insert(name.into(), ty);
    }

    pub fn add_function(&mut self, func: WitFunction) {
        self.functions.insert(func.name.clone(), func);
    }
}

/// World definition describing imports and exports for a WebAssembly Component
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WitWorld {
    pub name: String,
    pub imports: HashMap<String, WitInterface>,
    pub exports: HashMap<String, WitInterface>,
    pub types: HashMap<String, WitType>,
}

impl WitWorld {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            imports: HashMap::new(),
            exports: HashMap::new(),
            types: HashMap::new(),
        }
    }
}

/// Type registry holding declared WIT types for reference resolution
#[derive(Debug, Clone, Default)]
pub struct WitTypeRegistry {
    types: HashMap<String, WitType>,
}

impl WitTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>, ty: WitType) {
        self.types.insert(name.into(), ty);
    }

    pub fn get(&self, name: &str) -> Option<&WitType> {
        self.types.get(name)
    }

    /// Resolves named references recursively up to a depth limit of 32
    pub fn resolve<'a>(&'a self, ty: &'a WitType) -> Result<&'a WitType, String> {
        let mut curr = ty;
        let mut depth = 0;
        while let WitType::Named(name) = curr {
            depth += 1;
            if depth > 32 {
                return Err(format!("Cyclic or excessively deep type reference at '{}'", name));
            }
            match self.types.get(name) {
                Some(resolved) => curr = resolved,
                None => return Err(format!("Unresolved WIT type reference '{}'", name)),
            }
        }
        Ok(curr)
    }
}

// ---------------------------------------------------------------------------
// 3. Instant WASM Boundary Trapping Model
// ---------------------------------------------------------------------------

/// Structured reason why a boundary trap was triggered
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrapReason {
    TypeMismatch,
    IntegerOutOfRange,
    FloatOutOfRange,
    MissingRequiredField,
    UnknownField,
    InvalidVariantCase,
    InvalidEnumCase,
    InvalidFlag,
    InvalidTupleLength,
    InvalidResultStructure,
    UnresolvedTypeReference,
    ParameterCountMismatch,
    MissingParameter,
    UnknownParameter,
    RecursionLimitExceeded,
    Custom(String),
}

/// Instant WASM Boundary Trap emitted when an LLM agent hallucinates or breaches WIT contracts
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitBoundaryTrap {
    /// Exact JSON / AST path where violation occurred (e.g., `payload.ttl_secs` or `args[0]`)
    pub path: String,
    /// Canonical WIT type specification expected at this boundary
    pub expected_wit_type: String,
    /// Actual JSON value received from the LLM agent
    pub actual_value: serde_json::Value,
    /// Categorized trap reason
    pub reason: TrapReason,
    /// Surgical corrective guidance for the agent loop to self-repair its prompt output
    pub corrective_guidance: String,
}

impl WitBoundaryTrap {
    pub fn new(
        path: impl Into<String>,
        expected: impl Into<String>,
        actual: serde_json::Value,
        reason: TrapReason,
        guidance: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            expected_wit_type: expected.into(),
            actual_value: actual,
            reason,
            corrective_guidance: guidance.into(),
        }
    }

    /// Formats an actionable prompt injection snippet to feed directly back to the LLM agent
    pub fn to_agent_prompt(&self) -> String {
        format!(
            "CRITICAL: WebAssembly Interface Type Boundary Trap Triggered!\n\
             - Parameter Path: {}\n\
             - Expected WIT Type: {}\n\
             - Received Value: {}\n\
             - Violation Reason: {:?}\n\
             - Corrective Action: {}\n\
             Please regenerate the tool call arguments adhering strictly to the WIT specification.",
            self.path, self.expected_wit_type, self.actual_value, self.reason, self.corrective_guidance
        )
    }
}

impl fmt::Display for WitBoundaryTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WASM Boundary Trap at '{}' [{:?}]: expected '{}', received '{}'. Guidance: {}",
            self.path, self.reason, self.expected_wit_type, self.actual_value, self.corrective_guidance
        )
    }
}

impl std::error::Error for WitBoundaryTrap {}

// ---------------------------------------------------------------------------
// 4. Validator Options & Configuration
// ---------------------------------------------------------------------------

/// Configuration options for WIT boundary validation
#[derive(Debug, Clone)]
pub struct WitValidatorOptions {
    /// Disallows extraneous unknown fields not defined in the WIT record
    pub reject_unknown_fields: bool,
    /// Lenient matching between kebab-case, snake_case, and camelCase
    pub case_lenient: bool,
    /// Maximum AST recursion depth to guard against malicious recursive payloads
    pub max_depth: usize,
}

impl Default for WitValidatorOptions {
    fn default() -> Self {
        Self {
            reject_unknown_fields: true,
            case_lenient: true,
            max_depth: 64,
        }
    }
}

impl WitValidatorOptions {
    pub fn strict() -> Self {
        Self {
            reject_unknown_fields: true,
            case_lenient: false,
            max_depth: 64,
        }
    }

    pub fn lenient() -> Self {
        Self {
            reject_unknown_fields: false,
            case_lenient: true,
            max_depth: 64,
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Recursive-Descent WIT Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitParserError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for WitParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WIT Parse Error at line {}, col {}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for WitParserError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    LBrace,     // {
    RBrace,     // }
    LParen,     // (
    RParen,     // )
    LAngle,     // <
    RAngle,     // >
    Colon,      // :
    Comma,      // ,
    Semicolon,  // ;
    Equal,      // =
    Arrow,      // ->
    At,         // @
    Period,     // .
    Slash,      // /
    Eof,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    source: &'a str,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            chars: source.char_indices().peekable(),
            source,
            line: 1,
            col: 1,
        }
    }

    fn next_token(&mut self) -> Result<(Token, usize, usize), WitParserError> {
        loop {
            self.skip_whitespace();
            let (idx, ch) = match self.chars.peek() {
                Some(&(i, c)) => (i, c),
                None => return Ok((Token::Eof, self.line, self.col)),
            };

            // Single line comment or doc comment
            if ch == '/' {
                let mut lookahead = self.chars.clone();
                lookahead.next();
                if let Some((_, '/')) = lookahead.peek() {
                    // Skip till newline
                    self.chars.next();
                    self.chars.next();
                    self.col += 2;
                    while let Some(&(_, c)) = self.chars.peek() {
                        self.chars.next();
                        if c == '\n' {
                            self.line += 1;
                            self.col = 1;
                            break;
                        } else {
                            self.col += 1;
                        }
                    }
                    continue;
                } else if let Some((_, '*')) = lookahead.peek() {
                    // Block comment
                    self.chars.next();
                    self.chars.next();
                    self.col += 2;
                    while let Some(&(_, c)) = self.chars.peek() {
                        self.chars.next();
                        if c == '\n' {
                            self.line += 1;
                            self.col = 1;
                        } else {
                            self.col += 1;
                        }
                        if c == '*' {
                            if let Some(&(_, '/')) = self.chars.peek() {
                                self.chars.next();
                                self.col += 1;
                                break;
                            }
                        }
                    }
                    continue;
                }
            }

            let start_line = self.line;
            let start_col = self.col;

            // Punctuation
            match ch {
                '{' => { self.chars.next(); self.col += 1; return Ok((Token::LBrace, start_line, start_col)); }
                '}' => { self.chars.next(); self.col += 1; return Ok((Token::RBrace, start_line, start_col)); }
                '(' => { self.chars.next(); self.col += 1; return Ok((Token::LParen, start_line, start_col)); }
                ')' => { self.chars.next(); self.col += 1; return Ok((Token::RParen, start_line, start_col)); }
                '<' => { self.chars.next(); self.col += 1; return Ok((Token::LAngle, start_line, start_col)); }
                '>' => { self.chars.next(); self.col += 1; return Ok((Token::RAngle, start_line, start_col)); }
                ':' => { self.chars.next(); self.col += 1; return Ok((Token::Colon, start_line, start_col)); }
                ',' => { self.chars.next(); self.col += 1; return Ok((Token::Comma, start_line, start_col)); }
                ';' => { self.chars.next(); self.col += 1; return Ok((Token::Semicolon, start_line, start_col)); }
                '=' => { self.chars.next(); self.col += 1; return Ok((Token::Equal, start_line, start_col)); }
                '@' => { self.chars.next(); self.col += 1; return Ok((Token::At, start_line, start_col)); }
                '.' => { self.chars.next(); self.col += 1; return Ok((Token::Period, start_line, start_col)); }
                '/' => { self.chars.next(); self.col += 1; return Ok((Token::Slash, start_line, start_col)); }
                '-' => {
                    let mut lookahead = self.chars.clone();
                    lookahead.next();
                    if let Some((_, '>')) = lookahead.peek() {
                        self.chars.next();
                        self.chars.next();
                        self.col += 2;
                        return Ok((Token::Arrow, start_line, start_col));
                    }
                }
                _ => {}
            }

            // Identifiers, keywords, and version/numbers
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '%' || ch == '-' {
                let start_idx = idx;
                while let Some(&(_, c)) = self.chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '%' {
                        self.chars.next();
                        self.col += 1;
                    } else {
                        break;
                    }
                }
                let end_idx = self.chars.peek().map(|&(i, _)| i).unwrap_or(self.source.len());
                let text = &self.source[start_idx..end_idx];
                return Ok((Token::Ident(text.to_string()), start_line, start_col));
            }

            // Unexpected char
            self.chars.next();
            self.col += 1;
            return Err(WitParserError {
                message: format!("Unexpected character: '{}'", ch),
                line: start_line,
                column: start_col,
            });
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(&(_, c)) = self.chars.peek() {
            if c == ' ' || c == '\t' || c == '\r' {
                self.chars.next();
                self.col += 1;
            } else if c == '\n' {
                self.chars.next();
                self.line += 1;
                self.col = 1;
            } else {
                break;
            }
        }
    }
}

/// Standalone parser for standard WIT syntax declarations
pub struct WitParser<'a> {
    lexer: Lexer<'a>,
    current: Token,
    curr_line: usize,
    curr_col: usize,
}

impl<'a> WitParser<'a> {
    pub fn new(source: &'a str) -> Result<Self, WitParserError> {
        let mut lexer = Lexer::new(source);
        let (current, curr_line, curr_col) = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            curr_line,
            curr_col,
        })
    }

    fn advance(&mut self) -> Result<Token, WitParserError> {
        let (token, line, col) = self.lexer.next_token()?;
        let prev = std::mem::replace(&mut self.current, token);
        self.curr_line = line;
        self.curr_col = col;
        Ok(prev)
    }

    fn peek(&self) -> &Token {
        &self.current
    }

    fn expect_ident(&mut self) -> Result<String, WitParserError> {
        match self.advance()? {
            Token::Ident(s) => Ok(s),
            other => Err(WitParserError {
                message: format!("Expected identifier, found {:?}", other),
                line: self.curr_line,
                column: self.curr_col,
            }),
        }
    }

    fn expect(&mut self, expected: Token) -> Result<(), WitParserError> {
        let curr = self.advance()?;
        if curr == expected {
            Ok(())
        } else {
            Err(WitParserError {
                message: format!("Expected token {:?}, found {:?}", expected, curr),
                line: self.curr_line,
                column: self.curr_col,
            })
        }
    }

    /// Parses a complete WIT document containing package headers, interfaces, worlds, and types
    pub fn parse_file(&mut self) -> Result<WitWorld, WitParserError> {
        let mut world = WitWorld::new("default");

        while self.peek() != &Token::Eof {
            match self.peek() {
                Token::Ident(id) if id == "package" => {
                    self.advance()?;
                    // skip package namespace:pkg@version;
                    while self.peek() != &Token::Semicolon && self.peek() != &Token::Eof {
                        self.advance()?;
                    }
                    if self.peek() == &Token::Semicolon {
                        self.advance()?;
                    }
                }
                Token::Ident(id) if id == "interface" => {
                    let iface = self.parse_interface()?;
                    world.exports.insert(iface.name.clone(), iface);
                }
                Token::Ident(id) if id == "world" => {
                    self.advance()?;
                    let name = self.expect_ident()?;
                    world.name = name;
                    self.expect(Token::LBrace)?;
                    while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                        if let Token::Ident(decl) = self.peek() {
                            if decl == "import" || decl == "export" {
                                self.advance()?;
                                let item_name = self.expect_ident()?;
                                if self.peek() == &Token::Colon {
                                    self.advance()?;
                                    let _ty = self.parse_type()?;
                                }
                                if self.peek() == &Token::Semicolon {
                                    self.advance()?;
                                }
                                continue;
                            }
                        }
                        self.advance()?;
                    }
                    self.expect(Token::RBrace)?;
                }
                Token::Ident(id) if id == "record" || id == "variant" || id == "enum" || id == "flags" || id == "type" => {
                    let (name, ty) = self.parse_type_def()?;
                    world.types.insert(name, ty);
                }
                _ => {
                    self.advance()?;
                }
            }
        }

        Ok(world)
    }

    /// Parses an interface declaration: `interface <name> { ... }`
    pub fn parse_interface(&mut self) -> Result<WitInterface, WitParserError> {
        self.expect(Token::Ident("interface".into()))?;
        let name = self.expect_ident()?;
        let mut iface = WitInterface::new(name);
        self.expect(Token::LBrace)?;

        while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
            if let Token::Ident(kw) = self.peek().clone() {
                if kw == "record" || kw == "variant" || kw == "enum" || kw == "flags" || kw == "type" {
                    let (t_name, ty) = self.parse_type_def()?;
                    iface.add_type(t_name, ty);
                    continue;
                }
            }

            // Otherwise, attempt parsing a function: `func-name: func(...) -> ...;`
            let func = self.parse_function()?;
            iface.add_function(func);
            if self.peek() == &Token::Semicolon {
                self.advance()?;
            }
        }

        self.expect(Token::RBrace)?;
        Ok(iface)
    }

    /// Parses a WIT type definition: record, variant, enum, flags, or type alias
    pub fn parse_type_def(&mut self) -> Result<(String, WitType), WitParserError> {
        let kw = self.expect_ident()?;
        let name = self.expect_ident()?;

        match kw.as_str() {
            "record" => {
                self.expect(Token::LBrace)?;
                let mut fields = Vec::new();
                while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                    let field_name = self.expect_ident()?;
                    self.expect(Token::Colon)?;
                    let field_ty = self.parse_type()?;
                    fields.push(WitField::new(field_name, field_ty));
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RBrace)?;
                Ok((name, WitType::Record(fields)))
            }
            "variant" => {
                self.expect(Token::LBrace)?;
                let mut cases = Vec::new();
                while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                    let case_name = self.expect_ident()?;
                    let case_ty = if self.peek() == &Token::LParen {
                        self.advance()?;
                        let t = self.parse_type()?;
                        self.expect(Token::RParen)?;
                        Some(t)
                    } else {
                        None
                    };
                    cases.push(WitCase {
                        name: case_name,
                        ty: case_ty,
                        docs: None,
                    });
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RBrace)?;
                Ok((name, WitType::Variant(cases)))
            }
            "enum" => {
                self.expect(Token::LBrace)?;
                let mut variants = Vec::new();
                while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                    let var_name = self.expect_ident()?;
                    variants.push(var_name);
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RBrace)?;
                Ok((name, WitType::Enum(variants)))
            }
            "flags" => {
                self.expect(Token::LBrace)?;
                let mut flags = Vec::new();
                while self.peek() != &Token::RBrace && self.peek() != &Token::Eof {
                    let flag_name = self.expect_ident()?;
                    flags.push(flag_name);
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RBrace)?;
                Ok((name, WitType::Flags(flags)))
            }
            "type" => {
                self.expect(Token::Equal)?;
                let target_ty = self.parse_type()?;
                if self.peek() == &Token::Semicolon {
                    self.advance()?;
                }
                Ok((name, target_ty))
            }
            other => Err(WitParserError {
                message: format!("Unknown type keyword '{}'", other),
                line: self.curr_line,
                column: self.curr_col,
            }),
        }
    }

    /// Parses a function signature: `<name>: func(<params>) -> <results>;`
    pub fn parse_function(&mut self) -> Result<WitFunction, WitParserError> {
        let name = self.expect_ident()?;
        self.expect(Token::Colon)?;
        self.expect(Token::Ident("func".into()))?;
        self.expect(Token::LParen)?;

        let mut params = Vec::new();
        while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
            let p_name = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let p_ty = self.parse_type()?;
            params.push(WitParam::new(p_name, p_ty));
            if self.peek() == &Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RParen)?;

        let results = if self.peek() == &Token::Arrow {
            self.advance()?;
            if self.peek() == &Token::LParen {
                self.advance()?;
                let mut ret_params = Vec::new();
                while self.peek() != &Token::RParen && self.peek() != &Token::Eof {
                    let r_name = self.expect_ident()?;
                    self.expect(Token::Colon)?;
                    let r_ty = self.parse_type()?;
                    ret_params.push(WitParam::new(r_name, r_ty));
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RParen)?;
                WitFunctionResult::Named(ret_params)
            } else {
                let ty = self.parse_type()?;
                WitFunctionResult::Anon(ty)
            }
        } else {
            WitFunctionResult::None
        };

        Ok(WitFunction::new(name, params, results))
    }

    /// Parses any WIT type expression (primitives, list, option, result, tuple, named)
    pub fn parse_type(&mut self) -> Result<WitType, WitParserError> {
        let ident = self.expect_ident()?;
        match ident.as_str() {
            "bool" => Ok(WitType::Bool),
            "s8" => Ok(WitType::S8),
            "u8" => Ok(WitType::U8),
            "s16" => Ok(WitType::S16),
            "u16" => Ok(WitType::U16),
            "s32" => Ok(WitType::S32),
            "u32" => Ok(WitType::U32),
            "s64" => Ok(WitType::S64),
            "u64" => Ok(WitType::U64),
            "float32" => Ok(WitType::Float32),
            "float64" => Ok(WitType::Float64),
            "char" => Ok(WitType::Char),
            "string" => Ok(WitType::String),
            "list" => {
                self.expect(Token::LAngle)?;
                let inner = self.parse_type()?;
                self.expect(Token::RAngle)?;
                Ok(WitType::list(inner))
            }
            "option" => {
                self.expect(Token::LAngle)?;
                let inner = self.parse_type()?;
                self.expect(Token::RAngle)?;
                Ok(WitType::option(inner))
            }
            "result" => {
                if self.peek() == &Token::LAngle {
                    self.advance()?;
                    let ok = if self.peek() == &Token::Ident("_".into()) {
                        self.advance()?;
                        None
                    } else {
                        Some(self.parse_type()?)
                    };

                    let err = if self.peek() == &Token::Comma {
                        self.advance()?;
                        Some(self.parse_type()?)
                    } else {
                        None
                    };

                    self.expect(Token::RAngle)?;
                    Ok(WitType::result(ok, err))
                } else {
                    Ok(WitType::result(None, None))
                }
            }
            "tuple" => {
                self.expect(Token::LAngle)?;
                let mut items = Vec::new();
                while self.peek() != &Token::RAngle && self.peek() != &Token::Eof {
                    items.push(self.parse_type()?);
                    if self.peek() == &Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RAngle)?;
                Ok(WitType::tuple(items))
            }
            named => Ok(WitType::named(named)),
        }
    }
}

// ---------------------------------------------------------------------------
// 6. WIT Type-Safe Agent Validator Engine
// ---------------------------------------------------------------------------

/// Production-grade WIT validation engine enforcing component model type boundaries
#[derive(Debug, Clone, Default)]
pub struct WitValidator {
    pub options: WitValidatorOptions,
    pub registry: WitTypeRegistry,
    pub interfaces: HashMap<String, WitInterface>,
}

impl WitValidator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(mut self, options: WitValidatorOptions) -> Self {
        self.options = options;
        self
    }

    /// Parses and loads a WIT specification source string into the validator environment
    pub fn load_wit_str(&mut self, wit_source: &str) -> Result<(), WitParserError> {
        let mut parser = WitParser::new(wit_source)?;
        let world = parser.parse_file()?;

        // Register world-level types
        for (name, ty) in world.types {
            self.registry.register(name, ty);
        }

        // Register exported and imported interfaces
        for (name, iface) in world.exports {
            for (t_name, ty) in &iface.types {
                self.registry.register(t_name.clone(), ty.clone());
            }
            self.interfaces.insert(name, iface);
        }
        for (name, iface) in world.imports {
            for (t_name, ty) in &iface.types {
                self.registry.register(t_name.clone(), ty.clone());
            }
            self.interfaces.insert(name, iface);
        }

        Ok(())
    }

    /// Adds a pre-built WIT interface directly to the validator
    pub fn add_interface(&mut self, iface: WitInterface) {
        for (t_name, ty) in &iface.types {
            self.registry.register(t_name.clone(), ty.clone());
        }
        self.interfaces.insert(iface.name.clone(), iface);
    }

    /// Adds a named type directly to the registry
    pub fn add_type(&mut self, name: impl Into<String>, ty: WitType) {
        self.registry.register(name, ty);
    }

    /// Validates an arbitrary JSON value against a known registered WIT type
    pub fn validate_type(&self, type_name: &str, val: &serde_json::Value) -> Result<(), WitBoundaryTrap> {
        let ty = self.registry.get(type_name).ok_or_else(|| {
            WitBoundaryTrap::new(
                type_name,
                type_name,
                val.clone(),
                TrapReason::UnresolvedTypeReference,
                format!("WIT type '{}' is not registered in the validator environment.", type_name),
            )
        })?;
        self.validate_value(ty, val, type_name)
    }

    /// Validates an arbitrary JSON value against an explicit `WitType` AST definition
    pub fn validate_value(&self, ty: &WitType, val: &serde_json::Value, path: &str) -> Result<(), WitBoundaryTrap> {
        self.validate_value_inner(ty, val, path, 0)
    }

    /// Validates tool call parameters (JSON object or array) against a function in a WIT interface
    pub fn validate_tool_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), WitBoundaryTrap> {
        let func = self.find_function(tool_name).ok_or_else(|| {
            WitBoundaryTrap::new(
                tool_name,
                "function",
                args.clone(),
                TrapReason::Custom(format!("Function '{}' not found in loaded WIT interfaces", tool_name)),
                format!("The requested tool '{}' does not exist in any registered WIT interface.", tool_name),
            )
        })?;

        self.validate_function_args(func, args)
    }

    /// Validates tool call return value against a function's result signature
    pub fn validate_tool_return(
        &self,
        tool_name: &str,
        output: &serde_json::Value,
    ) -> Result<(), WitBoundaryTrap> {
        let func = self.find_function(tool_name).ok_or_else(|| {
            WitBoundaryTrap::new(
                tool_name,
                "function",
                output.clone(),
                TrapReason::Custom(format!("Function '{}' not found in loaded WIT interfaces", tool_name)),
                format!("The requested tool '{}' does not exist in any registered WIT interface.", tool_name),
            )
        })?;

        self.validate_function_return(func, output)
    }

    /// Finds a function across all loaded interfaces
    pub fn find_function(&self, name: &str) -> Option<&WitFunction> {
        for iface in self.interfaces.values() {
            if let Some(func) = iface.functions.get(name) {
                return Some(func);
            }
            if self.options.case_lenient {
                if let Some((_, func)) = iface
                    .functions
                    .iter()
                    .find(|(k, _)| self.identifier_matches(k, name))
                {
                    return Some(func);
                }
            }
        }
        None
    }

    /// Validates agent arguments against function parameters
    pub fn validate_function_args(
        &self,
        func: &WitFunction,
        args: &serde_json::Value,
    ) -> Result<(), WitBoundaryTrap> {
        match args {
            serde_json::Value::Object(map) => {
                for param in &func.params {
                    let field = self.find_field_in_object(map, &param.name);
                    match field {
                        Some((_, val)) => {
                            let param_path = format!("{}.{}", func.name, param.name);
                            self.validate_value_inner(&param.ty, val, &param_path, 0)?;
                        }
                        None => {
                            if matches!(param.ty, WitType::Option(_)) {
                                continue;
                            }
                            let param_path = format!("{}.{}", func.name, param.name);
                            return Err(WitBoundaryTrap::new(
                                param_path,
                                param.ty.type_name(),
                                serde_json::Value::Null,
                                TrapReason::MissingParameter,
                                format!(
                                    "Function '{}' is missing required parameter '{}' of type '{}'. Supply this argument in the parameters object.",
                                    func.name, param.name, param.ty.type_name()
                                ),
                            ));
                        }
                    }
                }

                if self.options.reject_unknown_fields {
                    for key in map.keys() {
                        if !func.params.iter().any(|p| self.identifier_matches(key, &p.name)) {
                            let key_path = format!("{}.{}", func.name, key);
                            let allowed: Vec<&str> = func.params.iter().map(|p| p.name.as_str()).collect();
                            return Err(WitBoundaryTrap::new(
                                key_path,
                                "none (unrecognized parameter)",
                                map.get(key).cloned().unwrap_or(serde_json::Value::Null),
                                TrapReason::UnknownParameter,
                                format!(
                                    "Function '{}' received extraneous argument '{}'. Allowed parameters are: {:?}.",
                                    func.name, key, allowed
                                ),
                            ));
                        }
                    }
                }
                Ok(())
            }
            serde_json::Value::Array(arr) => {
                if arr.len() > func.params.len() {
                    return Err(WitBoundaryTrap::new(
                        func.name.clone(),
                        format!("{} parameters", func.params.len()),
                        args.clone(),
                        TrapReason::ParameterCountMismatch,
                        format!(
                            "Function '{}' expects at most {} positional arguments, but received {}.",
                            func.name, func.params.len(), arr.len()
                        ),
                    ));
                }

                for (idx, param) in func.params.iter().enumerate() {
                    if idx < arr.len() {
                        let param_path = format!("{}.{}[{}]", func.name, param.name, idx);
                        self.validate_value_inner(&param.ty, &arr[idx], &param_path, 0)?;
                    } else if !matches!(param.ty, WitType::Option(_)) {
                        let param_path = format!("{}.{}", func.name, param.name);
                        return Err(WitBoundaryTrap::new(
                            param_path,
                            param.ty.type_name(),
                            serde_json::Value::Null,
                            TrapReason::MissingParameter,
                            format!(
                                "Function '{}' is missing required positional parameter '{}' at index {}.",
                                func.name, param.name, idx
                            ),
                        ));
                    }
                }
                Ok(())
            }
            serde_json::Value::Null if func.params.is_empty() => Ok(()),
            other => {
                if func.params.is_empty() {
                    if other.is_null() || other == &serde_json::json!({}) || other == &serde_json::json!([]) {
                        return Ok(());
                    }
                }
                Err(WitBoundaryTrap::new(
                    func.name.clone(),
                    "object or array of arguments",
                    other.clone(),
                    TrapReason::TypeMismatch,
                    format!("Tool call arguments for '{}' must be a JSON object or array.", func.name),
                ))
            }
        }
    }

    /// Validates function output against result signature
    pub fn validate_function_return(
        &self,
        func: &WitFunction,
        output: &serde_json::Value,
    ) -> Result<(), WitBoundaryTrap> {
        let ret_path = format!("{}.return", func.name);
        match &func.results {
            WitFunctionResult::None => {
                if output.is_null() || output == &serde_json::json!({}) {
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        ret_path,
                        "void / null",
                        output.clone(),
                        TrapReason::TypeMismatch,
                        format!("Function '{}' returns no value, expected null.", func.name),
                    ))
                }
            }
            WitFunctionResult::Anon(ty) => self.validate_value_inner(ty, output, &ret_path, 0),
            WitFunctionResult::Named(params) => {
                let rec = WitType::Record(
                    params
                        .iter()
                        .map(|p| WitField::new(p.name.clone(), p.ty.clone()))
                        .collect(),
                );
                self.validate_value_inner(&rec, output, &ret_path, 0)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Core Recursive Type Boundary Checking Engine
    // -----------------------------------------------------------------------

    fn validate_value_inner(
        &self,
        ty: &WitType,
        val: &serde_json::Value,
        path: &str,
        depth: usize,
    ) -> Result<(), WitBoundaryTrap> {
        if depth > self.options.max_depth {
            return Err(WitBoundaryTrap::new(
                path,
                ty.type_name(),
                val.clone(),
                TrapReason::RecursionLimitExceeded,
                format!("Recursion limit of {} exceeded at '{}'. Check for cyclic structures.", self.options.max_depth, path),
            ));
        }

        match ty {
            // --- Primitives ---
            WitType::Bool => {
                if val.is_boolean() {
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "bool",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected boolean (true/false), but received {}. Provide a boolean literal.", path, val),
                    ))
                }
            }
            WitType::S8 => {
                if let Some(n) = val.as_i64() {
                    if (-128..=127).contains(&n) {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "s8",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected signed 8-bit integer in range [-128, 127], but received {}.", path, n),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "s8",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected signed 8-bit integer 's8', but received {}. Pass an unquoted integer literal.", path, val),
                    ))
                }
            }
            WitType::U8 => {
                if let Some(n) = val.as_i64() {
                    if n < 0 {
                        return Err(WitBoundaryTrap::new(
                            path,
                            "u8",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected unsigned 8-bit integer in range [0, 255], but received negative value {}.", path, n),
                        ));
                    }
                    if n <= 255 {
                        return Ok(());
                    }
                } else if let Some(n) = val.as_u64() {
                    if n <= 255 {
                        return Ok(());
                    }
                }
                Err(WitBoundaryTrap::new(
                    path,
                    "u8",
                    val.clone(),
                    if val.is_number() { TrapReason::IntegerOutOfRange } else { TrapReason::TypeMismatch },
                    format!("Field '{}' expected unsigned 8-bit integer 'u8' (0..255), but received {}.", path, val),
                ))
            }
            WitType::S16 => {
                if let Some(n) = val.as_i64() {
                    if (-32768..=32767).contains(&n) {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "s16",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected signed 16-bit integer [-32768, 32767], received {}.", path, n),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "s16",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected signed 16-bit integer 's16', received {}.", path, val),
                    ))
                }
            }
            WitType::U16 => {
                if let Some(n) = val.as_i64() {
                    if n < 0 {
                        return Err(WitBoundaryTrap::new(
                            path,
                            "u16",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected unsigned 16-bit integer [0, 65535], received negative {}.", path, n),
                        ));
                    }
                    if n <= 65535 {
                        return Ok(());
                    }
                } else if let Some(n) = val.as_u64() {
                    if n <= 65535 {
                        return Ok(());
                    }
                }
                Err(WitBoundaryTrap::new(
                    path,
                    "u16",
                    val.clone(),
                    if val.is_number() { TrapReason::IntegerOutOfRange } else { TrapReason::TypeMismatch },
                    format!("Field '{}' expected unsigned 16-bit integer 'u16' (0..65535), received {}.", path, val),
                ))
            }
            WitType::S32 => {
                if let Some(n) = val.as_i64() {
                    if (-2147483648..=2147483647).contains(&n) {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "s32",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected signed 32-bit integer [-2147483648, 2147483647], received {}.", path, n),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "s32",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected signed 32-bit integer 's32', received {}.", path, val),
                    ))
                }
            }
            WitType::U32 => {
                if let Some(n) = val.as_i64() {
                    if n < 0 {
                        return Err(WitBoundaryTrap::new(
                            path,
                            "u32",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected unsigned 32-bit integer [0, 4294967295], received negative {}.", path, n),
                        ));
                    }
                    if n <= 4294967295 {
                        return Ok(());
                    }
                } else if let Some(n) = val.as_u64() {
                    if n <= 4294967295 {
                        return Ok(());
                    }
                }
                Err(WitBoundaryTrap::new(
                    path,
                    "u32",
                    val.clone(),
                    if val.is_number() { TrapReason::IntegerOutOfRange } else { TrapReason::TypeMismatch },
                    format!("Field '{}' expected unsigned 32-bit integer 'u32' (0..4294967295), received {}.", path, val),
                ))
            }
            WitType::S64 => {
                if val.as_i64().is_some() {
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "s64",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected signed 64-bit integer 's64', received {}.", path, val),
                    ))
                }
            }
            WitType::U64 => {
                if let Some(n) = val.as_i64() {
                    if n < 0 {
                        return Err(WitBoundaryTrap::new(
                            path,
                            "u64",
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!("Field '{}' expected unsigned 64-bit integer, received negative {}.", path, n),
                        ));
                    }
                    return Ok(());
                }
                if val.as_u64().is_some() {
                    return Ok(());
                }
                Err(WitBoundaryTrap::new(
                    path,
                    "u64",
                    val.clone(),
                    TrapReason::TypeMismatch,
                    format!("Field '{}' expected unsigned 64-bit integer 'u64', received {}.", path, val),
                ))
            }
            WitType::Float32 => {
                if let Some(f) = val.as_f64() {
                    if f.is_finite() && f >= (f32::MIN as f64) && f <= (f32::MAX as f64) {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "float32",
                            val.clone(),
                            TrapReason::FloatOutOfRange,
                            format!("Field '{}' expected finite 32-bit float, received out of range value {}.", path, f),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "float32",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected 32-bit float, received {}.", path, val),
                    ))
                }
            }
            WitType::Float64 => {
                if let Some(f) = val.as_f64() {
                    if f.is_finite() {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "float64",
                            val.clone(),
                            TrapReason::FloatOutOfRange,
                            format!("Field '{}' expected finite 64-bit float, received non-finite value {}.", path, f),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "float64",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected 64-bit float, received {}.", path, val),
                    ))
                }
            }
            WitType::Char => {
                if let Some(s) = val.as_str() {
                    if s.chars().count() == 1 {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            "char",
                            val.clone(),
                            TrapReason::TypeMismatch,
                            format!("Field '{}' expected single unicode char, but received string of length {}.", path, s.chars().count()),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "char",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected single unicode char, received {}.", path, val),
                    ))
                }
            }
            WitType::String => {
                if val.is_string() {
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        "string",
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected string, received {}. Provide a valid double-quoted string.", path, val),
                    ))
                }
            }

            // --- Compound Types ---
            WitType::List(inner) => {
                if let Some(arr) = val.as_array() {
                    for (idx, elem) in arr.iter().enumerate() {
                        let item_path = format!("{}[{}]", path, idx);
                        self.validate_value_inner(inner, elem, &item_path, depth + 1)?;
                    }
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected list (JSON array), received {}.", path, val),
                    ))
                }
            }
            WitType::Option(inner) => {
                if val.is_null() {
                    return Ok(());
                }
                // Handle optional wrappers like {"some": val} or {"none": null}
                if let Some(obj) = val.as_object() {
                    if obj.len() == 1 && (obj.contains_key("none") || obj.contains_key("None")) {
                        return Ok(());
                    }
                    if obj.len() == 1 && (obj.contains_key("some") || obj.contains_key("Some")) {
                        let inner_val = obj.get("some").or_else(|| obj.get("Some")).unwrap();
                        let sub_path = format!("{}.some", path);
                        return self.validate_value_inner(inner, inner_val, &sub_path, depth + 1);
                    }
                }
                // Direct value
                self.validate_value_inner(inner, val, path, depth + 1)
            }
            WitType::Result { ok, err } => {
                if let Some(obj) = val.as_object() {
                    let has_ok = obj.keys().any(|k| k.eq_ignore_ascii_case("ok"));
                    let has_err = obj.keys().any(|k| k.eq_ignore_ascii_case("err"));

                    if has_ok {
                        let ok_val = obj
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("ok"))
                            .map(|(_, v)| v)
                            .unwrap();
                        let sub_path = format!("{}.ok", path);
                        if let Some(ok_ty) = ok {
                            self.validate_value_inner(ok_ty, ok_val, &sub_path, depth + 1)
                        } else {
                            Ok(())
                        }
                    } else if has_err {
                        let err_val = obj
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("err"))
                            .map(|(_, v)| v)
                            .unwrap();
                        let sub_path = format!("{}.err", path);
                        if let Some(err_ty) = err {
                            self.validate_value_inner(err_ty, err_val, &sub_path, depth + 1)
                        } else {
                            Ok(())
                        }
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::InvalidResultStructure,
                            format!(
                                "Field '{}' expected Result formatted as {{\"ok\": ...}} or {{\"err\": ...}}, received keys: {:?}.",
                                path, obj.keys().collect::<Vec<_>>()
                            ),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Field '{}' expected Result object, received {}.", path, val),
                    ))
                }
            }
            WitType::Tuple(items) => {
                if let Some(arr) = val.as_array() {
                    if arr.len() != items.len() {
                        return Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::InvalidTupleLength,
                            format!(
                                "Tuple at '{}' expects {} elements, but received array with {} elements.",
                                path, items.len(), arr.len()
                            ),
                        ));
                    }
                    for (idx, (elem, elem_ty)) in arr.iter().zip(items.iter()).enumerate() {
                        let sub_path = format!("{}[{}]", path, idx);
                        self.validate_value_inner(elem_ty, elem, &sub_path, depth + 1)?;
                    }
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Tuple at '{}' expected JSON array, received {}.", path, val),
                    ))
                }
            }

            // --- Named Structures ---
            WitType::Record(fields) => {
                if let Some(obj) = val.as_object() {
                    for field in fields {
                        let found = self.find_field_in_object(obj, &field.name);
                        match found {
                            Some((_, field_val)) => {
                                let field_path = if path.is_empty() {
                                    field.name.clone()
                                } else {
                                    format!("{}.{}", path, field.name)
                                };
                                self.validate_value_inner(&field.ty, field_val, &field_path, depth + 1)?;
                            }
                            None => {
                                if matches!(field.ty, WitType::Option(_)) {
                                    continue;
                                }
                                let field_path = if path.is_empty() {
                                    field.name.clone()
                                } else {
                                    format!("{}.{}", path, field.name)
                                };
                                return Err(WitBoundaryTrap::new(
                                    field_path,
                                    field.ty.type_name(),
                                    serde_json::Value::Null,
                                    TrapReason::MissingRequiredField,
                                    format!(
                                        "Record at '{}' is missing required field '{}' of type '{}'. Include this field in the object.",
                                        path, field.name, field.ty.type_name()
                                    ),
                                ));
                            }
                        }
                    }

                    if self.options.reject_unknown_fields {
                        for key in obj.keys() {
                            if !fields.iter().any(|f| self.identifier_matches(key, &f.name)) {
                                let key_path = if path.is_empty() {
                                    key.clone()
                                } else {
                                    format!("{}.{}", path, key)
                                };
                                let allowed: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                                return Err(WitBoundaryTrap::new(
                                    key_path,
                                    "none (unrecognized field)",
                                    obj.get(key).cloned().unwrap_or(serde_json::Value::Null),
                                    TrapReason::UnknownField,
                                    format!(
                                        "Record at '{}' contains unexpected field '{}'. Allowed fields: {:?}. Remove this key.",
                                        path, key, allowed
                                    ),
                                ));
                            }
                        }
                    }
                    Ok(())
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Record at '{}' expected JSON object, received {}.", path, val),
                    ))
                }
            }
            WitType::Variant(cases) => {
                // Form 1: Unit variant case passed as string literal, e.g. "any"
                if let Some(s) = val.as_str() {
                    if let Some(case) = cases.iter().find(|c| self.identifier_matches(s, &c.name)) {
                        if case.ty.is_none() {
                            return Ok(());
                        } else {
                            return Err(WitBoundaryTrap::new(
                                path,
                                format!("{}({})", case.name, case.ty.as_ref().unwrap().type_name()),
                                val.clone(),
                                TrapReason::TypeMismatch,
                                format!(
                                    "Variant case '{}' requires payload of type '{}', but received unit string.",
                                    case.name, case.ty.as_ref().unwrap().type_name()
                                ),
                            ));
                        }
                    }
                    let allowed: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
                    return Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::InvalidVariantCase,
                        format!("Variant at '{}' received unknown case '{}'. Allowed cases: {:?}.", path, s, allowed),
                    ));
                }

                // Form 2: JSON object representation: `{"exact": "pattern"}` or `{"case": "exact", "value": "pattern"}`
                if let Some(obj) = val.as_object() {
                    let (case_name, case_val) = if let (Some(c), Some(v)) = (
                        obj.get("case").and_then(|v| v.as_str()),
                        obj.get("value"),
                    ) {
                        (c.to_string(), v)
                    } else if obj.len() == 1 {
                        let (k, v) = obj.iter().next().unwrap();
                        (k.clone(), v)
                    } else {
                        return Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::InvalidVariantCase,
                            format!(
                                "Variant at '{}' must be an object with a single case key or {{\"case\": ..., \"value\": ...}}, received keys: {:?}",
                                path, obj.keys().collect::<Vec<_>>()
                            ),
                        ));
                    };

                    if let Some(case) = cases.iter().find(|c| self.identifier_matches(&case_name, &c.name)) {
                        let case_path = format!("{}.{}", path, case.name);
                        if let Some(case_ty) = &case.ty {
                            return self.validate_value_inner(case_ty, case_val, &case_path, depth + 1);
                        } else if case_val.is_null() {
                            return Ok(());
                        } else {
                            return Err(WitBoundaryTrap::new(
                                case_path,
                                "none (unit case)",
                                case_val.clone(),
                                TrapReason::TypeMismatch,
                                format!("Variant case '{}' takes no payload, but received {}.", case.name, case_val),
                            ));
                        }
                    } else {
                        let allowed: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
                        return Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::InvalidVariantCase,
                            format!("Variant at '{}' received unknown case '{}'. Allowed cases: {:?}.", path, case_name, allowed),
                        ));
                    }
                }

                Err(WitBoundaryTrap::new(
                    path,
                    ty.type_name(),
                    val.clone(),
                    TrapReason::TypeMismatch,
                    format!("Variant at '{}' expected string or single-key object, received {}.", path, val),
                ))
            }
            WitType::Enum(variants) => {
                if let Some(s) = val.as_str() {
                    if variants.iter().any(|v| self.identifier_matches(s, v)) {
                        Ok(())
                    } else {
                        Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::InvalidEnumCase,
                            format!("Enum at '{}' received invalid case '{}'. Allowed values: {:?}.", path, s, variants),
                        ))
                    }
                } else {
                    Err(WitBoundaryTrap::new(
                        path,
                        ty.type_name(),
                        val.clone(),
                        TrapReason::TypeMismatch,
                        format!("Enum at '{}' expected string matching one of {:?}, received {}.", path, variants, val),
                    ))
                }
            }
            WitType::Flags(flags) => {
                // Array of flag strings: `["read", "write"]`
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            if !flags.iter().any(|f| self.identifier_matches(s, f)) {
                                return Err(WitBoundaryTrap::new(
                                    path,
                                    ty.type_name(),
                                    item.clone(),
                                    TrapReason::InvalidFlag,
                                    format!("Flags at '{}' received unknown flag '{}'. Valid flags: {:?}.", path, s, flags),
                                ));
                            }
                        } else {
                            return Err(WitBoundaryTrap::new(
                                path,
                                "string flag",
                                item.clone(),
                                TrapReason::TypeMismatch,
                                format!("Flag element at '{}' must be a string, received {}.", path, item),
                            ));
                        }
                    }
                    return Ok(());
                }

                // Object of flag booleans: `{"read": true, "write": true, "execute": false}`
                if let Some(obj) = val.as_object() {
                    for (k, v) in obj {
                        if !flags.iter().any(|f| self.identifier_matches(k, f)) {
                            return Err(WitBoundaryTrap::new(
                                path,
                                ty.type_name(),
                                serde_json::Value::String(k.clone()),
                                TrapReason::InvalidFlag,
                                format!("Flags object at '{}' contains unrecognized flag key '{}'. Valid flags: {:?}.", path, k, flags),
                            ));
                        }
                        if !v.is_boolean() {
                            let flag_path = format!("{}.{}", path, k);
                            return Err(WitBoundaryTrap::new(
                                flag_path,
                                "bool",
                                v.clone(),
                                TrapReason::TypeMismatch,
                                format!("Flag value for '{}' at '{}' must be a boolean true/false.", k, path),
                            ));
                        }
                    }
                    return Ok(());
                }

                // Bitmask integer: e.g. 0x03
                if let Some(n) = val.as_u64() {
                    let max_mask = if flags.len() >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << flags.len()) - 1
                    };
                    if n <= max_mask {
                        return Ok(());
                    } else {
                        return Err(WitBoundaryTrap::new(
                            path,
                            ty.type_name(),
                            val.clone(),
                            TrapReason::IntegerOutOfRange,
                            format!(
                                "Flags bitmask at '{}' has value {} exceeding maximum capacity {} for {} flags.",
                                path, n, max_mask, flags.len()
                            ),
                        ));
                    }
                }

                Err(WitBoundaryTrap::new(
                    path,
                    ty.type_name(),
                    val.clone(),
                    TrapReason::TypeMismatch,
                    format!("Flags at '{}' expected string array, bool map, or integer bitmask, received {}.", path, val),
                ))
            }
            WitType::Named(name) => {
                let resolved = self.registry.resolve(ty).map_err(|e| {
                    WitBoundaryTrap::new(
                        path,
                        name.clone(),
                        val.clone(),
                        TrapReason::UnresolvedTypeReference,
                        e,
                    )
                })?;
                self.validate_value_inner(resolved, val, path, depth + 1)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Identifier Matching & Case Normalization
    // -----------------------------------------------------------------------

    /// Matches identifiers handling exact match, kebab-case, snake_case, and camelCase
    pub fn identifier_matches(&self, candidate: &str, target: &str) -> bool {
        if candidate == target {
            return true;
        }
        if !self.options.case_lenient {
            return false;
        }

        // Direct kebab/snake conversion
        if candidate.replace('-', "_").eq_ignore_ascii_case(&target.replace('-', "_")) {
            return true;
        }

        // Flattening both for camelCase comparison
        let c_flat: String = candidate
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect();
        let t_flat: String = target
            .chars()
            .filter(|c| *c != '-' && *c != '_')
            .flat_map(|c| c.to_lowercase())
            .collect();

        c_flat == t_flat
    }

    fn find_field_in_object<'a>(
        &self,
        obj: &'a serde_json::Map<String, serde_json::Value>,
        target: &str,
    ) -> Option<(&'a String, &'a serde_json::Value)> {
        if let Some((k, v)) = obj.get_key_value(target) {
            return Some((k, v));
        }
        if !self.options.case_lenient {
            return None;
        }
        obj.iter().find(|(k, _)| self.identifier_matches(k, target))
    }
}

// ---------------------------------------------------------------------------
// 7. Zero-Mock Unit Test Suite
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_wit_primitive_validation_success_and_traps() {
        let validator = WitValidator::new();

        // 1. Bool
        assert!(validator.validate_value(&WitType::Bool, &json!(true), "test.bool").is_ok());
        let trap = validator.validate_value(&WitType::Bool, &json!("not-a-bool"), "test.bool").unwrap_err();
        assert_eq!(trap.reason, TrapReason::TypeMismatch);
        assert_eq!(trap.path, "test.bool");

        // 2. U8 & S8
        assert!(validator.validate_value(&WitType::U8, &json!(255), "test.u8").is_ok());
        let trap_u8_neg = validator.validate_value(&WitType::U8, &json!(-5), "test.u8").unwrap_err();
        assert_eq!(trap_u8_neg.reason, TrapReason::IntegerOutOfRange);

        let trap_u8_overflow = validator.validate_value(&WitType::U8, &json!(300), "test.u8").unwrap_err();
        assert_eq!(trap_u8_overflow.reason, TrapReason::IntegerOutOfRange);

        assert!(validator.validate_value(&WitType::S8, &json!(-128), "test.s8").is_ok());
        let trap_s8 = validator.validate_value(&WitType::S8, &json!(128), "test.s8").unwrap_err();
        assert_eq!(trap_s8.reason, TrapReason::IntegerOutOfRange);

        // 3. U32 & S32
        assert!(validator.validate_value(&WitType::U32, &json!(4294967295u64), "test.u32").is_ok());
        let trap_u32_neg = validator.validate_value(&WitType::U32, &json!(-1), "test.u32").unwrap_err();
        assert_eq!(trap_u32_neg.reason, TrapReason::IntegerOutOfRange);

        // 4. Float32 & Float64
        assert!(validator.validate_value(&WitType::Float32, &json!(3.14159), "test.f32").is_ok());
        assert!(validator.validate_value(&WitType::Float64, &json!(2.718281828459045), "test.f64").is_ok());

        // 5. Char & String
        assert!(validator.validate_value(&WitType::Char, &json!("z"), "test.char").is_ok());
        let trap_char = validator.validate_value(&WitType::Char, &json!("multiple"), "test.char").unwrap_err();
        assert_eq!(trap_char.reason, TrapReason::TypeMismatch);

        assert!(validator.validate_value(&WitType::String, &json!("CaldoAI Hagibis"), "test.str").is_ok());
        let trap_str = validator.validate_value(&WitType::String, &json!(12345), "test.str").unwrap_err();
        assert_eq!(trap_str.reason, TrapReason::TypeMismatch);
    }

    #[test]
    fn test_wit_record_validation() {
        let validator = WitValidator::new();

        let storage_record = WitType::record(vec![
            WitField::new("key", WitType::String),
            WitField::new("payload", WitType::list(WitType::U8)),
            WitField::new("ttl-secs", WitType::option(WitType::U32)),
        ]);

        // Valid record with exact keys
        let valid_json = json!({
            "key": "agent-state-01",
            "payload": [1, 2, 3, 255],
            "ttl-secs": 3600
        });
        assert!(validator.validate_value(&storage_record, &valid_json, "record").is_ok());

        // Valid record with snake_case and omitted optional field
        let valid_snake_omitted = json!({
            "key": "agent-state-02",
            "payload": [10, 20]
        });
        assert!(validator.validate_value(&storage_record, &valid_snake_omitted, "record").is_ok());

        // Trap on missing required field `key`
        let missing_key_json = json!({
            "payload": [1, 2],
            "ttl-secs": 60
        });
        let trap_missing = validator.validate_value(&storage_record, &missing_key_json, "record").unwrap_err();
        assert_eq!(trap_missing.reason, TrapReason::MissingRequiredField);
        assert_eq!(trap_missing.path, "record.key");

        // Trap on negative ttl-secs
        let neg_ttl_json = json!({
            "key": "k",
            "payload": [1],
            "ttl_secs": -100
        });
        let trap_ttl = validator.validate_value(&storage_record, &neg_ttl_json, "record").unwrap_err();
        assert_eq!(trap_ttl.reason, TrapReason::IntegerOutOfRange);
        assert_eq!(trap_ttl.path, "record.ttl-secs");

        // Trap on extraneous unknown field under strict mode
        let extra_field_json = json!({
            "key": "k",
            "payload": [1],
            "hallucinated_param": 999
        });
        let trap_unknown = validator.validate_value(&storage_record, &extra_field_json, "record").unwrap_err();
        assert_eq!(trap_unknown.reason, TrapReason::UnknownField);
    }

    #[test]
    fn test_wit_variant_and_enum_validation() {
        let validator = WitValidator::new();

        // 1. Enum
        let governor_profile = WitType::wit_enum(vec!["performance", "balanced", "powersave"]);
        assert!(validator.validate_value(&governor_profile, &json!("performance"), "profile").is_ok());
        assert!(validator.validate_value(&governor_profile, &json!("balanced"), "profile").is_ok());

        let trap_enum = validator.validate_value(&governor_profile, &json!("extreme-boost"), "profile").unwrap_err();
        assert_eq!(trap_enum.reason, TrapReason::InvalidEnumCase);
        assert!(trap_enum.corrective_guidance.contains("performance"));

        // 2. Variant
        let filter_mode = WitType::variant(vec![
            WitCase::with_type("exact", WitType::String),
            WitCase::with_type("regex", WitType::String),
            WitCase::empty("any"),
        ]);

        // Unit case as string
        assert!(validator.validate_value(&filter_mode, &json!("any"), "filter").is_ok());

        // Case with payload
        assert!(validator.validate_value(&filter_mode, &json!({"exact": "caldo-*"}), "filter").is_ok());
        assert!(validator.validate_value(&filter_mode, &json!({"case": "regex", "value": "^caldo.*$"}), "filter").is_ok());

        // Trap on unknown variant case
        let trap_var = validator.validate_value(&filter_mode, &json!({"fuzzy": "caldo"}), "filter").unwrap_err();
        assert_eq!(trap_var.reason, TrapReason::InvalidVariantCase);
    }

    #[test]
    fn test_wit_list_and_tuple_validation() {
        let validator = WitValidator::new();

        // List of U16
        let list_u16 = WitType::list(WitType::U16);
        assert!(validator.validate_value(&list_u16, &json!([10, 20, 300, 65535]), "list").is_ok());

        // Negative in list
        let trap_list = validator.validate_value(&list_u16, &json!([10, -5, 30]), "list").unwrap_err();
        assert_eq!(trap_list.reason, TrapReason::IntegerOutOfRange);
        assert_eq!(trap_list.path, "list[1]");

        // Tuple
        let tuple_ty = WitType::tuple(vec![WitType::String, WitType::U32, WitType::Bool]);
        assert!(validator.validate_value(&tuple_ty, &json!(["agent-cluster", 42, true]), "tup").is_ok());

        // Mismatched tuple length
        let trap_tup_len = validator.validate_value(&tuple_ty, &json!(["agent-cluster", 42]), "tup").unwrap_err();
        assert_eq!(trap_tup_len.reason, TrapReason::InvalidTupleLength);
    }

    #[test]
    fn test_wit_result_validation() {
        let validator = WitValidator::new();

        let res_ty = WitType::result_ok_err(WitType::U32, WitType::String);

        // Success Ok
        assert!(validator.validate_value(&res_ty, &json!({"ok": 1024}), "res").is_ok());

        // Success Err
        assert!(validator.validate_value(&res_ty, &json!({"err": "disk full"}), "res").is_ok());

        // Type mismatch inside Ok
        let trap_res_ok = validator.validate_value(&res_ty, &json!({"ok": -50}), "res").unwrap_err();
        assert_eq!(trap_res_ok.reason, TrapReason::IntegerOutOfRange);
        assert_eq!(trap_res_ok.path, "res.ok");

        // Invalid Result wrapper
        let trap_res_wrapper = validator.validate_value(&res_ty, &json!({"status": "fine"}), "res").unwrap_err();
        assert_eq!(trap_res_wrapper.reason, TrapReason::InvalidResultStructure);
    }

    #[test]
    fn test_wit_flags_validation() {
        let validator = WitValidator::new();
        let perm_flags = WitType::flags(vec!["read", "write", "execute"]);

        // Array format
        assert!(validator.validate_value(&perm_flags, &json!(["read", "write"]), "perms").is_ok());

        // Object format
        assert!(validator.validate_value(&perm_flags, &json!({"read": true, "write": true, "execute": false}), "perms").is_ok());

        // Bitmask format
        assert!(validator.validate_value(&perm_flags, &json!(3), "perms").is_ok());

        // Invalid flag in array
        let trap_flag = validator.validate_value(&perm_flags, &json!(["read", "admin"]), "perms").unwrap_err();
        assert_eq!(trap_flag.reason, TrapReason::InvalidFlag);
    }

    #[test]
    fn test_wit_parser_full_declarations() {
        let wit_source = r#"
            package hagibis:test@0.1.0;

            interface storage {
                enum status {
                    active,
                    suspended,
                    terminated,
                }

                flags permissions {
                    read,
                    write,
                }

                record item {
                    id: string,
                    count: u32,
                    active: bool,
                }

                get-item: func(id: string) -> result<item, string>;
                set-item: func(item: item, perms: permissions) -> result<bool, string>;
            }
        "#;

        let mut validator = WitValidator::new();
        validator.load_wit_str(wit_source).expect("WIT parser must succeed");

        assert!(validator.interfaces.contains_key("storage"));
        let iface = validator.interfaces.get("storage").unwrap();
        assert!(iface.functions.contains_key("get-item"));
        assert!(iface.functions.contains_key("set-item"));

        // Validate tool call to `get-item`
        let valid_call = json!({ "id": "item-99" });
        assert!(validator.validate_tool_call("get-item", &valid_call).is_ok());

        // Trap on missing parameter
        let bad_call = json!({});
        let trap = validator.validate_tool_call("get-item", &bad_call).unwrap_err();
        assert_eq!(trap.reason, TrapReason::MissingParameter);
        assert_eq!(trap.path, "get-item.id");
    }

    #[test]
    fn test_wit_parser_real_world_dynatune_and_tool_trapping() {
        let dynatune_wit = r#"
            package hagibis:dynatune@0.2.0;

            interface governor {
                enum governor-profile {
                    performance,
                    balanced,
                    powersave,
                }

                record telemetry-report {
                    cpu-temp-millicelsius: u32,
                    active-governor: string,
                    throttled: bool,
                }

                set-profile: func(profile: governor-profile) -> result<bool, string>;
                tune-mglru: func(min-ttl-ms: u32) -> result<bool, string>;
            }
        "#;

        let mut validator = WitValidator::new();
        validator.load_wit_str(dynatune_wit).expect("dynatune WIT parse must succeed");

        // 1. Valid call to `set-profile`
        let valid_profile = json!({ "profile": "balanced" });
        assert!(validator.validate_tool_call("set-profile", &valid_profile).is_ok());

        // 2. Hallucinated enum value triggers Instant WASM Boundary Trap
        let invalid_profile = json!({ "profile": "overclock-turbo" });
        let trap = validator.validate_tool_call("set-profile", &invalid_profile).unwrap_err();
        assert_eq!(trap.reason, TrapReason::InvalidEnumCase);
        assert!(trap.corrective_guidance.contains("performance"));
        assert!(trap.corrective_guidance.contains("balanced"));

        // 3. Negative integer passed to unsigned `u32` min-ttl-ms
        let bad_ttl = json!({ "min-ttl-ms": -50 });
        let trap_ttl = validator.validate_tool_call("tune-mglru", &bad_ttl).unwrap_err();
        assert_eq!(trap_ttl.reason, TrapReason::IntegerOutOfRange);
        assert_eq!(trap_ttl.path, "tune-mglru.min-ttl-ms");

        // 4. Test surgical LLM agent prompt injection generation
        let prompt_injection = trap_ttl.to_agent_prompt();
        assert!(prompt_injection.contains("CRITICAL: WebAssembly Interface Type Boundary Trap Triggered!"));
        assert!(prompt_injection.contains("tune-mglru.min-ttl-ms"));
        assert!(prompt_injection.contains("u32"));
    }
}
