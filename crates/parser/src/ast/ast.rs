//! Declares the types used to represent syntax trees for Go packages.
//!
//! This module is ported from Go's standard `go/ast/ast.go` package,
//! adapted to Rust conventions:
//! - Go's `Node`/`Expr`/`Stmt`/`Decl`/`Spec` interfaces are mapped to enums
//!   (`[Expr]`, `[Stmt]`, `[Decl]`, `[Spec]`) whose variants carry the
//!   corresponding Go node structs; exhaustive `match` replaces Go's type
//!   switches;
//! - the comment system (`Comment`, `CommentGroup`, `Doc`/`Comment` fields,
//!   `File.Comments`) is intentionally not ported: comments are discarded by
//!   the scanner and carry no meaning for the compiler;
//! - Go pointer fields become `Box<T>` (singly-owned subtrees) or `Option<T>`
//!   (nil-able), with `Rc<T>` only where Go shares nodes (`Scope`/`Object`).
//!
//! Syntax trees may be constructed directly, but they are typically produced
//! from Go source code by the parser.
//!
//! All nodes contain position information marking the beginning of the
//! corresponding source text segment; it is accessible via the `pos` accessor
//! method. Nodes may contain additional position info for language constructs
//! where comments may be found between parts of the construct (typically any
//! larger, parenthesized subpart). That position information is needed to
//! properly position comments when printing the construct.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::token::{NO_POS, Pos, Token};

use super::scope::{Object, Scope};

// ----------------------------------------------------------------------------
// Channel direction

/// The direction of a channel type is indicated by a bit
/// mask including one or both of the following constants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChanDir(pub i64);

impl ChanDir {
    pub const SEND: ChanDir = ChanDir(1 << 0);
    pub const RECV: ChanDir = ChanDir(1 << 1);
}

impl std::ops::BitOr for ChanDir {
    type Output = ChanDir;

    fn bitor(self, rhs: ChanDir) -> ChanDir {
        ChanDir(self.0 | rhs.0)
    }
}

// ----------------------------------------------------------------------------
// Fields

/// A Field represents a Field declaration list in a struct type,
/// a method list in an interface type, or a parameter/result declaration
/// in a signature.
///
/// [`Field::names`] is empty for unnamed parameters (parameter lists which
/// only contain types) and embedded struct fields. In the latter case, the
/// field name is the type name.
///
/// (Go's `Doc`/`Comment` documentation fields are not ported.)
#[derive(Clone, Debug)]
pub struct Field {
    pub names: Vec<Ident>,     // field/method/(type) parameter names; or empty
    pub typ: Option<Expr>,     // field/method/parameter type; or nil
    pub tag: Option<BasicLit>, // field tag; or nil
}

impl Field {
    pub fn pos(&self) -> Pos {
        if let Some(name) = self.names.first() {
            return name.pos();
        }
        if let Some(typ) = &self.typ {
            return typ.pos();
        }
        NO_POS
    }

    pub fn end(&self) -> Pos {
        if let Some(tag) = &self.tag {
            return tag.end();
        }
        if let Some(typ) = &self.typ {
            return typ.end();
        }
        if let Some(name) = self.names.last() {
            return name.end();
        }
        NO_POS
    }
}

/// A FieldList represents a list of Fields, enclosed by parentheses,
/// curly braces, or square brackets.
#[derive(Clone, Debug)]
pub struct FieldList {
    pub opening: Pos,     // position of opening parenthesis/brace/bracket, if any
    pub list: Vec<Field>, // field list
    pub closing: Pos,     // position of closing parenthesis/brace/bracket, if any
}

impl FieldList {
    pub fn pos(&self) -> Pos {
        if self.opening.is_valid() {
            return self.opening;
        }
        // the list should not be empty in this case;
        // be conservative and guard against bad ASTs
        if let Some(first) = self.list.first() {
            return first.pos();
        }
        NO_POS
    }

    pub fn end(&self) -> Pos {
        if self.closing.is_valid() {
            return self.closing + 1;
        }
        // the list should not be empty in this case;
        // be conservative and guard against bad ASTs
        if let Some(last) = self.list.last() {
            return last.end();
        }
        NO_POS
    }

    /// Returns the number of parameters or struct fields represented by a [`FieldList`].
    pub fn num_fields(&self) -> usize {
        let mut n = 0;
        for g in &self.list {
            let m = g.names.len();
            n += if m == 0 { 1 } else { m };
        }
        n
    }
}

// ----------------------------------------------------------------------------
// Expressions and types

/// An expression is represented by a tree consisting of one or more of the
/// following concrete expression nodes.

/// A BadExpr node is a placeholder for an expression containing
/// syntax errors for which a correct expression node cannot be created.
#[derive(Clone, Debug)]
pub struct BadExpr {
    pub from: Pos, // position range of bad expression
    pub to: Pos,
}

/// An Ident node represents an identifier.
#[derive(Clone, Debug)]
pub struct Ident {
    pub name_pos: Pos,           // identifier position
    pub name: String,            // identifier name
    pub obj: Option<Rc<Object>>, // denoted object, or nil. Deprecated: see Object.
}

/// An Ellipsis node stands for the "..." type in a
/// parameter list or the "..." length in an array type.
#[derive(Clone, Debug)]
pub struct Ellipsis {
    pub ellipsis: Pos,     // position of "..."
    pub elt: Option<Expr>, // ellipsis element type (parameter lists only); or nil
}

/// A BasicLit node represents a literal of basic type.
///
/// Note that for the CHAR and STRING kinds, the literal is stored with its
/// quotes. The crate-private `strconv` helpers (ported from Go's `strconv`)
/// can be used to unquote STRING and CHAR values. For raw string literals
/// (`Kind == token.STRING && Value[0] == '`'`), the `value` field contains
/// the string text without carriage returns (`\r`) that may have been
/// present in the source.
#[derive(Clone, Debug)]
pub struct BasicLit {
    pub value_pos: Pos, // literal position
    pub value_end: Pos, // position immediately after the literal
    pub kind: Token,    // token.INT, token.FLOAT, token.IMAG, token.CHAR, or token.STRING
    pub value: String, // literal string; e.g. 42, 0x7f, 3.14, 1e-9, 2.4i, 'a', '\x7f', "foo" or `\m\n\o`
}

/// A FuncLit node represents a function literal.
#[derive(Clone, Debug)]
pub struct FuncLit {
    pub typ: Box<FuncType>,   // function type
    pub body: Box<BlockStmt>, // function body
}

/// A CompositeLit node represents a composite literal.
#[derive(Clone, Debug)]
pub struct CompositeLit {
    pub typ: Option<Expr>, // literal type; or nil
    pub lbrace: Pos,       // position of "{"
    pub elts: Vec<Expr>,   // list of composite elements
    pub rbrace: Pos,       // position of "}"
    pub incomplete: bool,  // true if (source) expressions are missing in the Elts list
}

/// A ParenExpr node represents a parenthesized expression.
#[derive(Clone, Debug)]
pub struct ParenExpr {
    pub lparen: Pos, // position of "("
    pub x: Expr,     // parenthesized expression
    pub rparen: Pos, // position of ")"
}

/// A SelectorExpr node represents an expression followed by a selector.
#[derive(Clone, Debug)]
pub struct SelectorExpr {
    pub x: Expr,    // expression
    pub sel: Ident, // field selector
}

/// An IndexExpr node represents an expression followed by an index.
#[derive(Clone, Debug)]
pub struct IndexExpr {
    pub x: Expr,     // expression
    pub lbrack: Pos, // position of "["
    pub index: Expr, // index expression
    pub rbrack: Pos, // position of "]"
}

/// An IndexListExpr node represents an expression followed by multiple indices.
#[derive(Clone, Debug)]
pub struct IndexListExpr {
    pub x: Expr,            // expression
    pub lbrack: Pos,        // position of "["
    pub indices: Vec<Expr>, // index expressions
    pub rbrack: Pos,        // position of "]"
}

/// A SliceExpr node represents an expression followed by slice indices.
#[derive(Clone, Debug)]
pub struct SliceExpr {
    pub x: Expr,            // expression
    pub lbrack: Pos,        // position of "["
    pub low: Option<Expr>,  // begin of slice range; or nil
    pub high: Option<Expr>, // end of slice range; or nil
    pub max: Option<Expr>,  // maximum capacity of slice; or nil
    pub slice3: bool,       // true if 3-index slice (2 colons present)
    pub rbrack: Pos,        // position of "]"
}

/// A TypeAssertExpr node represents an expression followed by a type assertion.
#[derive(Clone, Debug)]
pub struct TypeAssertExpr {
    pub x: Expr,           // expression
    pub lparen: Pos,       // position of "("
    pub typ: Option<Expr>, // asserted type; nil means type switch X.(type)
    pub rparen: Pos,       // position of ")"
}

/// A CallExpr node represents an expression followed by an argument list.
#[derive(Clone, Debug)]
pub struct CallExpr {
    pub fun: Expr,       // function expression
    pub lparen: Pos,     // position of "("
    pub args: Vec<Expr>, // function arguments
    pub ellipsis: Pos,   // position of "..." (NO_POS if there is no "...")
    pub rparen: Pos,     // position of ")"
}

/// A StarExpr node represents an expression of the form "*" Expression.
/// Semantically it could be a unary "*" expression, or a pointer type.
#[derive(Clone, Debug)]
pub struct StarExpr {
    pub star: Pos, // position of "*"
    pub x: Expr,   // operand
}

/// A UnaryExpr node represents a unary expression.
/// Unary "*" expressions are represented via [`StarExpr`] nodes.
#[derive(Clone, Debug)]
pub struct UnaryExpr {
    pub op_pos: Pos, // position of Op
    pub op: Token,   // operator
    pub x: Expr,     // operand
}

/// A BinaryExpr node represents a binary expression.
#[derive(Clone, Debug)]
pub struct BinaryExpr {
    pub x: Expr,     // left operand
    pub op_pos: Pos, // position of Op
    pub op: Token,   // operator
    pub y: Expr,     // right operand
}

/// A KeyValueExpr node represents (key : value) pairs in composite literals.
#[derive(Clone, Debug)]
pub struct KeyValueExpr {
    pub key: Expr,
    pub colon: Pos, // position of ":"
    pub value: Expr,
}

/// A type is represented by a tree consisting of one or more of the following
/// type-specific expression nodes.

/// An ArrayType node represents an array or slice type.
#[derive(Clone, Debug)]
pub struct ArrayType {
    pub lbrack: Pos,       // position of "["
    pub len: Option<Expr>, // Ellipsis node for [...]T array types, nil for slice types
    pub elt: Expr,         // element type
}

/// A StructType node represents a struct type.
#[derive(Clone, Debug)]
pub struct StructType {
    pub struct_: Pos,              // position of "struct" keyword
    pub fields: Option<FieldList>, // list of field declarations
    pub incomplete: bool,          // true if (source) fields are missing in the Fields list
}

// Pointer types are represented via StarExpr nodes.

/// A FuncType node represents a function type.
#[derive(Clone, Debug)]
pub struct FuncType {
    pub func: Pos, // position of "func" keyword (NO_POS if there is no "func")
    pub type_params: Option<FieldList>, // type parameters; or nil
    pub params: Option<FieldList>, // (incoming) parameters; non-nil
    pub results: Option<FieldList>, // (outgoing) results; or nil
}

/// An InterfaceType node represents an interface type.
#[derive(Clone, Debug)]
pub struct InterfaceType {
    pub interface: Pos,             // position of "interface" keyword
    pub methods: Option<FieldList>, // list of embedded interfaces, methods, or types
    pub incomplete: bool, // true if (source) methods or types are missing in the Methods list
}

/// A MapType node represents a map type.
#[derive(Clone, Debug)]
pub struct MapType {
    pub map: Pos, // position of "map" keyword
    pub key: Expr,
    pub value: Expr,
}

/// A ChanType node represents a channel type.
#[derive(Clone, Debug)]
pub struct ChanType {
    pub begin: Pos,   // position of "chan" keyword or "<-" (whichever comes first)
    pub arrow: Pos,   // position of "<-" (NO_POS if there is no "<-")
    pub dir: ChanDir, // channel direction
    pub value: Expr,  // value type
}

// Pos and End implementations for expression/type nodes.

impl BadExpr {
    pub fn pos(&self) -> Pos {
        self.from
    }
    pub fn end(&self) -> Pos {
        self.to
    }
}

impl Ident {
    pub fn pos(&self) -> Pos {
        self.name_pos
    }
    pub fn end(&self) -> Pos {
        self.name_pos + self.name.len() as i64
    }
}

impl Ellipsis {
    pub fn pos(&self) -> Pos {
        self.ellipsis
    }

    pub fn end(&self) -> Pos {
        if let Some(elt) = &self.elt {
            return elt.end();
        }
        self.ellipsis + 3 // len("...")
    }
}

impl BasicLit {
    pub fn pos(&self) -> Pos {
        self.value_pos
    }

    pub fn end(&self) -> Pos {
        if !self.value_end.is_valid() {
            // Not from parser; use a heuristic.
            return self.value_pos + self.value.len() as i64;
        }
        self.value_end
    }
}

impl FuncLit {
    pub fn pos(&self) -> Pos {
        self.typ.pos()
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

impl CompositeLit {
    pub fn pos(&self) -> Pos {
        if let Some(typ) = &self.typ {
            return typ.pos();
        }
        self.lbrace
    }

    pub fn end(&self) -> Pos {
        self.rbrace + 1
    }
}

impl ParenExpr {
    pub fn pos(&self) -> Pos {
        self.lparen
    }
    pub fn end(&self) -> Pos {
        self.rparen + 1
    }
}

impl SelectorExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.sel.end()
    }
}

impl IndexExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.rbrack + 1
    }
}

impl IndexListExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.rbrack + 1
    }
}

impl SliceExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.rbrack + 1
    }
}

impl TypeAssertExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.rparen + 1
    }
}

impl CallExpr {
    pub fn pos(&self) -> Pos {
        self.fun.pos()
    }
    pub fn end(&self) -> Pos {
        self.rparen + 1
    }
}

impl StarExpr {
    pub fn pos(&self) -> Pos {
        self.star
    }
    pub fn end(&self) -> Pos {
        self.x.end()
    }
}

impl UnaryExpr {
    pub fn pos(&self) -> Pos {
        self.op_pos
    }
    pub fn end(&self) -> Pos {
        self.x.end()
    }
}

impl BinaryExpr {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.y.end()
    }
}

impl KeyValueExpr {
    pub fn pos(&self) -> Pos {
        self.key.pos()
    }
    pub fn end(&self) -> Pos {
        self.value.end()
    }
}

impl ArrayType {
    pub fn pos(&self) -> Pos {
        self.lbrack
    }
    pub fn end(&self) -> Pos {
        self.elt.end()
    }
}

impl StructType {
    pub fn pos(&self) -> Pos {
        self.struct_
    }
    pub fn end(&self) -> Pos {
        // Go dereferences the Fields pointer here and would panic on nil.
        self.fields
            .as_ref()
            .expect("ast.StructType.End: nil Fields")
            .end()
    }
}

impl FuncType {
    pub fn pos(&self) -> Pos {
        if self.func.is_valid() || self.params.is_none() {
            // see issue 3870
            return self.func;
        }
        // interface method declarations have no "func" keyword
        self.params
            .as_ref()
            .expect("ast.FuncType.Pos: nil Params")
            .pos()
    }

    pub fn end(&self) -> Pos {
        if let Some(results) = &self.results {
            return results.end();
        }
        self.params
            .as_ref()
            .expect("ast.FuncType.End: nil Params")
            .end()
    }
}

impl InterfaceType {
    pub fn pos(&self) -> Pos {
        self.interface
    }
    pub fn end(&self) -> Pos {
        self.methods
            .as_ref()
            .expect("ast.InterfaceType.End: nil Methods")
            .end()
    }
}

impl MapType {
    pub fn pos(&self) -> Pos {
        self.map
    }
    pub fn end(&self) -> Pos {
        self.value.end()
    }
}

impl ChanType {
    pub fn pos(&self) -> Pos {
        self.begin
    }
    pub fn end(&self) -> Pos {
        self.value.end()
    }
}

// ----------------------------------------------------------------------------
// Convenience functions for Idents

impl Ident {
    /// Reports whether id starts with an upper-case letter.
    pub fn is_exported(&self) -> bool {
        crate::token::is_exported(&self.name)
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Go's Ident.String returns "<nil>" for a nil receiver; an Ident
        // cannot be nil in this port, so only the name is printed.
        write!(f, "{}", self.name)
    }
}

/// Creates a new [`Ident`] without position.
/// Useful for ASTs generated by code other than the Go parser.
pub fn new_ident(name: impl Into<String>) -> Ident {
    Ident {
        name_pos: NO_POS,
        name: name.into(),
        obj: None,
    }
}

/// Reports whether name starts with an upper-case letter.
pub fn is_exported(name: &str) -> bool {
    crate::token::is_exported(name)
}

// ----------------------------------------------------------------------------
// Statements

/// A statement is represented by a tree consisting of one or more of the
/// following concrete statement nodes.

/// A BadStmt node is a placeholder for statements containing
/// syntax errors for which no correct statement nodes can be created.
#[derive(Clone, Debug)]
pub struct BadStmt {
    pub from: Pos, // position range of bad statement
    pub to: Pos,
}

/// A DeclStmt node represents a declaration in a statement list.
#[derive(Clone, Debug)]
pub struct DeclStmt {
    pub decl: Decl, // *GenDecl with CONST, TYPE, or VAR token
}

/// An EmptyStmt node represents an empty statement.
/// The "position" of the empty statement is the position
/// of the immediately following (explicit or implicit) semicolon.
#[derive(Clone, Debug)]
pub struct EmptyStmt {
    pub semicolon: Pos, // position of following ";"
    pub implicit: bool, // if set, ";" was omitted in the source
}

/// A LabeledStmt node represents a labeled statement.
#[derive(Clone, Debug)]
pub struct LabeledStmt {
    pub label: Ident,
    pub colon: Pos, // position of ":"
    pub stmt: Stmt,
}

/// An ExprStmt node represents a (stand-alone) expression in a statement list.
#[derive(Clone, Debug)]
pub struct ExprStmt {
    pub x: Expr, // expression
}

/// A SendStmt node represents a send statement.
#[derive(Clone, Debug)]
pub struct SendStmt {
    pub chan_: Expr,
    pub arrow: Pos, // position of "<-"
    pub value: Expr,
}

/// An IncDecStmt node represents an increment or decrement statement.
#[derive(Clone, Debug)]
pub struct IncDecStmt {
    pub x: Expr,
    pub tok_pos: Pos, // position of Tok
    pub tok: Token,   // INC or DEC
}

/// An AssignStmt node represents an assignment or a short variable declaration.
#[derive(Clone, Debug)]
pub struct AssignStmt {
    pub lhs: Vec<Expr>,
    pub tok_pos: Pos, // position of Tok
    pub tok: Token,   // assignment token, DEFINE
    pub rhs: Vec<Expr>,
}

/// A GoStmt node represents a go statement.
#[derive(Clone, Debug)]
pub struct GoStmt {
    pub go_: Pos, // position of "go" keyword
    pub call: Box<CallExpr>,
}

/// A DeferStmt node represents a defer statement.
#[derive(Clone, Debug)]
pub struct DeferStmt {
    pub defer_: Pos, // position of "defer" keyword
    pub call: Box<CallExpr>,
}

/// A ReturnStmt node represents a return statement.
#[derive(Clone, Debug)]
pub struct ReturnStmt {
    pub return_: Pos,       // position of "return" keyword
    pub results: Vec<Expr>, // result expressions
}

/// A BranchStmt node represents a break, continue, goto,
/// or fallthrough statement.
#[derive(Clone, Debug)]
pub struct BranchStmt {
    pub tok_pos: Pos,         // position of Tok
    pub tok: Token,           // keyword token (BREAK, CONTINUE, GOTO, FALLTHROUGH)
    pub label: Option<Ident>, // label name; or nil
}

/// A BlockStmt node represents a braced statement list.
#[derive(Clone, Debug)]
pub struct BlockStmt {
    pub lbrace: Pos, // position of "{"
    pub list: Vec<Stmt>,
    pub rbrace: Pos, // position of "}", if any (may be absent due to syntax error)
}

/// An IfStmt node represents an if statement.
#[derive(Clone, Debug)]
pub struct IfStmt {
    pub if_: Pos,           // position of "if" keyword
    pub init: Option<Stmt>, // initialization statement; or nil
    pub cond: Expr,         // condition
    pub body: Box<BlockStmt>,
    pub else_: Option<Stmt>, // else branch; or nil
}

/// A CaseClause represents a case of an expression or type switch statement.
#[derive(Clone, Debug)]
pub struct CaseClause {
    pub case: Pos,       // position of "case" or "default" keyword
    pub list: Vec<Expr>, // list of expressions or types; empty means default case
    pub colon: Pos,      // position of ":"
    pub body: Vec<Stmt>, // statement list
}

/// A SwitchStmt node represents an expression switch statement.
#[derive(Clone, Debug)]
pub struct SwitchStmt {
    pub switch: Pos,          // position of "switch" keyword
    pub init: Option<Stmt>,   // initialization statement; or nil
    pub tag: Option<Expr>,    // tag expression; or nil
    pub body: Box<BlockStmt>, // CaseClauses only
}

/// A TypeSwitchStmt node represents a type switch statement.
#[derive(Clone, Debug)]
pub struct TypeSwitchStmt {
    pub switch: Pos,          // position of "switch" keyword
    pub init: Option<Stmt>,   // initialization statement; or nil
    pub assign: Stmt,         // x := y.(type) or y.(type)
    pub body: Box<BlockStmt>, // CaseClauses only
}

/// A CommClause node represents a case of a select statement.
#[derive(Clone, Debug)]
pub struct CommClause {
    pub case: Pos,          // position of "case" or "default" keyword
    pub comm: Option<Stmt>, // send or receive statement; nil means default case
    pub colon: Pos,         // position of ":"
    pub body: Vec<Stmt>,    // statement list
}

/// A SelectStmt node represents a select statement.
#[derive(Clone, Debug)]
pub struct SelectStmt {
    pub select: Pos,          // position of "select" keyword
    pub body: Box<BlockStmt>, // CommClauses only
}

/// A ForStmt represents a for statement.
#[derive(Clone, Debug)]
pub struct ForStmt {
    pub for_: Pos,          // position of "for" keyword
    pub init: Option<Stmt>, // initialization statement; or nil
    pub cond: Option<Expr>, // condition; or nil
    pub post: Option<Stmt>, // post iteration statement; or nil
    pub body: Box<BlockStmt>,
}

/// A RangeStmt represents a for statement with a range clause.
#[derive(Clone, Debug)]
pub struct RangeStmt {
    pub for_: Pos,           // position of "for" keyword
    pub key: Option<Expr>,   // Key may be nil
    pub value: Option<Expr>, // Value may be nil
    pub tok_pos: Pos,        // position of Tok; invalid if Key == nil
    pub tok: Token,          // ILLEGAL if Key == nil, ASSIGN, DEFINE
    pub range: Pos,          // position of "range" keyword
    pub x: Expr,             // value to range over
    pub body: Box<BlockStmt>,
}

// Pos and End implementations for statement nodes.

impl BadStmt {
    pub fn pos(&self) -> Pos {
        self.from
    }
    pub fn end(&self) -> Pos {
        self.to
    }
}

impl DeclStmt {
    pub fn pos(&self) -> Pos {
        self.decl.pos()
    }
    pub fn end(&self) -> Pos {
        self.decl.end()
    }
}

impl EmptyStmt {
    pub fn pos(&self) -> Pos {
        self.semicolon
    }

    pub fn end(&self) -> Pos {
        if self.implicit {
            return self.semicolon;
        }
        self.semicolon + 1 // len(";")
    }
}

impl LabeledStmt {
    pub fn pos(&self) -> Pos {
        self.label.pos()
    }
    pub fn end(&self) -> Pos {
        self.stmt.end()
    }
}

impl ExprStmt {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }
    pub fn end(&self) -> Pos {
        self.x.end()
    }
}

impl SendStmt {
    pub fn pos(&self) -> Pos {
        self.chan_.pos()
    }
    pub fn end(&self) -> Pos {
        self.value.end()
    }
}

impl IncDecStmt {
    pub fn pos(&self) -> Pos {
        self.x.pos()
    }

    pub fn end(&self) -> Pos {
        self.tok_pos + 2 // len("++")
    }
}

impl AssignStmt {
    pub fn pos(&self) -> Pos {
        self.lhs
            .first()
            .expect("ast.AssignStmt.Pos: empty Lhs")
            .pos()
    }

    pub fn end(&self) -> Pos {
        // Go indexes the last element and would panic on an empty list.
        self.rhs
            .last()
            .expect("ast.AssignStmt.End: empty Rhs")
            .end()
    }
}

impl GoStmt {
    pub fn pos(&self) -> Pos {
        self.go_
    }
    pub fn end(&self) -> Pos {
        self.call.end()
    }
}

impl DeferStmt {
    pub fn pos(&self) -> Pos {
        self.defer_
    }
    pub fn end(&self) -> Pos {
        self.call.end()
    }
}

impl ReturnStmt {
    pub fn pos(&self) -> Pos {
        self.return_
    }

    pub fn end(&self) -> Pos {
        if let Some(last) = self.results.last() {
            return last.end();
        }
        self.return_ + 6 // len("return")
    }
}

impl BranchStmt {
    pub fn pos(&self) -> Pos {
        self.tok_pos
    }

    pub fn end(&self) -> Pos {
        if let Some(label) = &self.label {
            return label.end();
        }
        self.tok_pos + self.tok.to_string().len() as i64
    }
}

impl BlockStmt {
    pub fn pos(&self) -> Pos {
        self.lbrace
    }

    pub fn end(&self) -> Pos {
        if self.rbrace.is_valid() {
            return self.rbrace + 1;
        }
        if let Some(last) = self.list.last() {
            return last.end();
        }
        self.lbrace + 1
    }
}

impl IfStmt {
    pub fn pos(&self) -> Pos {
        self.if_
    }

    pub fn end(&self) -> Pos {
        if let Some(else_) = &self.else_ {
            return else_.end();
        }
        self.body.end()
    }
}

impl CaseClause {
    pub fn pos(&self) -> Pos {
        self.case
    }

    pub fn end(&self) -> Pos {
        if let Some(last) = self.body.last() {
            return last.end();
        }
        self.colon + 1
    }
}

impl SwitchStmt {
    pub fn pos(&self) -> Pos {
        self.switch
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

impl TypeSwitchStmt {
    pub fn pos(&self) -> Pos {
        self.switch
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

impl CommClause {
    pub fn pos(&self) -> Pos {
        self.case
    }

    pub fn end(&self) -> Pos {
        if let Some(last) = self.body.last() {
            return last.end();
        }
        self.colon + 1
    }
}

impl SelectStmt {
    pub fn pos(&self) -> Pos {
        self.select
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

impl ForStmt {
    pub fn pos(&self) -> Pos {
        self.for_
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

impl RangeStmt {
    pub fn pos(&self) -> Pos {
        self.for_
    }
    pub fn end(&self) -> Pos {
        self.body.end()
    }
}

// ----------------------------------------------------------------------------
// Declarations

/// A Spec node represents a single (non-parenthesized) import,
/// constant, type, or variable declaration.

/// An ImportSpec node represents a single package import.
///
/// (Go's `Doc`/`Comment` fields and the printer-oriented `EndPos` field are
/// not ported; the parser always provides a path.)
#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub name: Option<Ident>, // local package name (including "."); or nil
    pub path: BasicLit,      // import path
}

/// A ValueSpec node represents a constant or variable declaration
/// (ConstSpec or VarSpec production).
#[derive(Clone, Debug)]
pub struct ValueSpec {
    pub names: Vec<Ident>, // value names (len(Names) > 0)
    pub typ: Option<Expr>, // value type; or nil
    pub values: Vec<Expr>, // initial values
}

/// A TypeSpec node represents a type declaration (TypeSpec production).
#[derive(Clone, Debug)]
pub struct TypeSpec {
    pub name: Ident,                    // type name
    pub type_params: Option<FieldList>, // type parameters; or nil
    pub assign: Pos,                    // position of '=', if any
    pub typ: Expr, // *Ident, *ParenExpr, *SelectorExpr, *StarExpr, or any of the *XxxTypes
}

// Pos and End implementations for spec nodes.

impl ImportSpec {
    pub fn pos(&self) -> Pos {
        if let Some(name) = &self.name {
            return name.pos();
        }
        self.path.pos()
    }

    pub fn end(&self) -> Pos {
        self.path.end()
    }
}

impl ValueSpec {
    pub fn pos(&self) -> Pos {
        self.names
            .first()
            .expect("ast.ValueSpec.Pos: empty Names")
            .pos()
    }

    pub fn end(&self) -> Pos {
        if let Some(last) = self.values.last() {
            return last.end();
        }
        if let Some(typ) = &self.typ {
            return typ.end();
        }
        self.names
            .last()
            .expect("ast.ValueSpec.End: empty Names")
            .end()
    }
}

impl TypeSpec {
    pub fn pos(&self) -> Pos {
        self.name.pos()
    }
    pub fn end(&self) -> Pos {
        self.typ.end()
    }
}

/// A declaration is represented by one of the following declaration nodes.

/// A BadDecl node is a placeholder for a declaration containing
/// syntax errors for which a correct declaration node cannot be created.
#[derive(Clone, Debug)]
pub struct BadDecl {
    pub from: Pos, // position range of bad declaration
    pub to: Pos,
}

/// A GenDecl node (generic declaration node) represents an import,
/// constant, type or variable declaration. A valid Lparen position
/// (`Lparen.is_valid()`) indicates a parenthesized declaration.
///
/// Relationship between Tok value and Specs element type:
///
/// | token.IMPORT | [Spec::ImportSpec] |
/// |--------------|---------------------|
/// | token.CONST  | [Spec::ValueSpec]   |
/// | token.TYPE   | [Spec::TypeSpec]    |
/// | token.VAR    | [Spec::ValueSpec]   |
#[derive(Clone, Debug)]
pub struct GenDecl {
    pub tok_pos: Pos, // position of Tok
    pub tok: Token,   // IMPORT, CONST, TYPE, or VAR
    pub lparen: Pos,  // position of '(', if any
    pub specs: Vec<Spec>,
    pub rparen: Pos, // position of ')', if any
}

/// A FuncDecl node represents a function declaration.
#[derive(Clone, Debug)]
pub struct FuncDecl {
    pub recv: Option<FieldList>, // receiver (methods); or nil (functions)
    pub name: Ident,             // function/method name
    pub typ: FuncType, // function signature: type and value parameters, results, and position of "func" keyword
    pub body: Option<Box<BlockStmt>>, // function body; or nil for external (non-Go) function
}

// Pos and End implementations for declaration nodes.

impl BadDecl {
    pub fn pos(&self) -> Pos {
        self.from
    }
    pub fn end(&self) -> Pos {
        self.to
    }
}

impl GenDecl {
    pub fn pos(&self) -> Pos {
        self.tok_pos
    }

    pub fn end(&self) -> Pos {
        if self.rparen.is_valid() {
            return self.rparen + 1;
        }
        self.specs
            .first()
            .expect("ast.GenDecl.End: empty Specs")
            .end()
    }
}

impl FuncDecl {
    pub fn pos(&self) -> Pos {
        self.typ.pos()
    }

    pub fn end(&self) -> Pos {
        if let Some(body) = &self.body {
            return body.end();
        }
        self.typ.end()
    }
}

// ----------------------------------------------------------------------------
// Files and packages

/// A File node represents a Go source file.
///
/// (Go's comment lists, documentation comment and `GoVersion` are not ported;
/// comments carry no meaning for the compiler.)
#[derive(Clone, Debug)]
pub struct File {
    pub package: Pos,     // position of "package" keyword
    pub name: Ident,      // package name
    pub decls: Vec<Decl>, // top-level declarations

    pub file_start: Pos, // start and end of entire file
    pub file_end: Pos,
    pub scope: Option<Rc<Scope>>, // package scope (this file only). Deprecated: see Object
    pub imports: Vec<ImportSpec>, // imports in this file
    pub unresolved: Vec<Ident>,   // unresolved identifiers in this file. Deprecated: see Object
}

impl File {
    /// Returns the position of the package declaration.
    /// It may be invalid, for example in an empty file.
    ///
    /// (Use `file_start` for the start of the entire file. It is always valid.)
    pub fn pos(&self) -> Pos {
        self.package
    }

    /// Returns the end of the last declaration in the file.
    /// It may be invalid, for example in an empty file.
    ///
    /// (Use `file_end` for the end of the entire file. It is always valid.)
    pub fn end(&self) -> Pos {
        if let Some(last) = self.decls.last() {
            return last.end();
        }
        self.name.end()
    }
}

/// A Package node represents a set of source files collectively building a Go package.
///
/// Deprecated: use the type checker instead; see [`Object`].
#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,                          // package name
    pub scope: Option<Rc<Scope>>,              // package scope across all files
    pub imports: BTreeMap<String, Rc<Object>>, // map of package id -> package object
    pub files: BTreeMap<String, File>,         // Go source files by filename
}

impl Package {
    pub fn pos(&self) -> Pos {
        NO_POS
    }
    pub fn end(&self) -> Pos {
        NO_POS
    }
}

// ----------------------------------------------------------------------------
// Node category enums
//
// Go's Node interface hierarchy is mapped to enums whose variants carry the
// concrete node structs, so that Go's type switches become exhaustive matches.

/// All expression/type nodes: every [`Expr`] variant wraps the corresponding
/// Go expression or type struct.
///
/// Variants whose payload holds an `Expr`-typed field directly are boxed to
/// keep the enum size finite.
#[derive(Clone, Debug)]
pub enum Expr {
    BadExpr(BadExpr),
    Ident(Ident),
    Ellipsis(Box<Ellipsis>),
    BasicLit(BasicLit),
    FuncLit(FuncLit),
    CompositeLit(Box<CompositeLit>),
    ParenExpr(Box<ParenExpr>),
    SelectorExpr(Box<SelectorExpr>),
    IndexExpr(Box<IndexExpr>),
    IndexListExpr(Box<IndexListExpr>),
    SliceExpr(Box<SliceExpr>),
    TypeAssertExpr(Box<TypeAssertExpr>),
    CallExpr(Box<CallExpr>),
    StarExpr(Box<StarExpr>),
    UnaryExpr(Box<UnaryExpr>),
    BinaryExpr(Box<BinaryExpr>),
    KeyValueExpr(Box<KeyValueExpr>),
    ArrayType(Box<ArrayType>),
    StructType(StructType),
    FuncType(FuncType),
    InterfaceType(InterfaceType),
    MapType(Box<MapType>),
    ChanType(Box<ChanType>),
}

/// All statement nodes.
///
/// Variants whose payload holds a `Stmt`-typed field directly are boxed to
/// keep the enum size finite.
#[derive(Clone, Debug)]
pub enum Stmt {
    BadStmt(BadStmt),
    DeclStmt(DeclStmt),
    EmptyStmt(EmptyStmt),
    LabeledStmt(Box<LabeledStmt>),
    ExprStmt(ExprStmt),
    SendStmt(SendStmt),
    IncDecStmt(IncDecStmt),
    AssignStmt(AssignStmt),
    GoStmt(GoStmt),
    DeferStmt(DeferStmt),
    ReturnStmt(ReturnStmt),
    BranchStmt(BranchStmt),
    BlockStmt(BlockStmt),
    IfStmt(Box<IfStmt>),
    CaseClause(CaseClause),
    SwitchStmt(Box<SwitchStmt>),
    TypeSwitchStmt(Box<TypeSwitchStmt>),
    CommClause(Box<CommClause>),
    SelectStmt(SelectStmt),
    ForStmt(Box<ForStmt>),
    RangeStmt(RangeStmt),
}

/// All spec (non-parenthesized import/const/type/var declaration) nodes.
#[derive(Clone, Debug)]
pub enum Spec {
    ImportSpec(ImportSpec),
    ValueSpec(ValueSpec),
    TypeSpec(TypeSpec),
}

/// All declaration nodes.
#[derive(Clone, Debug)]
pub enum Decl {
    BadDecl(BadDecl),
    GenDecl(GenDecl),
    FuncDecl(FuncDecl),
}

/// Delegating pos/end implementations for the category enums.
macro_rules! impl_pos_end_enum {
    ($( $cat:ident: $( $v:ident ),* $(,)? );* $(;)?) => {
        $(
            impl $cat {
                /// Returns the position of the first character belonging to the node.
                pub fn pos(&self) -> Pos {
                    match self {
                        $( $cat::$v(x) => x.pos(), )*
                    }
                }

                /// Returns the position of the first character immediately after the node.
                pub fn end(&self) -> Pos {
                    match self {
                        $( $cat::$v(x) => x.end(), )*
                    }
                }
            }
        )*
    };
}

impl_pos_end_enum! {
    Expr: BadExpr, Ident, Ellipsis, BasicLit, FuncLit, CompositeLit, ParenExpr,
          SelectorExpr, IndexExpr, IndexListExpr, SliceExpr, TypeAssertExpr,
          CallExpr, StarExpr, UnaryExpr, BinaryExpr, KeyValueExpr, ArrayType,
          StructType, FuncType, InterfaceType, MapType, ChanType;
    Stmt: BadStmt, DeclStmt, EmptyStmt, LabeledStmt, ExprStmt, SendStmt,
          IncDecStmt, AssignStmt, GoStmt, DeferStmt, ReturnStmt, BranchStmt,
          BlockStmt, IfStmt, CaseClause, SwitchStmt, TypeSwitchStmt, CommClause,
          SelectStmt, ForStmt, RangeStmt;
    Spec: ImportSpec, ValueSpec, TypeSpec;
    Decl: BadDecl, GenDecl, FuncDecl;
}

/// Unparen returns the expression with any enclosing parentheses removed.
pub fn unparen(e: Expr) -> Expr {
    let mut e = e;
    loop {
        match e {
            Expr::ParenExpr(paren) => e = paren.x,
            _ => return e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ident_helpers() {
        let id = new_ident("main");
        assert!(!id.name_pos.is_valid());
        assert_eq!(id.name, "main");
        assert!(id.obj.is_none());
        assert!(!is_exported("main"));
        assert!(is_exported("Main"));
        assert!(!id.is_exported());
        assert_eq!(id.to_string(), "main");
    }

    #[test]
    fn unparen_removes_all_parens() {
        let x = new_ident("x");
        let e = Expr::ParenExpr(Box::new(ParenExpr {
            lparen: Pos::from_int(1),
            x: Expr::ParenExpr(Box::new(ParenExpr {
                lparen: Pos::from_int(2),
                x: Expr::Ident(x.clone()),
                rparen: Pos::from_int(3),
            })),
            rparen: Pos::from_int(4),
        }));
        match unparen(e) {
            Expr::Ident(id) => assert_eq!(id.name, "x"),
            other => panic!("expected Ident, got {other:?}"),
        }
    }

    #[test]
    fn ident_pos_end() {
        let id = Ident {
            name_pos: Pos::from_int(10),
            name: "foo".into(),
            obj: None,
        };
        assert_eq!(id.pos(), Pos::from_int(10));
        assert_eq!(id.end(), Pos::from_int(13)); // 10 + len("foo")
    }

    #[test]
    fn basic_lit_end_uses_value_end_when_valid() {
        // Heuristic fallback: value_pos + len(value).
        let lit = BasicLit {
            value_pos: Pos::from_int(20),
            value_end: NO_POS,
            kind: Token::String,
            value: "\"abc\"".into(),
        };
        assert_eq!(lit.end(), Pos::from_int(25));

        // Parser-provided value_end wins.
        let lit = BasicLit {
            value_pos: Pos::from_int(20),
            value_end: Pos::from_int(26),
            kind: Token::String,
            value: "\"abc\"".into(),
        };
        assert_eq!(lit.end(), Pos::from_int(26));
    }

    #[test]
    fn enum_pos_end_delegation() {
        let id = Ident {
            name_pos: Pos::from_int(5),
            name: "y".into(),
            obj: None,
        };
        let e = Expr::Ident(id);
        assert_eq!(e.pos(), Pos::from_int(5));
        assert_eq!(e.end(), Pos::from_int(6));

        // ParenExpr: end = rparen + 1.
        let e = Expr::ParenExpr(Box::new(ParenExpr {
            lparen: Pos::from_int(1),
            x: Expr::Ident(new_ident("y")),
            rparen: Pos::from_int(7),
        }));
        assert_eq!(e.end(), Pos::from_int(8));

        let s = Stmt::EmptyStmt(EmptyStmt {
            semicolon: Pos::from_int(9),
            implicit: false,
        });
        assert_eq!(s.end(), Pos::from_int(10));
        let s = Stmt::EmptyStmt(EmptyStmt {
            semicolon: Pos::from_int(9),
            implicit: true,
        });
        assert_eq!(s.end(), Pos::from_int(9));
    }

    #[test]
    fn file_pos_end_falls_back_to_package_name() {
        let f = File {
            package: Pos::from_int(30),
            name: new_ident("p"),
            decls: vec![],
            file_start: Pos::from_int(0),
            file_end: Pos::from_int(40),
            scope: None,
            imports: vec![],
            unresolved: vec![],
        };
        assert_eq!(f.pos(), Pos::from_int(30));
        // empty decls: falls back to name.end() = NO_POS + len("p") = Pos(1)
        assert_eq!(f.end(), Pos::from_int(1));
    }
}
