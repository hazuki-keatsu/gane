// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2009 The Go Authors.
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu
//
// Adapted from the Go standard library for Gane.

//! The core of the Go source parser.
//!
//! This module is ported from Go's standard `go/parser/parser.go`.
//! Go's private function order is preserved. Adaptations (besides those
//! listed in the [`super`] module documentation):
//!
//! - ordinary comments are dropped. Single-line `//go:` and `//gane:` command
//!   comments are retained and attached to the immediately following AST node;
//! - `expectSemi` no longer returns the line comment, so it returns `()`;
//! - Go's `bailout` panic is a private marker used for early termination
//!   after too many parse errors;
//! - Go's `incNestLev`/`decNestLev` pair is adapted to a nesting guard
//!   ([`NestGuard`]) built on `Rc<Cell<i32>>` so that the guard does not
//!   borrow the parser while further `&mut self` calls are made;
//! - Go's token-set `map[token.Token]bool` arguments are adapted to the
//!   [`TokenSet`] function type.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::ast::File as AstFile;
use crate::ast::*;
use crate::scanner::{ErrorHandler, ErrorList, SCAN_COMMENTS, Scanner};
use crate::token::{AstNodeId, File, NO_POS, Pos, Token};

use super::interface::{ALL_ERRORS, DECLARATION_ERRORS, IMPORTS_ONLY, Mode, PACKAGE_CLAUSE_ONLY};

/// The parser structure holds the parser's internal state.
pub(crate) struct Parser<'src> {
    file: Rc<File>,
    errors: Rc<RefCell<ErrorList>>,
    scanner: Scanner<'src>,

    // Go's tracing state and general comment/doc-comment state are not ported.
    mode: Mode, // parsing mode

    // Next token
    pos: Pos, // token position
    /// One token look-ahead.
    pub(crate) tok: Token,
    /// Token literal.
    pub(crate) lit: String,

    // Error recovery
    // (used to limit the number of calls to `advance`
    // w/o making scanning progress - avoids potential endless
    // loops across multiple parser functions during error recovery)
    sync_pos: Pos, // last synchronization position
    sync_cnt: i32, // number of `advance` calls without progress

    // Non-syntactic parser control
    expr_lev: i32, // < 0: in control clause, >= 0: in expression
    in_rhs: bool,  // if set, the parser is parsing a rhs expression

    imports: Vec<ImportSpec>, // list of imports
    pending_commands: Vec<CommentCommand>,

    // nest_lev is used to track and limit the recursion depth
    // during parsing. It is `Rc<Cell<i32>>` so that a NestGuard can
    // decrement it without borrowing the parser.
    nest_lev: Rc<Cell<i32>>,
}

/// A bailout panic is raised to indicate early termination (Go's
/// `bailout`). `pos`/`msg` are only populated when bailing out of object
/// resolution (exceeded scope depth); the parser's own 10-error cap panics
/// with empty fields and is silently swallowed by the entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bailout {
    pub(crate) pos: Pos,
    pub(crate) msg: String,
}

impl<'src> Parser<'src> {
    /// Creates and initializes a parser for `src`, scanning the first token.
    /// Scanner errors are reported into the parser's shared error list
    /// The scanner returns comments so supported compiler commands can be
    /// collected; all other comments are skipped by [`Parser::next`].
    pub(crate) fn new(file: Rc<File>, src: &'src [u8], mode: Mode) -> Parser<'src> {
        let errors = Rc::new(RefCell::new(ErrorList::default()));
        let eh: ErrorHandler = {
            let errors = errors.clone();
            Box::new(move |pos, msg| errors.borrow_mut().add(pos, msg))
        };
        let scanner = Scanner::new(file.clone(), src, Some(eh), SCAN_COMMENTS);
        let mut p = Parser {
            file,
            errors,
            scanner,
            mode,
            pos: NO_POS,
            tok: Token::Illegal,
            lit: String::new(),
            sync_pos: NO_POS,
            sync_cnt: 0,
            expr_lev: 0,
            in_rhs: false,
            imports: Vec::new(),
            pending_commands: Vec::new(),
            nest_lev: Rc::new(Cell::new(0)),
        };
        p.next();
        p
    }

    /// Returns the accumulated errors (scanner and parser), sorted by
    /// position. Returns None if there are no errors.
    pub(crate) fn sorted_errors(&self) -> Option<ErrorList> {
        let mut errors = self.errors.borrow_mut();
        errors.sort(); // Go: p.errors.Sort()
        if errors.is_empty() {
            None
        } else {
            Some(errors.clone())
        }
    }

    // /// Returns the file being parsed.
    // pub(crate) fn file_rc(&self) -> Rc<File> {
    //     self.file.clone()
    // }

    /// Returns a clone of the shared error list handle.
    pub(crate) fn errors_rc(&self) -> Rc<RefCell<ErrorList>> {
        self.errors.clone()
    }

    /// Returns the end position of the current token.
    fn end(&self) -> Pos {
        self.scanner.end()
    }

    // ------------------------------------------------------------------------
    // Parsing support

    /// Advance to the next token.
    ///
    /// Comments are handled by [`Parser::next`].
    fn next0(&mut self) {
        let (pos, tok, lit) = self.scanner.scan();
        self.pos = pos;
        self.tok = tok;
        self.lit = lit;
    }

    /// Returns the physical source line for a position, ignoring `//line`.
    fn line_for(&self, pos: Pos) -> i64 {
        self.file.position_for(pos, false).line
    }

    fn consume_command(&mut self, pos: Pos, lit: &str, previous_line: i64) {
        let (kind, text) = if let Some(text) = lit.strip_prefix("//go:") {
            (CommentCommandKind::Go, text)
        } else if let Some(text) = lit.strip_prefix("//gane:") {
            (CommentCommandKind::Gane, text)
        } else {
            return;
        };

        // A command following source text on the same line is trailing, not leading.
        if self.line_for(pos) == previous_line {
            return;
        }
        self.pending_commands.push(CommentCommand {
            slash: pos,
            kind,
            text: text.to_string(),
        });
    }

    /// Takes only the consecutive command lines immediately preceding `target`.
    fn take_leading_commands(&mut self, target: Pos) -> Vec<CommentCommand> {
        let mut next_line = self.line_for(target);
        let mut start = self.pending_commands.len();
        while start > 0 {
            let line = self.line_for(self.pending_commands[start - 1].slash);
            if line + 1 != next_line {
                break;
            }
            start -= 1;
            next_line = line;
        }
        let commands = self.pending_commands.split_off(start);
        self.pending_commands.clear();
        commands
    }

    /// Takes commands from the file header.
    ///
    /// Unlike commands attached to declarations and fields, file-level
    /// commands (notably `//go:build`) conventionally have a blank line
    /// between the directive block and the package clause. They therefore
    /// must not use [`Self::take_leading_commands`]' adjacency rule.
    fn take_file_commands(&mut self) -> Vec<CommentCommand> {
        std::mem::take(&mut self.pending_commands)
    }

    /// Advance to the next non-comment token, retaining supported compiler
    /// commands until their following AST node is constructed.
    pub(crate) fn next(&mut self) {
        let previous_line = if self.pos.is_valid() && self.tok != Token::Comment {
            self.line_for(self.pos)
        } else {
            0
        };
        self.next0();
        while self.tok == Token::Comment {
            let pos = self.pos;
            let lit = self.lit.clone();
            self.consume_command(pos, &lit, previous_line);
            self.next0();
        }
    }

    fn error(&self, pos: Pos, msg: String) {
        let epos = self.file.position(pos);

        // If AllErrors is not set, discard errors reported on the same line
        // as the last recorded error and stop parsing if there are more than
        // 10 errors.
        if self.mode & ALL_ERRORS == Mode::default() {
            // The borrows are scoped so that no RefCell borrow is live
            // across the bailout panic below (a panic while holding a borrow
            // would poison the RefCell and abort on the next borrow).
            let n = {
                let errors = self.errors.borrow();
                let n = errors.len();
                if n > 0
                    && errors
                        .iter()
                        .nth(n - 1)
                        .map(|err| err.pos.line)
                        .unwrap_or(0)
                        == epos.line
                {
                    return; // discard - likely a spurious error
                }
                n
            };
            if n > 10 {
                std::panic::panic_any(Bailout {
                    pos: NO_POS,
                    msg: String::new(),
                });
            }
        }

        self.errors.borrow_mut().add(epos, msg);
    }

    fn error_expected(&mut self, pos: Pos, msg: &str) {
        let mut m = String::from("expected ");
        m.push_str(msg);
        if pos == self.pos {
            // the error happened at the current position;
            // make the error message more specific
            if self.tok == Token::Semicolon && self.lit == "\n" {
                m.push_str(", found newline");
            } else if self.tok.is_literal() {
                // print 123 rather than 'INT', etc.
                m.push_str(", found ");
                m.push_str(&self.lit);
            } else {
                m.push_str(", found '");
                m.push_str(&self.tok.to_string());
                m.push('\'');
            }
        }
        self.error(pos, m);
    }

    pub(crate) fn expect(&mut self, tok: Token) -> Pos {
        let pos = self.pos;
        if self.tok != tok {
            let msg = format!("'{}'", tok);
            self.error_expected(pos, &msg);
        }
        self.next(); // make progress
        pos
    }

    /// expect2 is like expect, but it returns an invalid position
    /// if the expected token is not found.
    fn expect2(&mut self, tok: Token) -> Pos {
        let pos = if self.tok == tok {
            self.pos
        } else {
            let msg = format!("'{}'", tok);
            self.error_expected(self.pos, &msg);
            NO_POS
        };
        self.next(); // make progress
        pos
    }

    /// expect_closing is like expect but provides a better error message
    /// for the common case of a missing comma before a newline.
    fn expect_closing(&mut self, tok: Token, context: &str) -> Pos {
        if self.tok != tok && self.tok == Token::Semicolon && self.lit == "\n" {
            let msg = format!("missing ',' before newline in {context}");
            self.error(self.pos, msg);
            self.next();
        }
        self.expect(tok)
    }

    /// expectSemi consumes a semicolon.
    ///
    /// (Go returns the applicable line comment; comments are not collected
    /// in this port.)
    fn expect_semi(&mut self) {
        match self.tok {
            Token::RParen | Token::RBrace => {
                // semicolon is optional before a closing ')' or '}'
            }
            Token::Comma => {
                // permit a ',' instead of a ';' but complain
                self.error_expected(self.pos, "';'");
                self.next();
            }
            Token::Semicolon => {
                // explicit or artificial semicolon
                self.next();
            }
            _ => {
                self.error_expected(self.pos, "';'");
                self.advance(stmt_start);
            }
        }
    }

    fn at_comma(&mut self, context: &str, follow: Token) -> bool {
        if self.tok == Token::Comma {
            return true;
        }
        if self.tok != follow {
            let mut msg = "missing ','".to_string();
            if self.tok == Token::Semicolon && self.lit == "\n" {
                msg.push_str(" before newline");
            }
            msg.push_str(" in ");
            msg.push_str(context);
            self.error(self.pos, msg);
            return true; // "insert" comma and continue
        }
        false
    }

    /// advance consumes tokens until the current token p.tok
    /// is in the 'to' set, or token.EOF. For error recovery.
    fn advance(&mut self, to: TokenSet) {
        while self.tok != Token::EOF {
            if to(self.tok) {
                // Return only if parser made some progress since last
                // sync or if it has not reached 10 advance calls without
                // progress. Otherwise consume at least one token to
                // avoid an endless parser loop (it is possible that
                // both parseOperand and parseStmt call advance and
                // correctly do not advance, thus the need for the
                // invocation limit p.syncCnt).
                if self.pos == self.sync_pos && self.sync_cnt < 10 {
                    self.sync_cnt += 1;
                    return;
                }
                if self.pos > self.sync_pos {
                    self.sync_pos = self.pos;
                    self.sync_cnt = 0;
                    return;
                }
                // Reaching here indicates a parser bug, likely an
                // incorrect token list in this function, but it only
                // leads to skipping of possibly correct code if a
                // previous error is present, and thus is preferred
                // over a non-terminating parse.
            }
            self.next();
        }
    }

    // ------------------------------------------------------------------------
    // Identifiers

    fn parse_ident(&mut self) -> Ident {
        let pos = self.pos;
        let mut name = String::from("_");
        if self.tok == Token::Ident {
            name = self.lit.clone();
            self.next();
        } else {
            self.expect(Token::Ident); // use expect() error handling
        }
        Ident {
            node_id: AstNodeId::INVALID,
            name_pos: pos,
            name,
        }
    }

    fn parse_ident_list(&mut self) -> Vec<Ident> {
        let mut list = Vec::new();
        list.push(self.parse_ident());
        while self.tok == Token::Comma {
            self.next();
            list.push(self.parse_ident());
        }
        list
    }

    // ------------------------------------------------------------------------
    // Common productions
    //
    // (parseExprList and parseList are defined once the expression parsing
    // group, on which they depend, is ported.)

    // ------------------------------------------------------------------------
    // Types

    fn parse_type(&mut self) -> Expr {
        let typ = self.try_ident_or_type();

        if typ.is_none() {
            let pos = self.pos;
            self.error_expected(pos, "type");
            self.advance(expr_end);
            return Expr::BadExpr(BadExpr {
                node_id: AstNodeId::INVALID,
                from: pos,
                to: self.pos,
            });
        }

        typ.unwrap()
    }

    fn parse_qualified_ident(&mut self, ident: Option<Ident>) -> Expr {
        let typ = self.parse_type_name(ident);
        if self.tok == Token::LBrack {
            return self.parse_type_instance(typ);
        }
        typ
    }

    /// If the result is an identifier, it is retained as source syntax.
    fn parse_type_name(&mut self, ident: Option<Ident>) -> Expr {
        let ident = match ident {
            Some(ident) => ident,
            None => self.parse_ident(),
        };

        if self.tok == Token::Period {
            // ident is a package name
            self.next();
            let sel = self.parse_ident();
            return Expr::SelectorExpr(Box::new(SelectorExpr {
                node_id: AstNodeId::INVALID,
                x: Expr::Ident(ident),
                sel,
            }));
        }

        Expr::Ident(ident)
    }

    /// "[" has already been consumed, and lbrack is its position.
    /// If len != nil it is the already consumed array length.
    fn parse_array_type(&mut self, lbrack: Pos, len: Option<Expr>) -> Expr {
        // Go: *ast.ArrayType
        let len = match len {
            Some(len) => Some(len),
            None => {
                self.expr_lev += 1;
                let len = if self.tok == Token::Ellipsis {
                    // always permit ellipsis for more fault-tolerant parsing
                    let ellipsis = Ellipsis {
                        node_id: AstNodeId::INVALID,
                        ellipsis: self.pos,
                        elt: None,
                    };
                    self.next();
                    Some(Expr::Ellipsis(Box::new(ellipsis)))
                } else if self.tok != Token::RBrack {
                    Some(self.parse_rhs())
                } else {
                    None
                };
                self.expr_lev -= 1;
                len
            }
        };
        if self.tok == Token::Comma {
            // Trailing commas are accepted in type parameter
            // lists but not in array type declarations.
            // Accept for better error handling but complain.
            self.error(self.pos, "unexpected comma; expecting ]".to_string());
            self.next();
        }
        self.expect(Token::RBrack);
        let elt = self.parse_type();
        Expr::ArrayType(Box::new(ArrayType {
            node_id: AstNodeId::INVALID,
            lbrack,
            len,
            elt,
        }))
    }

    fn parse_array_field_or_type_instance(&mut self, x: Ident) -> (Option<Ident>, Expr) {
        let lbrack = self.expect(Token::LBrack);
        let mut trailing_comma = NO_POS; // if valid, the position of a trailing comma preceding the ']'
        let mut args: Vec<Expr> = Vec::new();
        if self.tok != Token::RBrack {
            self.expr_lev += 1;
            args.push(self.parse_rhs());
            while self.tok == Token::Comma {
                let comma = self.pos;
                self.next();
                if self.tok == Token::RBrack {
                    trailing_comma = comma;
                    break;
                }
                args.push(self.parse_rhs());
            }
            self.expr_lev -= 1;
        }
        let rbrack = self.expect(Token::RBrack);

        if args.is_empty() {
            // x []E
            let elt = self.parse_type();
            return (
                Some(x),
                Expr::ArrayType(Box::new(ArrayType {
                    node_id: AstNodeId::INVALID,
                    lbrack,
                    len: None,
                    elt,
                })),
            );
        }

        // x [P]E or x[P]
        if args.len() == 1 {
            let elt = self.try_ident_or_type();
            if let Some(elt) = elt {
                // x [P]E
                if trailing_comma.is_valid() {
                    // Trailing commas are invalid in array type fields.
                    self.error(trailing_comma, "unexpected comma; expecting ]".to_string());
                }
                return (
                    Some(x),
                    Expr::ArrayType(Box::new(ArrayType {
                        node_id: AstNodeId::INVALID,
                        lbrack,
                        len: Some(args.into_iter().next().unwrap()),
                        elt,
                    })),
                );
            }
        }

        // x[P], x[P1, P2], ...
        (None, pack_index_expr(Expr::Ident(x), lbrack, args, rbrack))
    }

    fn parse_field_decl(&mut self) -> Field {
        let commands = self.take_leading_commands(self.pos);
        let mut names: Vec<Ident> = Vec::new();
        let mut typ: Option<Expr>;
        match self.tok {
            Token::Ident => {
                let name = self.parse_ident();
                if matches!(
                    self.tok,
                    Token::Period | Token::String | Token::Semicolon | Token::RBrace
                ) {
                    // embedded type
                    typ = Some(Expr::Ident(name.clone()));
                    if self.tok == Token::Period {
                        typ = Some(self.parse_qualified_ident(Some(name)));
                    }
                } else {
                    // name1, name2, ... T
                    names.push(name.clone());
                    while self.tok == Token::Comma {
                        self.next();
                        names.push(self.parse_ident());
                    }
                    // Careful dance: We don't know if we have an embedded instantiated
                    // type T[P1, P2, ...] or a field T of array type []E or [P]E.
                    if names.len() == 1 && self.tok == Token::LBrack {
                        let (name2, typ2) = self.parse_array_field_or_type_instance(name);
                        match name2 {
                            Some(name2) => names = vec![name2],
                            None => names = Vec::new(),
                        }
                        typ = Some(typ2);
                    } else {
                        // T P
                        typ = Some(self.parse_type());
                    }
                }
            }
            Token::Mul => {
                let star = self.pos;
                self.next();
                if self.tok == Token::LParen {
                    // *(T)
                    self.error(self.pos, "cannot parenthesize embedded type".to_string());
                    self.next();
                    typ = Some(self.parse_qualified_ident(None));
                    // expect closing ')' but no need to complain if missing
                    if self.tok == Token::RParen {
                        self.next();
                    }
                } else {
                    // *T
                    typ = Some(self.parse_qualified_ident(None));
                }
                typ = Some(Expr::StarExpr(Box::new(StarExpr {
                    node_id: AstNodeId::INVALID,
                    star,
                    x: typ.unwrap(),
                })));
            }
            Token::LParen => {
                self.error(self.pos, "cannot parenthesize embedded type".to_string());
                self.next();
                if self.tok == Token::Mul {
                    // (*T)
                    let star = self.pos;
                    self.next();
                    typ = Some(Expr::StarExpr(Box::new(StarExpr {
                        node_id: AstNodeId::INVALID,
                        star,
                        x: self.parse_qualified_ident(None),
                    })));
                } else {
                    // (T)
                    typ = Some(self.parse_qualified_ident(None));
                }
                // expect closing ')' but no need to complain if missing
                if self.tok == Token::RParen {
                    self.next();
                }
            }
            _ => {
                let pos = self.pos;
                self.error_expected(pos, "field name or embedded type");
                self.advance(expr_end);
                typ = Some(Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from: pos,
                    to: self.pos,
                }));
            }
        }

        let mut tag = None;
        if self.tok == Token::String {
            tag = Some(BasicLit {
                node_id: AstNodeId::INVALID,
                value_pos: self.pos,
                value_end: self.end(),
                kind: self.tok,
                value: self.lit.clone(),
            });
            self.next();
        }

        self.expect_semi();

        Field {
            node_id: AstNodeId::INVALID,
            commands,
            names,
            typ,
            tag,
        }
    }

    fn parse_struct_type(&mut self) -> Expr {
        let pos = self.expect(Token::Struct);
        let lbrace = self.expect(Token::LBrace);
        let mut list: Vec<Field> = Vec::new();
        while matches!(self.tok, Token::Ident | Token::Mul | Token::LParen) {
            // a field declaration cannot start with a '(' but we accept
            // it here for more robust parsing and better error messages
            // (parseFieldDecl will check and complain if necessary)
            list.push(self.parse_field_decl());
        }
        let rbrace = self.expect(Token::RBrace);

        Expr::StructType(StructType {
            node_id: AstNodeId::INVALID,
            struct_: pos,
            fields: Some(FieldList {
                node_id: AstNodeId::INVALID,
                opening: lbrace,
                list,
                closing: rbrace,
            }),
            incomplete: false,
        })
    }

    fn parse_pointer_type(&mut self) -> Expr {
        let star = self.expect(Token::Mul);
        let base = self.parse_type();
        Expr::StarExpr(Box::new(StarExpr {
            node_id: AstNodeId::INVALID,
            star,
            x: base,
        }))
    }

    fn parse_dots_type(&mut self) -> Expr {
        let pos = self.expect(Token::Ellipsis);
        let elt = self.parse_type();
        Expr::Ellipsis(Box::new(Ellipsis {
            node_id: AstNodeId::INVALID,
            ellipsis: pos,
            elt: Some(elt),
        }))
    }

    /// parse_param_decl parses a single parameter declaration (Go's local
    /// `field` struct is ported as [`ParamField`]).
    ///
    /// (Go's `TODO(rfindley)` comment about the error message for type
    /// parameter lists is kept verbatim below.)
    fn parse_param_decl(&mut self, name: Option<Ident>, type_sets_ok: bool) -> ParamField {
        // TODO(rFindley) refactor to be more similar to paramDeclOrNil in the
        // syntax package
        let ptok = self.tok;
        if name.is_some() {
            self.tok = Token::Ident; // force token.IDENT case in switch below
        } else if type_sets_ok && self.tok == Token::Tilde {
            // "~" ...
            return ParamField {
                commands: self.take_leading_commands(self.pos),
                name: None,
                typ: Some(self.embedded_elem(None)),
            };
        }

        let mut f = ParamField {
            commands: self.take_leading_commands(self.pos),
            name: None,
            typ: None,
        };
        match self.tok {
            Token::Ident => {
                // name
                if name.is_some() {
                    f.name = name;
                    self.tok = ptok;
                } else {
                    f.name = Some(self.parse_ident());
                }
                match self.tok {
                    Token::Ident
                    | Token::Mul
                    | Token::Arrow
                    | Token::Func
                    | Token::Chan
                    | Token::Map
                    | Token::Struct
                    | Token::Interface
                    | Token::LParen => {
                        // name type
                        f.typ = Some(self.parse_type());
                    }
                    Token::LBrack => {
                        // name "[" type1, ..., typeN "]" or name "[" n "]" type
                        let (name2, typ2) =
                            self.parse_array_field_or_type_instance(f.name.clone().unwrap());
                        f.name = name2;
                        f.typ = Some(typ2);
                    }
                    Token::Ellipsis => {
                        // name "..." type
                        f.typ = Some(self.parse_dots_type());
                        return f; // don't allow ...type "|" ...
                    }
                    Token::Period => {
                        // name "." ...
                        let name3 = f.name.take().unwrap();
                        f.typ = Some(self.parse_qualified_ident(Some(name3)));
                        f.name = None;
                    }
                    Token::Tilde => {
                        if type_sets_ok {
                            f.typ = Some(self.embedded_elem(None));
                            return f;
                        }
                    }
                    Token::Or if type_sets_ok => {
                        // name "|" typeset
                        let name4 = f.name.take().unwrap();
                        f.typ = Some(self.embedded_elem(Some(Expr::Ident(name4))));
                        f.name = None;
                        return f;
                    }
                    _ => {}
                }
            }
            Token::Mul
            | Token::Arrow
            | Token::Func
            | Token::LBrack
            | Token::Chan
            | Token::Map
            | Token::Struct
            | Token::Interface
            | Token::LParen => {
                // type
                f.typ = Some(self.parse_type());
            }
            Token::Ellipsis => {
                // "..." type
                // (always accepted)
                f.typ = Some(self.parse_dots_type());
                return f; // don't allow ...type "|" ...
            }
            _ => {
                // TODO(rfindley): this is incorrect in the case of type
                // parameter lists (should be "']'" in that case)
                self.error_expected(self.pos, "')'");
                self.advance(expr_end);
            }
        }

        // [name] type "|"
        if type_sets_ok && self.tok == Token::Or && f.typ.is_some() {
            f.typ = Some(self.embedded_elem(f.typ));
        }

        f
    }

    fn parse_parameter_list(
        &mut self,
        name0: Option<Ident>,
        typ0: Option<Expr>,
        closing: Token,
        dddok: bool,
    ) -> Vec<Field> {
        // Type parameters are the only parameter list closed by ']'.
        let tparams = closing == Token::RBrack;

        let mut name0 = name0;
        let mut typ0 = typ0;

        let pos0 = if let Some(name0) = &name0 {
            name0.pos()
        } else if let Some(typ0) = &typ0 {
            typ0.pos()
        } else {
            self.pos
        };

        // Note: The code below matches the corresponding code in the syntax
        //       parser closely. Changes must be reflected in either parser.
        //       For the code to match, we use the local []field list that
        //       corresponds to []syntax.Field. At the end, the list must be
        //       converted into an []*ast.Field.

        let mut list: Vec<ParamField> = Vec::new();
        let mut named = 0; // number of parameters that have an explicit name and type
        let mut typed = 0; // number of parameters that have an explicit type

        while name0.is_some() || (self.tok != closing && self.tok != Token::EOF) {
            let par = if typ0.is_some() {
                if tparams {
                    typ0 = Some(self.embedded_elem(typ0));
                }
                ParamField {
                    commands: self.take_leading_commands(self.pos),
                    name: name0,
                    typ: typ0,
                }
            } else {
                self.parse_param_decl(name0, tparams)
            };
            name0 = None; // 1st name was consumed if present
            typ0 = None; // 1st typ was consumed if present
            if par.name.is_some() || par.typ.is_some() {
                if par.name.is_some() && par.typ.is_some() {
                    named += 1;
                }
                if par.typ.is_some() {
                    typed += 1;
                }
                list.push(par);
            }
            if !self.at_comma("parameter list", closing) {
                break;
            }
            self.next();
        }

        if list.is_empty() {
            return Vec::new(); // not uncommon
        }

        // `keys` records, per parameter, the index of the parameter whose
        // type it shares; it drives the grouping into ast.Fields below (see
        // there). Without a distribution pass (named == len(list)) every
        // parameter keeps its own type, i.e. its own index.
        let mut keys: Vec<usize> = (0..list.len()).collect();

        // distribute parameter types (len(list) > 0)
        if named == 0 {
            // all unnamed => found names are type names
            for par in &mut list {
                if let Some(name) = par.name.take() {
                    par.typ = Some(Expr::Ident(name));
                }
            }
            if tparams {
                // This is the same error handling as below, adjusted for type
                // parameters only. See comment below for details.
                // (go.dev/issue/64534)
                let (err_pos, msg) = if named == typed
                /* same as typed == 0 */
                {
                    (
                        self.pos,
                        "missing type constraint".to_string(), // position error at closing ]
                    )
                } else {
                    let mut msg = "missing type parameter name".to_string();
                    if list.len() == 1 {
                        msg.push_str(" or invalid array length");
                    }
                    (pos0, msg)
                };
                self.error(err_pos, msg);
            }
        } else if named != list.len() {
            // some named or we're in a type parameter list => all must be named
            let mut err_pos: Option<Pos> = None; // left-most error position (or invalid)
            let mut typ: Option<Expr> = None; // current type (from right to left)
            let mut typ_src = 0; // index of the parameter `typ` was taken from
            for i in (0..list.len()).rev() {
                if list[i].typ.is_some() {
                    typ = list[i].typ.clone();
                    typ_src = i;
                    if list[i].name.is_none() {
                        let npos = typ.as_ref().unwrap().pos();
                        err_pos = Some(npos);
                        let mut n = new_ident("_");
                        n.name_pos = npos; // correct position
                        list[i].name = Some(n);
                    }
                } else if let Some(t) = &typ {
                    keys[i] = typ_src;
                    list[i].typ = Some(t.clone());
                } else {
                    // list[i].typ == nil && typ == nil => we only have a name
                    let epos = list[i].name.as_ref().unwrap().pos();
                    err_pos = Some(epos);
                    keys[i] = i;
                    list[i].typ = Some(Expr::BadExpr(BadExpr {
                        node_id: AstNodeId::INVALID,
                        from: epos,
                        to: self.pos,
                    }));
                }
            }
            if let Some(err_pos) = err_pos {
                // Not all parameters are named because named != len(list).
                // If named == typed, there must be parameters that have no types.
                // They must be at the end of the parameter list, otherwise types
                // would have been filled in by the right-to-left sweep above and
                // there would be no error.
                // If tparams is set, the parameter list is a type parameter list.
                let (err_pos, msg) = if named == typed {
                    let msg = if tparams {
                        "missing type constraint"
                    } else {
                        "missing parameter type"
                    };
                    // position error at closing token ) or ]
                    (self.pos, msg.to_string())
                } else {
                    if tparams {
                        let mut msg = "missing type parameter name".to_string();
                        // go.dev/issue/60812
                        if list.len() == 1 {
                            msg.push_str(" or invalid array length");
                        }
                        (err_pos, msg)
                    } else {
                        (err_pos, "missing parameter name".to_string())
                    }
                };
                self.error(err_pos, msg);
            }
        }

        // check use of ...
        // (only report the first occurrence; Go's `first` flag)
        let nlist = list.len();
        let mut first = true;
        for (i, f) in list.iter_mut().enumerate() {
            let ellipsis_ellipsis = match &f.typ {
                Some(Expr::Ellipsis(t)) => Some((t.ellipsis, t.pos(), t.end())),
                _ => None,
            };
            if let Some((ellipsis_pos, from, to)) = ellipsis_ellipsis
                && (!dddok || i + 1 < nlist)
            {
                if first {
                    first = false;
                    if dddok {
                        self.error(
                            ellipsis_pos,
                            "can only use ... with final parameter".to_string(),
                        );
                    } else {
                        self.error(ellipsis_pos, "invalid use of ...".to_string());
                    }
                }
                // Use T instead of invalid ...T.
                f.typ = Some(Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from,
                    to,
                }));
            }
        }

        // Convert list to []*ast.Field.
        // If list contains types only, each type gets its own ast.Field.
        if named == 0 {
            // parameter list consists of types only
            let mut params = Vec::new();
            for par in &list {
                assert(par.typ.is_some(), "nil type in unnamed parameter list");
                params.push(Field {
                    node_id: AstNodeId::INVALID,
                    commands: par.commands.clone(),
                    names: Vec::new(),
                    typ: par.typ.clone(),
                    tag: None,
                });
            }
            return params;
        }

        // If the parameter list consists of named parameters with types,
        // collect all names with the same types into a single ast.Field.
        // (Go groups parameters by pointer equality of their type nodes;
        // nodes are cloned by value in this port, so the grouping is
        // reproduced through the `keys` recorded above: a parameter whose
        // type was filled in from the right-to-left sweep shares the key of
        // the parameter it was copied from.)
        let mut params: Vec<Field> = Vec::new();
        let mut names: Vec<Ident> = Vec::new();
        let mut group_key: Option<usize> = None;
        for (i, par) in list.iter().enumerate() {
            if let Some(k) = group_key {
                if keys[i] != k {
                    let typ = list[k].typ.clone();
                    assert(typ.is_some(), "nil type in named parameter list");
                    params.push(Field {
                        node_id: AstNodeId::INVALID,
                        commands: list[k].commands.clone(),
                        names: std::mem::take(&mut names),
                        typ,
                        tag: None,
                    });
                    group_key = Some(keys[i]);
                }
            } else {
                group_key = Some(keys[i]);
            }
            names.push(par.name.clone().expect("name in named parameter list"));
        }
        if let Some(k) = group_key {
            let typ = list[k].typ.clone();
            assert(typ.is_some(), "nil type in named parameter list");
            params.push(Field {
                node_id: AstNodeId::INVALID,
                commands: list[k].commands.clone(),
                names,
                typ,
                tag: None,
            });
        }
        params
    }

    fn parse_type_parameters(&mut self) -> Option<FieldList> {
        let lbrack = self.expect(Token::LBrack);
        let mut list: Vec<Field> = Vec::new();
        if self.tok != Token::RBrack {
            list = self.parse_parameter_list(None, None, Token::RBrack, false);
        }
        let rbrack = self.expect(Token::RBrack);

        if list.is_empty() {
            self.error(rbrack, "empty type parameter list".to_string());
            return None; // avoid follow-on errors
        }

        Some(FieldList {
            node_id: AstNodeId::INVALID,
            opening: lbrack,
            list,
            closing: rbrack,
        })
    }

    fn parse_parameters(&mut self, result: bool) -> Option<FieldList> {
        if !result || self.tok == Token::LParen {
            let lparen = self.expect(Token::LParen);
            let mut list: Vec<Field> = Vec::new();
            if self.tok != Token::RParen {
                list = self.parse_parameter_list(None, None, Token::RParen, !result);
            }
            let rparen = self.expect(Token::RParen);
            return Some(FieldList {
                node_id: AstNodeId::INVALID,
                opening: lparen,
                list,
                closing: rparen,
            });
        }

        let commands = self.take_leading_commands(self.pos);
        if let Some(typ) = self.try_ident_or_type() {
            let list = vec![Field {
                node_id: AstNodeId::INVALID,
                commands,
                names: Vec::new(),
                typ: Some(typ),
                tag: None,
            }];
            return Some(FieldList {
                node_id: AstNodeId::INVALID,
                opening: NO_POS,
                list,
                closing: NO_POS,
            });
        }

        None
    }

    fn parse_func_type(&mut self) -> FuncType {
        let pos = self.expect(Token::Func);
        // accept type parameters for more tolerant parsing but complain
        if self.tok == Token::LBrack {
            let tparams = self.parse_type_parameters();
            if let Some(tparams) = tparams {
                self.error(
                    tparams.opening,
                    "function type must have no type parameters".to_string(),
                );
            }
        }
        let params = self.parse_parameters(false);
        let results = self.parse_parameters(true);

        FuncType {
            node_id: AstNodeId::INVALID,
            func: pos,
            type_params: None,
            params,
            results,
        }
    }

    fn parse_method_spec(&mut self) -> Field {
        let commands = self.take_leading_commands(self.pos);
        let mut idents: Vec<Ident> = Vec::new();
        let typ: Expr = match self.parse_type_name(None) {
            Expr::Ident(ident) => match self.tok {
                Token::LBrack => {
                    // generic method or embedded instantiated type
                    let lbrack = self.pos;
                    self.next();
                    self.expr_lev += 1;
                    let x = self.parse_expr();
                    self.expr_lev -= 1;
                    if let Expr::Ident(name0) = &x {
                        if self.tok != Token::Comma && self.tok != Token::RBrack {
                            // generic method m[T any]
                            //
                            // Interface methods do not have type parameters. We parse
                            // them for a better error message and improved error
                            // recovery.
                            let _ = self.parse_parameter_list(
                                Some(name0.clone()),
                                None,
                                Token::RBrack,
                                false,
                            );
                            let _ = self.expect(Token::RBrack);
                            self.error(
                                lbrack,
                                "interface method must have no type parameters".to_string(),
                            );

                            // TODO(rfindley) refactor to share code with parseFuncType.
                            let params = self.parse_parameters(false);
                            let results = self.parse_parameters(true);
                            idents = vec![ident];
                            Expr::FuncType(FuncType {
                                node_id: AstNodeId::INVALID,
                                func: NO_POS,
                                type_params: None,
                                params,
                                results,
                            })
                        } else {
                            self.parse_embedded_instantiated_type(ident, lbrack, x)
                        }
                    } else {
                        self.parse_embedded_instantiated_type(ident, lbrack, x)
                    }
                }
                Token::LParen => {
                    // ordinary method
                    // TODO(rfindley) refactor to share code with parseFuncType.
                    let params = self.parse_parameters(false);
                    let results = self.parse_parameters(true);
                    idents = vec![ident];
                    Expr::FuncType(FuncType {
                        node_id: AstNodeId::INVALID,
                        func: NO_POS,
                        type_params: None,
                        params,
                        results,
                    })
                }
                _ => {
                    // embedded type
                    Expr::Ident(ident)
                }
            },
            x => {
                // embedded, possibly instantiated type
                if self.tok == Token::LBrack {
                    // embedded instantiated interface
                    self.parse_type_instance(x)
                } else {
                    x
                }
            }
        };

        // (Go adds the comment at the callsite: the field below may be
        // joined with additional type specs using '|'. The TODO(rfindley)
        // comments about comment handling are obsolete in this port.)
        Field {
            node_id: AstNodeId::INVALID,
            commands,
            names: idents,
            typ: Some(typ),
            tag: None,
        }
    }

    fn parse_embedded_instantiated_type(&mut self, ident: Ident, lbrack: Pos, x: Expr) -> Expr {
        // embedded instantiated type
        // TODO(rfindley) should resolve all identifiers in x.
        let mut list = vec![x];
        if self.at_comma("type argument list", Token::RBrack) {
            self.expr_lev += 1;
            self.next();
            while self.tok != Token::RBrack && self.tok != Token::EOF {
                list.push(self.parse_type());
                if !self.at_comma("type argument list", Token::RBrack) {
                    break;
                }
                self.next();
            }
            self.expr_lev -= 1;
        }
        let rbrack = self.expect_closing(Token::RBrack, "type argument list");
        pack_index_expr(Expr::Ident(ident), lbrack, list, rbrack)
    }

    fn embedded_elem(&mut self, x: Option<Expr>) -> Expr {
        let mut x = match x {
            Some(x) => x,
            None => self.embedded_term(),
        };
        while self.tok == Token::Or {
            let op_pos = self.pos;
            let op = Token::Or;
            self.next();
            let y = self.embedded_term();
            x = Expr::BinaryExpr(Box::new(BinaryExpr {
                node_id: AstNodeId::INVALID,
                x,
                op_pos,
                op,
                y,
            }));
        }
        x
    }

    fn embedded_term(&mut self) -> Expr {
        if self.tok == Token::Tilde {
            let op_pos = self.pos;
            let op = Token::Tilde;
            self.next();
            let x = self.parse_type();
            return Expr::UnaryExpr(Box::new(UnaryExpr {
                node_id: AstNodeId::INVALID,
                op_pos,
                op,
                x,
            }));
        }

        let t = self.try_ident_or_type();
        match t {
            Some(t) => t,
            None => {
                let pos = self.pos;
                self.error_expected(pos, "~ term or type");
                self.advance(expr_end);
                Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from: pos,
                    to: self.pos,
                })
            }
        }
    }

    fn parse_interface_type(&mut self) -> Expr {
        let pos = self.expect(Token::Interface);
        let lbrace = self.expect(Token::LBrace);

        let mut list: Vec<Field> = Vec::new();

        loop {
            match self.tok {
                Token::Ident => {
                    let mut f = self.parse_method_spec();
                    if f.names.is_empty() {
                        let t = f.typ.take().unwrap();
                        f.typ = Some(self.embedded_elem(Some(t)));
                    }
                    self.expect_semi();
                    list.push(f);
                }
                Token::Tilde => {
                    let commands = self.take_leading_commands(self.pos);
                    let typ = self.embedded_elem(None);
                    self.expect_semi();
                    list.push(Field {
                        node_id: AstNodeId::INVALID,
                        commands,
                        names: Vec::new(),
                        typ: Some(typ),
                        tag: None,
                    });
                }
                _ => {
                    let t = self.try_ident_or_type();
                    match t {
                        Some(t) => {
                            let commands = self.take_leading_commands(t.pos());
                            let typ = self.embedded_elem(Some(t));
                            self.expect_semi();
                            list.push(Field {
                                node_id: AstNodeId::INVALID,
                                commands,
                                names: Vec::new(),
                                typ: Some(typ),
                                tag: None,
                            });
                        }
                        None => break,
                    }
                }
            }
        }

        // TODO(rfindley): the error produced here could be improved, since we could
        // accept an identifier, 'type', or a '}' at this point.
        let rbrace = self.expect(Token::RBrace);

        Expr::InterfaceType(InterfaceType {
            node_id: AstNodeId::INVALID,
            interface: pos,
            methods: Some(FieldList {
                node_id: AstNodeId::INVALID,
                opening: lbrace,
                list,
                closing: rbrace,
            }),
            incomplete: false,
        })
    }

    fn parse_map_type(&mut self) -> Expr {
        let pos = self.expect(Token::Map);
        self.expect(Token::LBrack);
        let key = self.parse_type();
        self.expect(Token::RBrack);
        let value = self.parse_type();

        Expr::MapType(Box::new(MapType {
            node_id: AstNodeId::INVALID,
            map: pos,
            key,
            value,
        }))
    }

    fn parse_chan_type(&mut self) -> Expr {
        let pos = self.pos;
        let mut dir = ChanDir::SEND | ChanDir::RECV;
        let mut arrow = NO_POS;
        if self.tok == Token::Chan {
            self.next();
            if self.tok == Token::Arrow {
                arrow = self.pos;
                self.next();
                dir = ChanDir::SEND;
            }
        } else {
            arrow = self.expect(Token::Arrow);
            self.expect(Token::Chan);
            dir = ChanDir::RECV;
        }
        let value = self.parse_type();

        Expr::ChanType(Box::new(ChanType {
            node_id: AstNodeId::INVALID,
            begin: pos,
            arrow,
            dir,
            value,
        }))
    }

    fn parse_type_instance(&mut self, typ: Expr) -> Expr {
        let opening = self.expect(Token::LBrack);
        self.expr_lev += 1;
        let mut list: Vec<Expr> = Vec::new();
        while self.tok != Token::RBrack && self.tok != Token::EOF {
            list.push(self.parse_type());
            if !self.at_comma("type argument list", Token::RBrack) {
                break;
            }
            self.next();
        }
        self.expr_lev -= 1;

        let closing = self.expect_closing(Token::RBrack, "type argument list");

        if list.is_empty() {
            self.error_expected(closing, "type argument list");
            return Expr::IndexExpr(Box::new(IndexExpr {
                node_id: AstNodeId::INVALID,
                x: typ,
                lbrack: opening,
                index: Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from: opening + 1,
                    to: closing,
                }),
                rbrack: closing,
            }));
        }

        pack_index_expr(typ, opening, list, closing)
    }

    fn try_ident_or_type(&mut self) -> Option<Expr> {
        let _nest = inc_nest_lev(self);

        match self.tok {
            Token::Ident => {
                let mut typ = self.parse_type_name(None);
                if self.tok == Token::LBrack {
                    typ = self.parse_type_instance(typ);
                }
                Some(typ)
            }
            Token::LBrack => {
                let lbrack = self.expect(Token::LBrack);
                Some(self.parse_array_type(lbrack, None))
            }
            Token::Struct => Some(self.parse_struct_type()),
            Token::Mul => Some(self.parse_pointer_type()),
            Token::Func => Some(Expr::FuncType(self.parse_func_type())),
            Token::Interface => Some(self.parse_interface_type()),
            Token::Map => Some(self.parse_map_type()),
            Token::Chan | Token::Arrow => Some(self.parse_chan_type()),
            Token::LParen => {
                let lparen = self.pos;
                self.next();
                let typ = self.parse_type();
                let rparen = self.expect(Token::RParen);
                Some(Expr::ParenExpr(Box::new(ParenExpr {
                    node_id: AstNodeId::INVALID,
                    lparen,
                    x: typ,
                    rparen,
                })))
            }
            _ => None, // no type found
        }
    }
}

/// Go's local `field` struct (name, typ), used while parsing parameter
/// lists; Go's `doc`/`comment` fields are not ported.
struct ParamField {
    commands: Vec<CommentCommand>,
    name: Option<Ident>,
    typ: Option<Expr>,
}

// ----------------------------------------------------------------------------
// Blocks

impl<'src> Parser<'src> {
    fn parse_stmt_list(&mut self) -> Vec<Stmt> {
        let mut list: Vec<Stmt> = Vec::new();
        while !matches!(
            self.tok,
            Token::Case | Token::Default | Token::RBrace | Token::EOF
        ) {
            list.push(self.parse_stmt());
        }
        list
    }

    fn parse_body(&mut self) -> BlockStmt {
        let lbrace = self.expect(Token::LBrace);
        let list = self.parse_stmt_list();
        let rbrace = self.expect2(Token::RBrace);

        BlockStmt {
            node_id: AstNodeId::INVALID,
            lbrace,
            list,
            rbrace,
        }
    }

    fn parse_block_stmt(&mut self) -> BlockStmt {
        let lbrace = self.expect(Token::LBrace);
        let list = self.parse_stmt_list();
        let rbrace = self.expect2(Token::RBrace);

        BlockStmt {
            node_id: AstNodeId::INVALID,
            lbrace,
            list,
            rbrace,
        }
    }

    // ------------------------------------------------------------------------
    // Expressions

    fn parse_func_type_or_lit(&mut self) -> Expr {
        let typ = self.parse_func_type();
        if self.tok != Token::LBrace {
            // function type only
            return Expr::FuncType(typ);
        }

        self.expr_lev += 1;
        let body = self.parse_body();
        self.expr_lev -= 1;

        Expr::FuncLit(FuncLit {
            node_id: AstNodeId::INVALID,
            typ: Box::new(typ),
            body: Box::new(body),
        })
    }

    /// parse_operand may return an expression or a raw type (incl. array
    /// types of the form [...]T). Callers must verify the result.
    fn parse_operand(&mut self) -> Expr {
        match self.tok {
            Token::Ident => return Expr::Ident(self.parse_ident()),

            Token::Int | Token::Float | Token::Imag | Token::Char | Token::String => {
                let x = BasicLit {
                    node_id: AstNodeId::INVALID,
                    value_pos: self.pos,
                    value_end: self.end(),
                    kind: self.tok,
                    value: self.lit.clone(),
                };
                self.next();
                return Expr::BasicLit(x);
            }

            Token::LBrace => return self.parse_literal_value(None),

            Token::LParen => {
                let lparen = self.pos;
                self.next();
                self.expr_lev += 1;
                let x = self.parse_rhs(); // types may be parenthesized: (some type)
                self.expr_lev -= 1;
                let rparen = self.expect(Token::RParen);
                return Expr::ParenExpr(Box::new(ParenExpr {
                    node_id: AstNodeId::INVALID,
                    lparen,
                    x,
                    rparen,
                }));
            }

            Token::Func => return self.parse_func_type_or_lit(),

            _ => {}
        }

        if let Some(typ) = self.try_ident_or_type() {
            // could be type for composite literal or conversion
            assert!(!matches!(typ, Expr::Ident(_)), "type cannot be identifier");
            return typ;
        }

        // we have an error
        let pos = self.pos;
        self.error_expected(pos, "operand");
        self.advance(stmt_start);
        Expr::BadExpr(BadExpr {
            node_id: AstNodeId::INVALID,
            from: pos,
            to: self.pos,
        })
    }

    fn parse_selector(&mut self, x: Expr) -> Expr {
        let sel = self.parse_ident();
        Expr::SelectorExpr(Box::new(SelectorExpr {
            node_id: AstNodeId::INVALID,
            x,
            sel,
        }))
    }

    fn parse_type_assertion(&mut self, x: Expr) -> Expr {
        let lparen = self.expect(Token::LParen);
        let typ;
        if self.tok == Token::Type {
            // type switch: typ == nil
            typ = None;
            self.next();
        } else {
            typ = Some(self.parse_type());
        }
        let rparen = self.expect(Token::RParen);

        Expr::TypeAssertExpr(Box::new(TypeAssertExpr {
            node_id: AstNodeId::INVALID,
            x,
            lparen,
            typ,
            rparen,
        }))
    }

    fn parse_index_or_slice_or_instance(&mut self, x: Expr) -> Expr {
        let lbrack = self.expect(Token::LBrack);
        if self.tok == Token::RBrack {
            // empty index, slice or index expressions are not permitted;
            // accept them for parsing tolerance, but complain
            self.error_expected(self.pos, "operand");
            let rbrack = self.pos;
            self.next();
            return Expr::IndexExpr(Box::new(IndexExpr {
                node_id: AstNodeId::INVALID,
                x,
                lbrack,
                index: Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from: rbrack,
                    to: rbrack,
                }),
                rbrack,
            }));
        }
        self.expr_lev += 1;

        const N: usize = 3; // change the 3 to 2 to disable 3-index slices
        let mut args: Vec<Expr> = Vec::new();
        let mut index: [Option<Expr>; N] = [None, None, None];
        let mut colons = [NO_POS; N - 1];
        if self.tok != Token::Colon {
            // We can't know if we have an index expression or a type instantiation;
            // so even if we see a (named) type we are not going to be in type context.
            index[0] = Some(self.parse_rhs());
        }
        let mut ncolons = 0;
        match self.tok {
            Token::Colon => {
                // slice expression
                while self.tok == Token::Colon && ncolons < colons.len() {
                    colons[ncolons] = self.pos;
                    ncolons += 1;
                    self.next();
                    if !matches!(self.tok, Token::Colon | Token::RBrack | Token::EOF) {
                        index[ncolons] = Some(self.parse_rhs());
                    }
                }
            }
            Token::Comma => {
                // instance expression
                args.push(index[0].take().unwrap());
                while self.tok == Token::Comma {
                    self.next();
                    if !matches!(self.tok, Token::RBrack | Token::EOF) {
                        args.push(self.parse_type());
                    }
                }
            }
            _ => {}
        }

        self.expr_lev -= 1;
        let rbrack = self.expect(Token::RBrack);

        if ncolons > 0 {
            // slice expression
            let slice3 = ncolons == 2;
            if slice3 {
                // Check presence of middle and final index here rather than during
                // type-checking to prevent erroneous programs from passing through
                // gofmt (was go.dev/issue/7305).
                if index[1].is_none() {
                    self.error(
                        colons[0],
                        "middle index required in 3-index slice".to_string(),
                    );
                    index[1] = Some(Expr::BadExpr(BadExpr {
                        node_id: AstNodeId::INVALID,
                        from: colons[0] + 1,
                        to: colons[1],
                    }));
                }
                if index[2].is_none() {
                    self.error(
                        colons[1],
                        "final index required in 3-index slice".to_string(),
                    );
                    index[2] = Some(Expr::BadExpr(BadExpr {
                        node_id: AstNodeId::INVALID,
                        from: colons[1] + 1,
                        to: rbrack,
                    }));
                }
            }
            return Expr::SliceExpr(Box::new(SliceExpr {
                node_id: AstNodeId::INVALID,
                x,
                lbrack,
                low: index[0].take(),
                high: index[1].take(),
                max: index[2].take(),
                slice3,
                rbrack,
            }));
        }

        if args.is_empty() {
            // index expression
            return Expr::IndexExpr(Box::new(IndexExpr {
                node_id: AstNodeId::INVALID,
                x,
                lbrack,
                index: index[0].take().unwrap(),
                rbrack,
            }));
        }

        // instance expression
        pack_index_expr(x, lbrack, args, rbrack)
    }

    fn parse_call_or_conversion(&mut self, fun: Expr) -> Expr {
        let lparen = self.expect(Token::LParen);
        self.expr_lev += 1;
        let mut list: Vec<Expr> = Vec::new();
        let mut ellipsis = NO_POS;
        while self.tok != Token::RParen && self.tok != Token::EOF && !ellipsis.is_valid() {
            list.push(self.parse_rhs()); // builtins may expect a type: make(some type, ...)
            if self.tok == Token::Ellipsis {
                ellipsis = self.pos;
                self.next();
            }
            if !self.at_comma("argument list", Token::RParen) {
                break;
            }
            self.next();
        }
        self.expr_lev -= 1;
        let rparen = self.expect_closing(Token::RParen, "argument list");

        Expr::CallExpr(Box::new(CallExpr {
            node_id: AstNodeId::INVALID,
            fun,
            lparen,
            args: list,
            ellipsis,
            rparen,
        }))
    }

    fn parse_element(&mut self) -> Expr {
        let x = self.parse_expr();
        if self.tok == Token::Colon {
            let colon = self.pos;
            self.next();
            let value = self.parse_expr();
            return Expr::KeyValueExpr(Box::new(KeyValueExpr {
                node_id: AstNodeId::INVALID,
                key: x,
                colon,
                value,
            }));
        }
        x
    }

    fn parse_element_list(&mut self) -> Vec<Expr> {
        let mut list: Vec<Expr> = Vec::new();
        while self.tok != Token::RBrace && self.tok != Token::EOF {
            list.push(self.parse_element());
            if !self.at_comma("composite literal", Token::RBrace) {
                break;
            }
            self.next();
        }
        list
    }

    fn parse_literal_value(&mut self, typ: Option<Expr>) -> Expr {
        let _nest = inc_nest_lev(self);

        let lbrace = self.expect(Token::LBrace);
        let mut elts: Vec<Expr> = Vec::new();
        self.expr_lev += 1;
        if self.tok != Token::RBrace {
            elts = self.parse_element_list();
        }
        self.expr_lev -= 1;
        let rbrace = self.expect_closing(Token::RBrace, "composite literal");
        Expr::CompositeLit(Box::new(CompositeLit {
            node_id: AstNodeId::INVALID,
            typ,
            lbrace,
            elts,
            rbrace,
            incomplete: false,
        }))
    }

    fn parse_primary_expr(&mut self, x: Option<Expr>) -> Expr {
        let mut x = match x {
            Some(x) => x,
            None => self.parse_operand(),
        };
        // We track the nesting here rather than at the entry for the function,
        // since it can iteratively produce a nested output, and we want to
        // limit how deep a structure we generate.
        let nest = NestAccum {
            nest_lev: self.nest_lev.clone(),
            n: Cell::new(0),
        };
        loop {
            nest.inc(self);
            match self.tok {
                Token::Period => {
                    self.next();
                    match self.tok {
                        Token::Ident => x = self.parse_selector(x),
                        Token::LParen => x = self.parse_type_assertion(x),
                        _ => {
                            let pos = self.pos;
                            self.error_expected(pos, "selector or type assertion");
                            // TODO(rFindley) The check for token.RBRACE below is a
                            //                targeted fix to error recovery sufficient
                            //                to make the x/tools tests to pass with the
                            //                new parsing logic introduced for type
                            //                parameters. Remove this once error recovery
                            //                has been more generally reconsidered.
                            if self.tok != Token::RBrace {
                                self.next(); // make progress
                            }
                            let sel = Ident {
                                node_id: AstNodeId::INVALID,
                                name_pos: pos,
                                name: "_".to_string(),
                            };
                            x = Expr::SelectorExpr(Box::new(SelectorExpr {
                                node_id: AstNodeId::INVALID,
                                x,
                                sel,
                            }));
                        }
                    }
                }
                Token::LBrack => x = self.parse_index_or_slice_or_instance(x),
                Token::LParen => x = self.parse_call_or_conversion(x),
                Token::LBrace => {
                    // operand may have returned a parenthesized complit
                    // type; accept it but complain if we have a complit
                    let was_paren = matches!(x, Expr::ParenExpr(_));
                    let t = unparen(x.clone());
                    // determine if '{' belongs to a composite literal or a block statement
                    // (Go inspects the unparenthesized type but returns the
                    // original `x` on the early exits below)
                    match &t {
                        Expr::BadExpr(_) | Expr::Ident(_) | Expr::SelectorExpr(_) => {
                            if self.expr_lev < 0 {
                                return x;
                            }
                            // x is possibly a composite literal type
                        }
                        Expr::IndexExpr(_) | Expr::IndexListExpr(_) => {
                            if self.expr_lev < 0 {
                                return x;
                            }
                            // x is possibly a composite literal type
                        }
                        Expr::ArrayType(_) | Expr::StructType(_) | Expr::MapType(_) => {
                            // x is a composite literal type
                        }
                        _ => return x,
                    }
                    if was_paren {
                        // Go: t != x (x was parenthesized)
                        self.error(
                            t.pos(),
                            "cannot parenthesize type in composite literal".to_string(),
                        );
                        // already progressed, no need to advance
                    }
                    x = self.parse_literal_value(Some(t));
                }
                _ => return x,
            }
        }
    }

    fn parse_unary_expr(&mut self) -> Expr {
        let _nest = inc_nest_lev(self);

        match self.tok {
            Token::Add | Token::Sub | Token::Not | Token::XOr | Token::And | Token::Tilde => {
                let (pos, op) = (self.pos, self.tok);
                self.next();
                let x = self.parse_unary_expr();
                Expr::UnaryExpr(Box::new(UnaryExpr {
                    node_id: AstNodeId::INVALID,
                    op_pos: pos,
                    op,
                    x,
                }))
            }

            Token::Arrow => {
                // channel type or receive expression
                let arrow = self.pos;
                self.next();

                // If the next token is token.CHAN we still don't know if it
                // is a channel type or a receive operation - we only know
                // once we have found the end of the unary expression. There
                // are two cases:
                //
                //   <- type  => (<-type) must be channel type
                //   <- expr  => <-(expr) is a receive from an expression
                //
                // In the first case, the arrow must be re-associated with
                // the channel type parsed already:
                //
                //   <- (chan type)    =>  (<-chan type)
                //   <- (chan<- type)  =>  (<-chan (<-type))

                let x = self.parse_unary_expr();

                // determine which case we have
                match x {
                    Expr::ChanType(ct) => {
                        // (<-type)

                        // re-associate position info and <-
                        // (Go re-associates the arrow through the chain of
                        // nested channel types with a loop over shared
                        // pointers; with owned nodes this becomes a
                        // recursive descent along `ct.value`)
                        let mut ct = *ct;
                        let mut dir = ChanDir::SEND;
                        let mut arrow = arrow;
                        self.re_associate_arrow(&mut ct, &mut arrow, &mut dir);
                        if dir == ChanDir::SEND {
                            self.error_expected(arrow, "channel type");
                        }
                        Expr::ChanType(Box::new(ct))
                    }
                    // <-(expr)
                    x => Expr::UnaryExpr(Box::new(UnaryExpr {
                        node_id: AstNodeId::INVALID,
                        op_pos: arrow,
                        op: Token::Arrow,
                        x,
                    })),
                }
            }

            Token::Mul => {
                // pointer type or unary "*" expression
                let pos = self.pos;
                self.next();
                let x = self.parse_unary_expr();
                Expr::StarExpr(Box::new(StarExpr {
                    node_id: AstNodeId::INVALID,
                    star: pos,
                    x,
                }))
            }

            _ => self.parse_primary_expr(None),
        }
    }

    fn tok_prec(&self) -> (Token, i8) {
        let mut tok = self.tok;
        if self.in_rhs && tok == Token::Assign {
            tok = Token::Equal;
        }
        (tok, tok.get_precedence())
    }

    /// parse_binary_expr parses a (possibly) binary expression.
    /// If x is non-nil, it is used as the left operand.
    ///
    /// (Go's TODO(rfindley) comment about parseBinaryExpr having become
    /// overloaded is not ported.)
    fn parse_binary_expr(&mut self, x: Option<Expr>, prec1: i8) -> Expr {
        let mut x = match x {
            Some(x) => x,
            None => self.parse_unary_expr(),
        };
        // We track the nesting here rather than at the entry for the function,
        // since it can iteratively produce a nested output, and we want to
        // limit how deep a structure we generate.
        let nest = NestAccum {
            nest_lev: self.nest_lev.clone(),
            n: Cell::new(0),
        };
        loop {
            nest.inc(self);
            let (op, oprec) = self.tok_prec();
            if oprec < prec1 {
                return x;
            }
            let pos = self.expect(op);
            let y = self.parse_binary_expr(None, oprec + 1);
            x = Expr::BinaryExpr(Box::new(BinaryExpr {
                node_id: AstNodeId::INVALID,
                x,
                op_pos: pos,
                op,
                y,
            }));
        }
    }

    /// The result may be a type or even a raw type ([...]int).
    fn parse_expr(&mut self) -> Expr {
        self.parse_binary_expr(None, Token::LOWEST_PREC + 1)
    }

    pub(crate) fn parse_rhs(&mut self) -> Expr {
        let old = self.in_rhs;
        self.in_rhs = true;
        let x = self.parse_expr();
        self.in_rhs = old;
        x
    }

    /// Parses a comma-separated expression list.
    fn parse_expr_list(&mut self) -> Vec<Expr> {
        let mut list: Vec<Expr> = Vec::new();
        list.push(self.parse_expr());
        while self.tok == Token::Comma {
            self.next();
            list.push(self.parse_expr());
        }
        list
    }

    fn parse_list(&mut self, in_rhs: bool) -> Vec<Expr> {
        let old = self.in_rhs;
        self.in_rhs = in_rhs;
        let list = self.parse_expr_list();
        self.in_rhs = old;
        list
    }
}

/// inc_nest_lev increments the parser's nesting depth counter and returns a
/// guard that decrements it when dropped (adapts Go's `incNestLev`/
/// `decNestLev` pair). If the maximum nesting depth is exceeded, reports an
/// error and bails out like Go's `incNestLev` does.
fn inc_nest_lev(p: &mut Parser) -> NestGuard {
    p.nest_lev.set(p.nest_lev.get() + 1);
    if p.nest_lev.get() > MAX_NEST_LEV {
        p.error(p.pos, "exceeded max nesting depth".to_string());
        std::panic::panic_any(Bailout {
            pos: NO_POS,
            msg: String::new(),
        });
    }
    NestGuard {
        nest_lev: p.nest_lev.clone(),
    }
}

/// A set of tokens, used for error recovery.
///
/// (Go passes `map[token.Token]bool` token sets around; adapted to function
/// pointers.)
type TokenSet = fn(Token) -> bool;

/// stmtStart is the set of tokens that can start a statement.
fn stmt_start(tok: Token) -> bool {
    matches!(
        tok,
        Token::Break
            | Token::Const
            | Token::Continue
            | Token::Defer
            | Token::FallThrough
            | Token::For
            | Token::Go
            | Token::Goto
            | Token::If
            | Token::Return
            | Token::Select
            | Token::Switch
            | Token::Type
            | Token::Var
    )
}

/// declStart is the set of tokens that can start a declaration.
fn decl_start(tok: Token) -> bool {
    matches!(tok, Token::Import | Token::Const | Token::Type | Token::Var)
}

/// exprEnd is the set of tokens that can end an expression.
fn expr_end(tok: Token) -> bool {
    matches!(
        tok,
        Token::Comma
            | Token::Colon
            | Token::Semicolon
            | Token::RParen
            | Token::RBrack
            | Token::RBrace
    )
}

/// maxNestLev is the deepest we're willing to recurse during parsing.
///
/// (Go uses `1e5`, which is only reachable because goroutine stacks grow;
/// Rust threads have fixed-size stacks, so the guard is lowered to 1e4.
/// Deviation from Go: 1e5 -> 1e4. Deep tests run on dedicated large-stack
/// threads.)
const MAX_NEST_LEV: i32 = 10_000;

/// A guard that decrements the parser's nesting depth counter when dropped,
/// adapting Go's `decNestLev`.
struct NestGuard {
    nest_lev: Rc<Cell<i32>>,
}

impl Drop for NestGuard {
    fn drop(&mut self) {
        self.nest_lev.set(self.nest_lev.get() - 1);
    }
}

fn assert(cond: bool, msg: &str) {
    if !cond {
        panic!("go/parser internal error: {msg}");
    }
}

/// pack_index_expr returns an IndexExpr x[expr0] or IndexListExpr x[expr0, ...].
fn pack_index_expr(x: Expr, lbrack: Pos, exprs: Vec<Expr>, rbrack: Pos) -> Expr {
    match exprs.len() {
        0 => panic!("internal error: packIndexExpr with empty expr slice"),
        1 => Expr::IndexExpr(Box::new(IndexExpr {
            node_id: AstNodeId::INVALID,
            x,
            lbrack,
            index: exprs.into_iter().next().unwrap(),
            rbrack,
        })),
        _ => Expr::IndexListExpr(Box::new(IndexListExpr {
            node_id: AstNodeId::INVALID,
            x,
            lbrack,
            indices: exprs,
            rbrack,
        })),
    }
}

/// Adapts Go's `incNestLev`/`decNestLev` accounting for the iterative loops
/// in `parse_primary_expr` and `parse_binary_expr` (Go: `for n = 1; ; n++ {
/// incNestLev(p) ... }` with `defer func() { p.nestLev -= n }()`): the
/// accumulated count is undone when the accumulator is dropped, including on
/// unwinding panics.
struct NestAccum {
    nest_lev: Rc<Cell<i32>>,
    n: Cell<i32>,
}

impl NestAccum {
    /// Go's `incNestLev`: increments the nesting depth; reports an error and
    /// bails out (Go: panic) if the maximum nesting depth is exceeded.
    fn inc(&self, p: &Parser) {
        self.nest_lev.set(self.nest_lev.get() + 1);
        self.n.set(self.n.get() + 1);
        if self.nest_lev.get() > MAX_NEST_LEV {
            p.error(p.pos, "exceeded max nesting depth".to_string());
            std::panic::panic_any(Bailout {
                pos: NO_POS,
                msg: String::new(),
            });
        }
    }
}

impl Drop for NestAccum {
    fn drop(&mut self) {
        self.nest_lev.set(self.nest_lev.get() - self.n.get());
    }
}

/// Parsing modes for parse_simple_stmt (Go's `basic`, `labelOk`, `rangeOk`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SimpleStmtMode {
    Basic,
    LabelOk,
    RangeOk,
}

/// Go's anonymous `semi` struct in parseIfHeader: an (explicit or artificial)
/// semicolon, recorded for error reporting.
struct Semi {
    pos: Pos,
    lit: String, // ";" or "\n"
}

// ----------------------------------------------------------------------------
// Statements

impl<'src> Parser<'src> {
    /// parse_simple_stmt returns whether it parsed the assignment of a range
    /// clause (with mode == RangeOk). The returned statement is an assignment
    /// with a right-hand side that is a single unary expression of the form
    /// "range x". No guarantees are given for the left-hand side.
    fn parse_simple_stmt(&mut self, mode: SimpleStmtMode) -> (Stmt, bool) {
        let mut x = self.parse_list(false);

        match self.tok {
            Token::Define
            | Token::Assign
            | Token::AddAssign
            | Token::SubAssign
            | Token::MulAssign
            | Token::QuoAssign
            | Token::RemAssign
            | Token::AndAssign
            | Token::OrAssign
            | Token::XOrAssign
            | Token::ShlAssign
            | Token::ShrAssign
            | Token::AndNotAssign => {
                // assignment statement, possibly part of a range clause
                let (pos, tok) = (self.pos, self.tok);
                self.next();
                let (y, is_range) = if mode == SimpleStmtMode::RangeOk
                    && self.tok == Token::Range
                    && (tok == Token::Define || tok == Token::Assign)
                {
                    let pos = self.pos;
                    self.next();
                    let x = self.parse_rhs();
                    (
                        vec![Expr::UnaryExpr(Box::new(UnaryExpr {
                            node_id: AstNodeId::INVALID,
                            op_pos: pos,
                            op: Token::Range,
                            x,
                        }))],
                        true,
                    )
                } else {
                    (self.parse_list(true), false)
                };
                return (
                    Stmt::AssignStmt(AssignStmt {
                        node_id: AstNodeId::INVALID,
                        lhs: x,
                        tok_pos: pos,
                        tok,
                        rhs: y,
                    }),
                    is_range,
                );
            }
            _ => {}
        }

        if x.len() > 1 {
            let pos = x[0].pos();
            self.error_expected(pos, "1 expression");
            // continue with first expression
        }

        match self.tok {
            Token::Colon => {
                // labeled statement
                let colon = self.pos;
                self.next();
                if mode == SimpleStmtMode::LabelOk
                    && let Expr::Ident(label) = &x[0]
                {
                    // Go spec: The scope of a label is the body of the function
                    // in which it is declared and excludes the body of any nested
                    // function.
                    let label = label.clone();
                    let stmt = self.parse_stmt();
                    let s = LabeledStmt {
                        node_id: AstNodeId::INVALID,
                        label,
                        colon,
                        stmt,
                    };
                    return (Stmt::LabeledStmt(Box::new(s)), false);
                }

                // The label declaration typically starts at x[0].Pos(), but the label
                // declaration may be erroneous due to a token after that position (and
                // before the ':'). If SpuriousErrors is not set, the (only) error
                // reported for the line is the illegal label error instead of the token
                // before the ':' that caused the problem. Thus, use the (latest) colon
                // position for error reporting.
                self.error(colon, "illegal label declaration".to_string());
                let from = x[0].pos();
                return (
                    Stmt::BadStmt(BadStmt {
                        node_id: AstNodeId::INVALID,
                        from,
                        to: colon + 1,
                    }),
                    false,
                );
            }
            Token::Arrow => {
                // send statement
                let arrow = self.pos;
                self.next();
                let y = self.parse_rhs();
                let chan_ = x.remove(0);
                return (
                    Stmt::SendStmt(SendStmt {
                        node_id: AstNodeId::INVALID,
                        chan_,
                        arrow,
                        value: y,
                    }),
                    false,
                );
            }
            Token::Inc | Token::Dec => {
                // increment or decrement
                let (tok_pos, tok) = (self.pos, self.tok);
                self.next();
                let x0 = x.remove(0);
                return (
                    Stmt::IncDecStmt(IncDecStmt {
                        node_id: AstNodeId::INVALID,
                        x: x0,
                        tok_pos,
                        tok,
                    }),
                    false,
                );
            }
            _ => {}
        }

        // expression
        let x0 = x.remove(0);
        (
            Stmt::ExprStmt(ExprStmt {
                node_id: AstNodeId::INVALID,
                x: x0,
            }),
            false,
        )
    }

    fn parse_call_expr(&mut self, call_type: &str) -> Option<CallExpr> {
        let x = self.parse_rhs(); // could be a conversion: (some type)(x)
        let was_paren = matches!(x, Expr::ParenExpr(_));
        let paren_pos = was_paren.then(|| x.pos());
        let x = unparen(x);
        if was_paren {
            let msg = format!("expression in {call_type} must not be parenthesized");
            self.error(paren_pos.unwrap(), msg);
        }
        match x {
            Expr::CallExpr(call) => Some(*call),
            Expr::BadExpr(_) => None,
            x => {
                // only report error if it's a new one
                let msg = format!("expression in {call_type} must be function call");
                self.error(x.end(), msg);
                None
            }
        }
    }

    fn parse_go_stmt(&mut self) -> Stmt {
        let pos = self.expect(Token::Go);
        let call = self.parse_call_expr("go");
        self.expect_semi();
        match call {
            None => Stmt::BadStmt(BadStmt {
                node_id: AstNodeId::INVALID,
                from: pos,
                to: pos + 2,
            }), // len("go")
            Some(call) => Stmt::GoStmt(GoStmt {
                node_id: AstNodeId::INVALID,
                go_: pos,
                call: Box::new(call),
            }),
        }
    }

    fn parse_defer_stmt(&mut self) -> Stmt {
        let pos = self.expect(Token::Defer);
        let call = self.parse_call_expr("defer");
        self.expect_semi();
        match call {
            None => Stmt::BadStmt(BadStmt {
                node_id: AstNodeId::INVALID,
                from: pos,
                to: pos + 5,
            }), // len("defer")
            Some(call) => Stmt::DeferStmt(DeferStmt {
                node_id: AstNodeId::INVALID,
                defer_: pos,
                call: Box::new(call),
            }),
        }
    }

    fn parse_return_stmt(&mut self) -> Stmt {
        let pos = self.pos;
        self.expect(Token::Return);
        let mut x: Vec<Expr> = Vec::new();
        if !matches!(self.tok, Token::Semicolon | Token::RBrace) {
            x = self.parse_list(true);
        }
        self.expect_semi();

        Stmt::ReturnStmt(ReturnStmt {
            node_id: AstNodeId::INVALID,
            return_: pos,
            results: x,
        })
    }

    fn parse_branch_stmt(&mut self, tok: Token) -> Stmt {
        let pos = self.expect(tok);
        let mut label = None;
        if tok == Token::Goto
            || ((tok == Token::Continue || tok == Token::Break) && self.tok == Token::Ident)
        {
            label = Some(self.parse_ident());
        }
        self.expect_semi();

        Stmt::BranchStmt(BranchStmt {
            node_id: AstNodeId::INVALID,
            tok_pos: pos,
            tok,
            label,
        })
    }

    fn make_expr(&mut self, s: Option<Stmt>, want: &str) -> Option<Expr> {
        let s = s?;
        if let Stmt::ExprStmt(es) = &s {
            return Some(es.x.clone());
        }
        let found = if matches!(s, Stmt::AssignStmt(_)) {
            "assignment"
        } else {
            "simple statement"
        };
        let msg = format!(
            "expected {want}, found {found} (missing parentheses around composite literal?)"
        );
        self.error(s.pos(), msg);
        Some(Expr::BadExpr(BadExpr {
            node_id: AstNodeId::INVALID,
            from: s.pos(),
            to: s.end(),
        }))
    }

    /// parse_if_header is an adjusted version of parser.header
    /// in cmd/compile/internal/syntax/parser.go, which has
    /// been tuned for better error handling.
    fn parse_if_header(&mut self) -> (Option<Stmt>, Expr) {
        if self.tok == Token::LBrace {
            self.error(self.pos, "missing condition in if statement".to_string());
            return (
                None,
                Expr::BadExpr(BadExpr {
                    node_id: AstNodeId::INVALID,
                    from: self.pos,
                    to: self.pos,
                }),
            );
        }
        // self.tok != token.LBRACE

        let prev_lev = self.expr_lev;
        self.expr_lev = -1;

        let mut init: Option<Stmt> = None;
        if self.tok != Token::Semicolon {
            // accept potential variable declaration but complain
            if self.tok == Token::Var {
                self.next();
                self.error(
                    self.pos,
                    "var declaration not allowed in if initializer".to_string(),
                );
            }
            let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
            init = Some(s);
        }

        let mut cond_stmt: Option<Stmt> = None;
        let mut semi: Option<Semi> = None; // ";" or "\n"; Some if pos valid
        if self.tok != Token::LBrace {
            if self.tok == Token::Semicolon {
                semi = Some(Semi {
                    pos: self.pos,
                    lit: self.lit.clone(),
                });
                self.next();
            } else {
                self.expect(Token::Semicolon);
            }
            if self.tok != Token::LBrace {
                let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
                cond_stmt = Some(s);
            }
        } else {
            cond_stmt = init.take();
        }

        let mut cond: Option<Expr> = None;
        if let Some(cond_stmt) = cond_stmt {
            cond = self.make_expr(Some(cond_stmt), "boolean expression");
        } else if let Some(semi) = &semi {
            if semi.lit == "\n" {
                self.error(
                    semi.pos,
                    "unexpected newline, expecting { after if clause".to_string(),
                );
            } else {
                self.error(semi.pos, "missing condition in if statement".to_string());
            }
        }

        // make sure we have a valid AST
        let cond = match cond {
            Some(cond) => cond,
            None => Expr::BadExpr(BadExpr {
                node_id: AstNodeId::INVALID,
                from: self.pos,
                to: self.pos,
            }),
        };

        self.expr_lev = prev_lev;
        (init, cond)
    }

    fn parse_if_stmt(&mut self) -> Stmt {
        let _nest = inc_nest_lev(self);

        let pos = self.expect(Token::If);

        let (init, cond) = self.parse_if_header();
        let body = self.parse_block_stmt();

        let else_;
        if self.tok == Token::Else {
            self.next();
            match self.tok {
                Token::If => else_ = Some(self.parse_if_stmt()),
                Token::LBrace => {
                    else_ = Some(Stmt::BlockStmt(self.parse_block_stmt()));
                    self.expect_semi();
                }
                _ => {
                    self.error_expected(self.pos, "if statement or block");
                    else_ = Some(Stmt::BadStmt(BadStmt {
                        node_id: AstNodeId::INVALID,
                        from: self.pos,
                        to: self.pos,
                    }));
                }
            }
        } else {
            else_ = None;
            self.expect_semi();
        }

        Stmt::IfStmt(Box::new(IfStmt {
            node_id: AstNodeId::INVALID,
            if_: pos,
            init,
            cond,
            body: Box::new(body),
            else_,
        }))
    }

    fn parse_case_clause(&mut self) -> Stmt {
        let pos = self.pos;
        let mut list: Vec<Expr> = Vec::new();
        if self.tok == Token::Case {
            self.next();
            list = self.parse_list(true);
        } else {
            self.expect(Token::Default);
        }

        let colon = self.expect(Token::Colon);
        let body = self.parse_stmt_list();

        Stmt::CaseClause(CaseClause {
            node_id: AstNodeId::INVALID,
            case: pos,
            list,
            colon,
            body,
        })
    }

    fn is_type_switch_assert(x: &Expr) -> bool {
        matches!(
            x,
            Expr::TypeAssertExpr(t) if t.typ.is_none()
        )
    }

    fn is_type_switch_guard(&mut self, s: &Stmt) -> bool {
        match s {
            Stmt::ExprStmt(es) => {
                // x.(type)
                Self::is_type_switch_assert(&es.x)
            }
            Stmt::AssignStmt(as_)
                if as_.lhs.len() == 1
                    && as_.rhs.len() == 1
                    && Self::is_type_switch_assert(&as_.rhs[0]) =>
            {
                match as_.tok {
                    Token::Assign => {
                        // permit v = x.(type) but complain
                        self.error(as_.tok_pos, "expected ':=', found '='".to_string());
                        true
                    }
                    Token::Define => true,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn parse_switch_stmt(&mut self) -> Stmt {
        let pos = self.expect(Token::Switch);

        let mut s1: Option<Stmt> = None;
        let mut s2: Option<Stmt> = None;
        if self.tok != Token::LBrace {
            let prev_lev = self.expr_lev;
            self.expr_lev = -1;
            if self.tok != Token::Semicolon {
                let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
                s2 = Some(s);
            }
            if self.tok == Token::Semicolon {
                self.next();
                s1 = s2.take();
                s2 = None;
                if self.tok != Token::LBrace {
                    // A TypeSwitchGuard may declare a variable in addition
                    // to the variable declared in the initial SimpleStmt.
                    // Introduce extra scope to avoid redeclaration errors:
                    //
                    //	switch t := 0; t := x.(T) { ... }
                    //
                    // (this code is not valid Go because the first t
                    // cannot be accessed and thus is never used, the extra
                    // scope is needed for the correct error message).
                    //
                    // If we don't have a type switch, s2 must be an expression.
                    // Having the extra nested but empty scope won't affect it.
                    let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
                    s2 = Some(s);
                }
            }
            self.expr_lev = prev_lev;
        }

        let type_switch = match &s2 {
            Some(s2) => self.is_type_switch_guard(s2),
            None => false,
        };
        let lbrace = self.expect(Token::LBrace);
        let mut list: Vec<Stmt> = Vec::new();
        while matches!(self.tok, Token::Case | Token::Default) {
            list.push(self.parse_case_clause());
        }
        let rbrace = self.expect(Token::RBrace);
        self.expect_semi();
        let body = BlockStmt {
            node_id: AstNodeId::INVALID,
            lbrace,
            list,
            rbrace,
        };

        if type_switch {
            return Stmt::TypeSwitchStmt(Box::new(TypeSwitchStmt {
                node_id: AstNodeId::INVALID,
                switch: pos,
                init: s1,
                assign: s2.expect("type switch guard"),
                body: Box::new(body),
            }));
        }

        let tag = self.make_expr(s2, "switch expression");
        Stmt::SwitchStmt(Box::new(SwitchStmt {
            node_id: AstNodeId::INVALID,
            switch: pos,
            init: s1,
            tag,
            body: Box::new(body),
        }))
    }

    fn parse_comm_clause(&mut self) -> Stmt {
        let pos = self.pos;
        let mut comm: Option<Stmt> = None;
        if self.tok == Token::Case {
            self.next();
            let mut lhs = self.parse_list(false);
            if self.tok == Token::Arrow {
                // SendStmt
                if lhs.len() > 1 {
                    let pos = lhs[0].pos();
                    self.error_expected(pos, "1 expression");
                    // continue with first expression
                }
                let arrow = self.pos;
                self.next();
                let rhs = self.parse_rhs();
                let chan_ = lhs.remove(0);
                comm = Some(Stmt::SendStmt(SendStmt {
                    node_id: AstNodeId::INVALID,
                    chan_,
                    arrow,
                    value: rhs,
                }));
            } else {
                // RecvStmt
                if self.tok == Token::Assign || self.tok == Token::Define {
                    // RecvStmt with assignment
                    if lhs.len() > 2 {
                        let pos = lhs[0].pos();
                        self.error_expected(pos, "1 or 2 expressions");
                        // continue with first two expressions
                        lhs.truncate(2);
                    }
                    let (tok_pos, tok) = (self.pos, self.tok);
                    self.next();
                    let rhs = self.parse_rhs();
                    comm = Some(Stmt::AssignStmt(AssignStmt {
                        node_id: AstNodeId::INVALID,
                        lhs,
                        tok_pos,
                        tok,
                        rhs: vec![rhs],
                    }));
                } else {
                    // lhs must be single receive operation
                    if lhs.len() > 1 {
                        let pos = lhs[0].pos();
                        self.error_expected(pos, "1 expression");
                        // continue with first expression
                    }
                    let x = lhs.remove(0);
                    comm = Some(Stmt::ExprStmt(ExprStmt {
                        node_id: AstNodeId::INVALID,
                        x,
                    }));
                }
            }
        } else {
            self.expect(Token::Default);
        }

        let colon = self.expect(Token::Colon);
        let body = self.parse_stmt_list();

        Stmt::CommClause(Box::new(CommClause {
            node_id: AstNodeId::INVALID,
            case: pos,
            comm,
            colon,
            body,
        }))
    }

    fn parse_select_stmt(&mut self) -> Stmt {
        let pos = self.expect(Token::Select);
        let lbrace = self.expect(Token::LBrace);
        let mut list: Vec<Stmt> = Vec::new();
        while matches!(self.tok, Token::Case | Token::Default) {
            list.push(self.parse_comm_clause());
        }
        let rbrace = self.expect(Token::RBrace);
        self.expect_semi();
        let body = BlockStmt {
            node_id: AstNodeId::INVALID,
            lbrace,
            list,
            rbrace,
        };

        Stmt::SelectStmt(SelectStmt {
            node_id: AstNodeId::INVALID,
            select: pos,
            body: Box::new(body),
        })
    }

    fn parse_for_stmt(&mut self) -> Stmt {
        let pos = self.expect(Token::For);

        let mut s1: Option<Stmt> = None;
        let mut s2: Option<Stmt> = None;
        let mut s3: Option<Stmt> = None;
        let mut is_range = false;
        if self.tok != Token::LBrace {
            let prev_lev = self.expr_lev;
            self.expr_lev = -1;
            if self.tok != Token::Semicolon {
                if self.tok == Token::Range {
                    // "for range x" (nil lhs in assignment)
                    let pos = self.pos;
                    self.next();
                    let x = self.parse_rhs();
                    let y = vec![Expr::UnaryExpr(Box::new(UnaryExpr {
                        node_id: AstNodeId::INVALID,
                        op_pos: pos,
                        op: Token::Range,
                        x,
                    }))];
                    s2 = Some(Stmt::AssignStmt(AssignStmt {
                        node_id: AstNodeId::INVALID,
                        lhs: Vec::new(),
                        tok_pos: NO_POS,
                        tok: Token::Illegal,
                        rhs: y,
                    }));
                    is_range = true;
                } else {
                    let (s, range) = self.parse_simple_stmt(SimpleStmtMode::RangeOk);
                    s2 = Some(s);
                    is_range = range;
                }
            }
            if !is_range && self.tok == Token::Semicolon {
                self.next();
                s1 = s2.take();
                s2 = None;
                if self.tok != Token::Semicolon {
                    let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
                    s2 = Some(s);
                }
                self.expect_semi();
                if self.tok != Token::LBrace {
                    let (s, _) = self.parse_simple_stmt(SimpleStmtMode::Basic);
                    s3 = Some(s);
                }
            }
            self.expr_lev = prev_lev;
        }

        let body = self.parse_block_stmt();
        self.expect_semi();

        if is_range {
            let as_ = match s2 {
                Some(Stmt::AssignStmt(as_)) => as_,
                _ => unreachable!("range clause without assignment"),
            };
            // check lhs
            let (key, value) = match as_.lhs.len() {
                0 => (None, None), // nothing to do
                1 => {
                    let mut it = as_.lhs.into_iter();
                    (it.next(), None)
                }
                2 => {
                    let mut it = as_.lhs.into_iter();
                    (it.next(), it.next())
                }
                _ => {
                    let last = as_.lhs.last().unwrap();
                    let msg = "at most 2 expressions";
                    self.error_expected(last.pos(), msg);
                    return Stmt::BadStmt(BadStmt {
                        node_id: AstNodeId::INVALID,
                        from: pos,
                        to: body.end(),
                    });
                }
            };
            // parse_simple_stmt returned a right-hand side that
            // is a single unary expression of the form "range x"
            let r0 = as_.rhs.into_iter().next().expect("range rhs");
            let (range, x) = match r0 {
                Expr::UnaryExpr(u) => {
                    let u = *u;
                    assert(u.op == Token::Range, "range rhs is not a range unary");
                    (u.op_pos, u.x)
                }
                _ => unreachable!("range rhs is not a unary expression"),
            };
            return Stmt::RangeStmt(RangeStmt {
                node_id: AstNodeId::INVALID,
                for_: pos,
                key,
                value,
                tok_pos: as_.tok_pos,
                tok: as_.tok,
                range,
                x,
                body: Box::new(body),
            });
        }

        // regular for statement
        let cond = self.make_expr(s2, "boolean or range expression");
        Stmt::ForStmt(Box::new(ForStmt {
            node_id: AstNodeId::INVALID,
            for_: pos,
            init: s1,
            cond,
            post: s3,
            body: Box::new(body),
        }))
    }

    fn parse_stmt(&mut self) -> Stmt {
        let _nest = inc_nest_lev(self);

        match self.tok {
            Token::Const | Token::Type | Token::Var => Stmt::DeclStmt(DeclStmt {
        node_id: AstNodeId::INVALID,
                decl: self.parse_decl(stmt_start),
            }),
            // tokens that may start an expression
            Token::Ident
            | Token::Int
            | Token::Float
            | Token::Imag
            | Token::Char
            | Token::String
            | Token::Func
            | Token::LParen // operands
            | Token::LBrack
            | Token::Struct
            | Token::Map
            | Token::Chan
            | Token::Interface // composite types
            | Token::Add
            | Token::Sub
            | Token::Mul
            | Token::And
            | Token::XOr
            | Token::Arrow
            | Token::Not => {
                // unary operators
                let (s, _) = self.parse_simple_stmt(SimpleStmtMode::LabelOk);
                // because of the required look-ahead, labeled statements are
                // parsed by parse_simple_stmt - don't expect a semicolon after
                // them
                if !matches!(s, Stmt::LabeledStmt(_)) {
                    self.expect_semi();
                }
                s
            }
            Token::Go => self.parse_go_stmt(),
            Token::Defer => self.parse_defer_stmt(),
            Token::Return => self.parse_return_stmt(),
            Token::Break | Token::Continue | Token::Goto | Token::FallThrough => {
                self.parse_branch_stmt(self.tok)
            }
            Token::LBrace => {
                let s = self.parse_block_stmt();
                self.expect_semi();
                Stmt::BlockStmt(s)
            }
            Token::If => self.parse_if_stmt(),
            Token::Switch => self.parse_switch_stmt(),
            Token::Select => self.parse_select_stmt(),
            Token::For => self.parse_for_stmt(),
            Token::Semicolon => {
                // Is it ever possible to have an implicit semicolon
                // producing an empty statement in a valid program?
                // (handle correctly anyway)
                let s = EmptyStmt {
        node_id: AstNodeId::INVALID,
                    semicolon: self.pos,
                    implicit: self.lit == "\n",
                };
                self.next();
                Stmt::EmptyStmt(s)
            }
            Token::RBrace => {
                // a semicolon may be omitted before a closing "}"
                Stmt::EmptyStmt(EmptyStmt {
        node_id: AstNodeId::INVALID,
                    semicolon: self.pos,
                    implicit: true,
                })
            }
            _ => {
                // no statement found
                let pos = self.pos;
                self.error_expected(pos, "statement");
                self.advance(stmt_start);
                Stmt::BadStmt(BadStmt {
        node_id: AstNodeId::INVALID,
                    from: pos,
                    to: self.pos,
                })
            }
        }
    }

    /// Re-associates the "<-" of a receive expression with the channel type
    /// it precedes (Go's loop in parseUnaryExpr mutating a chain of channel
    /// types via shared pointers; with owned nodes this is a recursive
    /// descent along `ChanType.value`).
    fn re_associate_arrow(&mut self, ct: &mut ChanType, arrow: &mut Pos, dir: &mut ChanDir) {
        if ct.dir == ChanDir::RECV {
            // error: (<-type) is (<-(<-chan T))
            self.error_expected(ct.arrow, "'chan'");
        }
        // arrow, typ.Begin, typ.Arrow = typ.Arrow, arrow, arrow
        let new_arrow = ct.arrow;
        ct.begin = *arrow;
        ct.arrow = *arrow;
        *arrow = new_arrow;
        // dir, typ.Dir = typ.Dir, ast.RECV
        let new_dir = ct.dir;
        ct.dir = ChanDir::RECV;
        *dir = new_dir;
        // descend while Go's loop would continue (ok && dir == ast.SEND)
        if *dir == ChanDir::SEND
            && let Expr::ChanType(inner) = &mut ct.value
        {
            self.re_associate_arrow(inner, arrow, dir);
        }
    }

    // ------------------------------------------------------------------------
    // Declarations

    /// parse_generic_type parses the type parameters (and following type) of
    /// a type specification (Go's `parseGenericType`, mutating the spec).
    fn parse_generic_type(
        &mut self,
        spec: &mut TypeSpec,
        open_pos: Pos,
        name0: Option<Ident>,
        typ0: Option<Expr>,
    ) {
        let list = self.parse_parameter_list(name0, typ0, Token::RBrack, false);
        let close_pos = self.expect(Token::RBrack);
        spec.type_params = Some(FieldList {
            node_id: AstNodeId::INVALID,
            opening: open_pos,
            list,
            closing: close_pos,
        });
        if self.tok == Token::Assign {
            // type alias
            spec.assign = self.pos;
            self.next();
        }
        spec.typ = self.parse_type();
    }

    fn parse_func_decl(&mut self) -> Decl {
        let commands = self.take_leading_commands(self.pos);
        let pos = self.expect(Token::Func);

        let mut recv = None;
        if self.tok == Token::LParen {
            recv = self.parse_parameters(false);
        }

        let ident = self.parse_ident();

        let mut tparams = None;
        if self.tok == Token::LBrack {
            tparams = self.parse_type_parameters();
        }
        let params = self.parse_parameters(false);
        let results = self.parse_parameters(true);

        let mut body: Option<BlockStmt> = None;
        match self.tok {
            Token::LBrace => {
                body = Some(self.parse_body());
                self.expect_semi();
            }
            Token::Semicolon => {
                self.next();
                if self.tok == Token::LBrace {
                    // opening { of function declaration on next line
                    self.error(
                        self.pos,
                        "unexpected semicolon or newline before {".to_string(),
                    );
                    body = Some(self.parse_body());
                    self.expect_semi();
                }
            }
            _ => {
                self.expect_semi();
            }
        }

        Decl::FuncDecl(FuncDecl {
            node_id: AstNodeId::INVALID,
            commands,
            recv,
            name: ident,
            typ: FuncType {
                node_id: AstNodeId::INVALID,
                func: pos,
                type_params: tparams,
                params,
                results,
            },
            body: body.map(Box::new),
        })
    }

    fn parse_gen_decl(&mut self, keyword: Token, f: SpecFunction) -> Decl {
        let commands = self.take_leading_commands(self.pos);
        let pos = self.expect(keyword);
        let mut lparen = NO_POS;
        let mut rparen = NO_POS;
        let mut list: Vec<Spec> = Vec::new();
        if self.tok == Token::LParen {
            lparen = self.pos;
            self.next();
            // (Go counts specs with an `iota` parameter; it is unused in the
            // current snapshot and dropped in this port.)
            while self.tok != Token::RParen && self.tok != Token::EOF {
                list.push(f(self, keyword));
            }
            rparen = self.expect(Token::RParen);
            self.expect_semi();
        } else {
            list.push(f(self, keyword));
        }

        Decl::GenDecl(GenDecl {
            node_id: AstNodeId::INVALID,
            commands,
            tok_pos: pos,
            tok: keyword,
            lparen,
            specs: list,
            rparen,
        })
    }

    fn parse_decl(&mut self, sync: TokenSet) -> Decl {
        // (Go's `var f parseSpecFunction` dispatch is inlined via the match
        // below; the spec functions take the keyword so they match Go's
        // `parseSpecFunction` type.)
        let f: SpecFunction = match self.tok {
            Token::Import => parse_import_spec,

            Token::Const | Token::Var => parse_value_spec,

            Token::Type => parse_type_spec,

            Token::Func => return self.parse_func_decl(),

            _ => {
                let pos = self.pos;
                self.error_expected(pos, "declaration");
                self.advance(sync);
                return Decl::BadDecl(BadDecl {
                    node_id: AstNodeId::INVALID,
                    from: pos,
                    to: self.pos,
                });
            }
        };

        self.parse_gen_decl(self.tok, f)
    }

    // ------------------------------------------------------------------------
    // Source files

    /// Parses a Go source file. Returns None if parsing was abandoned early
    /// (errors while scanning the first token or parsing the package clause);
    /// the caller then fabricates an empty file like Go's ParseFile defer
    /// does.
    ///
    /// FileStart/FileEnd are set by the caller.
    pub(crate) fn parse_file(&mut self) -> Option<AstFile> {
        // Don't bother parsing the rest if we had errors scanning the first token.
        // Likely not a Go source file at all.
        if !self.errors.borrow().is_empty() {
            return None;
        }

        // package clause
        let commands = self.take_file_commands();
        let pos = self.expect(Token::Package);
        // Go spec: The package clause is not a declaration;
        // the package name does not appear in any scope.
        let ident = self.parse_ident();
        if ident.name == "_" && self.mode & DECLARATION_ERRORS != Mode::default() {
            self.error(self.pos, "invalid package name _".to_string());
        }
        self.expect_semi();

        // Don't bother parsing the rest if we had errors parsing the package clause.
        // Likely not a Go source file at all.
        if !self.errors.borrow().is_empty() {
            return None;
        }

        let mut decls: Vec<Decl> = Vec::new();
        if self.mode & PACKAGE_CLAUSE_ONLY == Mode::default() {
            // import decls
            while self.tok == Token::Import {
                decls.push(self.parse_gen_decl(Token::Import, parse_import_spec));
            }

            if self.mode & IMPORTS_ONLY == Mode::default() {
                // rest of package body
                let mut prev = Token::Import;
                while self.tok != Token::EOF {
                    // Continue to accept import declarations for error tolerance,
                    // but complain.
                    if self.tok == Token::Import && prev != Token::Import {
                        self.error(
                            self.pos,
                            "imports must appear before other declarations".to_string(),
                        );
                    }
                    prev = self.tok;

                    decls.push(self.parse_decl(decl_start));
                }
            }
        }

        let f = AstFile {
            node_id: AstNodeId::INVALID,
            commands,
            package: pos,
            name: ident,
            decls,
            file_start: NO_POS, // set by the caller, like Go's ParseFile defer
            file_end: NO_POS,
            imports: std::mem::take(&mut self.imports),
        };

        Some(f)
    }
}

/// Go's `parseSpecFunction` function type. The `doc` and `iota` parameters
/// are not ported: comments are discarded, and Go's iota parameter is unused.
type SpecFunction = for<'a, 'b> fn(&'a mut Parser<'b>, Token) -> Spec;

fn parse_import_spec(p: &mut Parser, _keyword: Token) -> Spec {
    let commands = p.take_leading_commands(p.pos);
    let mut ident: Option<Ident> = None;
    match p.tok {
        Token::Ident => ident = Some(p.parse_ident()),
        Token::Period => {
            ident = Some(Ident {
                node_id: AstNodeId::INVALID,
                name_pos: p.pos,
                name: ".".to_string(),
            });
            p.next();
        }
        _ => {}
    }

    let pos = p.pos;
    let mut end = p.pos;
    let mut path = String::new();
    if p.tok == Token::String {
        path = p.lit.clone();
        end = p.end();
        p.next();
    } else if p.tok.is_literal() {
        p.error(pos, "import path must be a string".to_string());
        p.next();
    } else {
        p.error(pos, "missing import path".to_string());
        p.advance(expr_end);
    }
    p.expect_semi();

    // collect imports
    let spec = ImportSpec {
        node_id: AstNodeId::INVALID,
        commands,
        name: ident,
        path: BasicLit {
            node_id: AstNodeId::INVALID,
            value_pos: pos,
            value_end: end,
            kind: Token::String,
            value: path,
        },
    };
    // (Go shares the spec between the declaration list and File.Imports;
    // in this port the spec is duplicated by value, to be revisited by the
    // future object-resolution round.)
    p.imports.push(spec.clone());
    Spec::ImportSpec(spec)
}

fn parse_value_spec(p: &mut Parser, keyword: Token) -> Spec {
    let commands = p.take_leading_commands(p.pos);
    let idents = p.parse_ident_list();
    let mut typ: Option<Expr> = None;
    let mut values: Vec<Expr> = Vec::new();
    match keyword {
        Token::Const => {
            // always permit optional type and initialization for more tolerant parsing
            if !matches!(p.tok, Token::EOF | Token::Semicolon | Token::RParen) {
                typ = p.try_ident_or_type();
                if p.tok == Token::Assign {
                    p.next();
                    values = p.parse_list(true);
                }
            }
        }
        Token::Var => {
            if p.tok != Token::Assign {
                typ = Some(p.parse_type());
            }
            if p.tok == Token::Assign {
                p.next();
                values = p.parse_list(true);
            }
        }
        _ => unreachable!(),
    }
    p.expect_semi();

    Spec::ValueSpec(ValueSpec {
        node_id: AstNodeId::INVALID,
        commands,
        names: idents,
        typ,
        values,
    })
}

fn parse_type_spec(p: &mut Parser, _keyword: Token) -> Spec {
    let commands = p.take_leading_commands(p.pos);
    let name = p.parse_ident();
    let mut spec = TypeSpec {
        node_id: AstNodeId::INVALID,
        commands,
        name,
        type_params: None,
        assign: NO_POS,
        // placeholder; overwritten in every branch below before returning
        typ: Expr::BadExpr(BadExpr {
            node_id: AstNodeId::INVALID,
            from: NO_POS,
            to: NO_POS,
        }),
    };

    if p.tok == Token::LBrack {
        // spec.Name "[" ...
        // array/slice type or type parameter list
        let lbrack = p.pos;
        p.next();
        if p.tok == Token::Ident {
            // We may have an array type or a type parameter list.
            // In either case we expect an expression x (which may
            // just be a name, or a more complex expression) which
            // we can analyze further.
            //
            // A type parameter list may have a type bound starting
            // with a "[" as in: P []E. In that case, simply parsing
            // an expression would lead to an error: P[] is invalid.
            // But since index or slice expressions are never constant
            // and thus invalid array length expressions, if the name
            // is followed by "[" it must be the start of an array or
            // slice constraint. Only if we don't see a "[" do we
            // need to parse a full expression. Notably, name <- x
            // is not a concern because name <- x is a statement and
            // not an expression.
            let mut x: Expr = Expr::Ident(p.parse_ident());
            if p.tok != Token::LBrack {
                // To parse the expression starting with name, expand
                // the call sequence we would get by passing in name
                // to parser.expr, and pass in name to parsePrimaryExpr.
                p.expr_lev += 1;
                let lhs = p.parse_primary_expr(Some(x));
                x = p.parse_binary_expr(Some(lhs), Token::LOWEST_PREC + 1);
                p.expr_lev -= 1;
            }
            // Analyze expression x. If we can split x into a type parameter
            // name, possibly followed by a type parameter type, we consider
            // this the start of a type parameter list, with some caveats:
            // a single name followed by "]" tilts the decision towards an
            // array declaration; a type parameter type that could also be
            // an ordinary expression but which is followed by a comma tilts
            // the decision towards a type parameter list.
            let (pname, ptype) = extract_name(&x, p.tok == Token::Comma);
            if pname.is_some() && (ptype.is_some() || p.tok != Token::RBrack) {
                // spec.Name "[" pname ...
                // spec.Name "[" pname ptype ...
                // spec.Name "[" pname ptype "," ...
                p.parse_generic_type(&mut spec, lbrack, pname, ptype); // ptype may be nil
            } else {
                // spec.Name "[" pname "]" ...
                // spec.Name "[" x ...
                spec.typ = p.parse_array_type(lbrack, Some(x));
            }
        } else {
            // array type
            spec.typ = p.parse_array_type(lbrack, None);
        }
    } else {
        // no type parameters
        if p.tok == Token::Assign {
            // type alias
            spec.assign = p.pos;
            p.next();
        }
        spec.typ = p.parse_type();
    }

    p.expect_semi();

    Spec::TypeSpec(spec)
}

/// extract_name splits the expression x into (name, expr) if syntactically
/// x can be written as name expr. The split only happens if expr is a type
/// element (per the `is_type_elem` predicate) or if force is set.
/// If x is just a name, the result is (name, nil). If the split succeeds,
/// the result is (name, expr). Otherwise the result is (nil, x) - in this
/// port the caller still owns x, so the failure case returns (nil, nil).
/// Examples:
///
/// ```text
/// x           force    name    expr
/// ------------------------------------
/// P*[]int     T/F      P       *[]int
/// P*E         T        P       *E
/// P*E         F        nil     P*E
/// P([]int)    T/F      P       ([]int)
/// P(E)        T        P       (E)
/// P(E)        F        nil     P(E)
/// P*E|F|~G    T/F      P       *E|F|~G
/// P*E|F|G     T        P       *E|F|G
/// P*E|F|G     F        nil     P*E|F|G
/// ```
fn extract_name(x: &Expr, force: bool) -> (Option<Ident>, Option<Expr>) {
    match x {
        Expr::Ident(id) => return (Some(id.clone()), None),
        Expr::BinaryExpr(b) => {
            if b.op == Token::Mul {
                if let Expr::Ident(name) = &b.x
                    && (force || is_type_elem(&b.y))
                {
                    // x = name *x.Y
                    return (
                        Some(name.clone()),
                        Some(Expr::StarExpr(Box::new(StarExpr {
                            node_id: AstNodeId::INVALID,
                            star: b.op_pos,
                            x: b.y.clone(),
                        }))),
                    );
                }
            } else if b.op == Token::Or
                && let (Some(name), Some(lhs)) = extract_name(&b.x, force || is_type_elem(&b.y))
            {
                // x = name lhs|x.Y
                return (
                    Some(name),
                    Some(Expr::BinaryExpr(Box::new(BinaryExpr {
                        node_id: AstNodeId::INVALID,
                        x: lhs,
                        op_pos: b.op_pos,
                        op: Token::Or,
                        y: b.y.clone(),
                    }))),
                );
            }
        }
        Expr::CallExpr(c) => {
            if let Expr::Ident(name) = &c.fun
                && c.args.len() == 1
                && !c.ellipsis.is_valid()
                && (force || is_type_elem(&c.args[0]))
            {
                // x = name (x.Args[0])
                // (Note that the cmd/compile/internal/syntax parser does
                // not care about syntax tree fidelity and does not
                // preserve parentheses here.)
                return (
                    Some(name.clone()),
                    Some(Expr::ParenExpr(Box::new(ParenExpr {
                        node_id: AstNodeId::INVALID,
                        lparen: c.lparen,
                        x: c.args[0].clone(),
                        rparen: c.rparen,
                    }))),
                );
            }
        }
        _ => {}
    }
    (None, None)
}

/// is_type_elem reports whether x is a (possibly parenthesized) type element
/// expression. The result is false if x could be a type element OR an
/// ordinary (value) expression.
fn is_type_elem(x: &Expr) -> bool {
    match x {
        Expr::ArrayType(_)
        | Expr::StructType(_)
        | Expr::FuncType(_)
        | Expr::InterfaceType(_)
        | Expr::MapType(_)
        | Expr::ChanType(_) => true,
        Expr::BinaryExpr(b) => is_type_elem(&b.x) || is_type_elem(&b.y),
        Expr::UnaryExpr(u) => u.op == Token::Tilde,
        Expr::ParenExpr(pe) => is_type_elem(&pe.x),
        _ => false,
    }
}

// Ported Go parser tests (parser_test.go, error_test.go, short_test.go,
// example_test.go) live in this sibling file, reachable only for tests.
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
