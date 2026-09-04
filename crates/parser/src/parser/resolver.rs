//! Deprecated identifier resolution.
//!
//! This module is ported from Go's standard `go/parser/resolver.go`, which
//! resolves the identifiers of an already-parsed file into `Ident.obj`, and
//! fills `File.scope` and `File.unresolved`.
//!
//! Adaptations:
//!
//! - Go walks the tree with an `ast.Visitor` over shared pointers and writes
//!   `ident.Obj` while descending; in this port the AST is owned by value
//!   and there is no mutable generic walker, so the traversal is written by
//!   hand as `&mut`-recursion mirroring the order of Go's `Visit` cases
//!   (the "dispatch layer"), plus a second `&mut` pass over the identifiers
//!   only that applies the results of the final package-scope lookup (the
//!   "finalize layer"). The two layers must stay in sync with the children
//!   order of `ast::walk::children`.
//! - Go marks collected unresolved identifiers with a package-global
//!   sentinel object and later patches them through retained tree pointers;
//!   here each `resolve_file` builds its own sentinel and the finalize
//!   layer patches every identifier whose `obj` is that sentinel.
//! - Label references are not patchable at scope-close time (their `&mut`
//!   borrow is gone), so `close_label_scope` records resolved label
//!   objects keyed by position; the finalize layer applies them. Unresolved
//!   labels report "label ... undefined" immediately, like Go.
//! - Go's package-level singleflight (`ResolveFile` + `resolveOnces`) is
//!   dropped: the port is single-threaded and files are owned by value, so
//!   a `ResolveFile` runs a fresh resolution every time (a `file.scope`
//!   guard makes a repeated call a no-op, like Go's successful case).
//!   `TestResolveFilePanicConcurrent` is therefore not ported.
//! - The `debugResolve` tracing is not ported.
//! - `Object.decl` holds a by-value copy of the declaring node made at
//!   declaration time (not pointer-identical to the tree node); see
//!   [`crate::ast::scope`] for the rationale.
//!
//! Go line numbers are cited next to each ported method.

use std::rc::Rc;

use crate::ast::*;
use crate::token::{File as TokenFile, Pos, Token};

use super::parser::Bailout;

/// Go's `maxScopeDepth` (resolver.go:119): the deepest scope nesting
/// accepted during object resolution.
pub(crate) const MAX_SCOPE_DEPTH: i32 = 1e3 as i32;

/// Go's `resolver` struct (resolver.go:121-135). All trees live in the
/// caller; the resolver only keeps scope/object state and collects
/// unresolved and label-pending identifiers.
struct Resolver<'a> {
    handle: Rc<TokenFile>,
    decl_err: Option<&'a dyn Fn(Pos, String)>,

    // Ordinary identifier scopes
    pkg_scope: Rc<Scope>,         // pkg_scope.outer == None
    top_scope: Option<Rc<Scope>>, // top-most scope; may be pkg_scope
    unresolved: Vec<Ident>,       // unresolved identifiers
    depth: i32,                   // scope depth

    // Label scopes
    // (maintained by open/close_label_scope)
    label_scope: Option<Rc<Scope>>, // label scope for current function
    target_stack: Vec<Vec<Ident>>,  // stack of unresolved labels

    // Labels resolved at scope-close time, keyed by the position of their
    // referencing identifier (applied by the finalize pass).
    label_resolutions: Vec<(Pos, Rc<Object>)>,

    // Sentinel marking collected unresolved identifiers.
    sentinel: Rc<Object>,
}

impl<'a> Resolver<'a> {
    /// Go's `openScope` (resolver.go:151-160).
    fn open_scope(&mut self, pos: Pos) {
        self.depth += 1;
        if self.depth > MAX_SCOPE_DEPTH {
            std::panic::panic_any(Bailout {
                pos,
                msg: "exceeded max scope depth during object resolution".to_string(),
            });
        }
        let outer = self.top_scope.clone();
        self.top_scope = Some(Rc::new(Scope::new_scope(outer)));
    }

    /// Go's `closeScope` (resolver.go:162-168).
    fn close_scope(&mut self) {
        self.depth -= 1;
        self.top_scope = self.top_scope.as_ref().and_then(|s| s.outer.clone());
    }

    /// Go's `openLabelScope` (resolver.go:170-173).
    fn open_label_scope(&mut self) {
        let outer = self.label_scope.clone();
        self.label_scope = Some(Rc::new(Scope::new_scope(outer)));
        self.target_stack.push(Vec::new());
    }

    /// Go's `closeLabelScope` (resolver.go:175-188).
    fn close_label_scope(&mut self) {
        // resolve labels
        let targets = self.target_stack.pop().expect("target stack underflow");
        let scope = self.label_scope.clone();
        if let Some(scope) = scope {
            for ident in targets {
                match scope.lookup(&ident.name) {
                    Some(obj) => {
                        // The label object is applied to the tree by the
                        // finalize pass (its `&mut` borrow is no longer
                        // available here).
                        self.label_resolutions.push((ident.name_pos, obj));
                    }
                    None => {
                        if let Some(decl_err) = &self.decl_err {
                            let msg = format!("label {} undefined", ident.name);
                            decl_err(ident.name_pos, msg);
                        }
                    }
                }
            }
        } else {
            assert_(false, "unbalanced label scopes");
        }
        // pop label scope
        self.label_scope = self.label_scope.as_ref().and_then(|s| s.outer.clone());
    }

    /// Renders a position through the token file, like Go's `sprintf`
    /// (resolver.go:141-149) renders `token.Pos` arguments.
    fn sprintf(&self, format: &str, pos: Pos) -> String {
        // (Go's sprintf only ever substitutes token.Pos arguments.)
        format.replace("%v", &self.handle.position(pos).to_string())
    }

    /// Go's `declare` (resolver.go:190-218). `iota` is the const-spec index
    /// stored as `ObjectData::Iota` (Go's `data`); pass None otherwise.
    fn declare(
        &mut self,
        decl: &ObjectDecl,
        iota: Option<i64>,
        scope: &Rc<Scope>,
        kind: ObjKind,
        idents: &mut [Ident],
    ) {
        for ident in idents {
            if ident.obj.is_some() {
                panic!(
                    "{}: identifier {} already declared or resolved",
                    self.sprintf("%v", ident.name_pos),
                    ident.name
                );
            }
            let mut obj_val = Object::new_obj(kind, ident.name.clone());
            // remember the corresponding declaration for redeclaration
            // errors and global variable resolution/typechecking phase
            // (Go writes obj.Decl/obj.Data before handing obj out.)
            obj_val.decl = Some(decl.clone());
            obj_val.data = iota.map(ObjectData::Iota);
            let obj = Rc::new(obj_val);
            // Identifiers (for receiver type parameters) are written to the
            // scope, but never set as the resolved object.
            // See go.dev/issue/50956.
            if !matches!(decl, ObjectDecl::Ident(_)) {
                ident.obj = Some(obj.clone());
            }
            if ident.name != "_" {
                if let Some(alt) = scope.insert(obj) {
                    if let Some(decl_err) = &self.decl_err {
                        let mut prev_decl = String::new();
                        if alt.pos().is_valid() {
                            prev_decl = self.sprintf("\n\tprevious declaration at %v", alt.pos());
                        }
                        let msg = format!("{} redeclared in this block{}", ident.name, prev_decl);
                        decl_err(ident.name_pos, msg);
                    }
                }
            }
        }
    }

    /// Go's `shortVarDecl` (resolver.go:220-247).
    fn short_var_decl(&mut self, decl: &mut AssignStmt) {
        // Go spec: A short variable declaration may redeclare variables
        // provided they were originally declared in the same block with
        // the same type, and at least one of the non-blank variables is new.
        // (The declaration is copied for `Object.decl` before the lhs
        // identifiers are mutated; each object stores its own by-value
        // copy.)
        let decl_copy = decl.clone();
        let mut n = 0; // number of new variables
        for x in &mut decl.lhs {
            if let Expr::Ident(ident) = x {
                assert_(
                    ident.obj.is_none(),
                    "identifier already declared or resolved",
                );
                let mut obj_val = Object::new_obj(ObjKind::Var, ident.name.clone());
                // remember corresponding assignment for other tools
                obj_val.decl = Some(ObjectDecl::AssignStmt(decl_copy.clone()));
                let obj = Rc::new(obj_val);
                ident.obj = Some(obj.clone());
                if ident.name != "_" {
                    if let Some(alt) = self.top_scope.as_ref().expect("top scope").insert(obj) {
                        ident.obj = Some(alt); // redeclaration
                    } else {
                        n += 1; // new declaration
                    }
                }
            }
        }
        if n == 0 {
            if let Some(decl_err) = &self.decl_err {
                let pos = decl.lhs.first().expect("no lhs").pos();
                decl_err(pos, "no new variables on left side of :=".to_string());
            }
        }
    }

    /// If `ident` is an identifier, resolve attempts to resolve it by
    /// looking up the object it denotes. If no object is found and
    /// `collect_unresolved` is set, it is marked as unresolved and
    /// collected in the list of unresolved identifiers (resolver.go:254-289).
    fn resolve(&mut self, ident: &mut Ident, collect_unresolved: bool) {
        if ident.obj.is_some() {
            panic!(
                "{}: identifier {} already declared or resolved",
                self.sprintf("%v", ident.name_pos),
                ident.name
            );
        }
        // '_' should never refer to existing declarations, because it has
        // special handling in the spec.
        if ident.name == "_" {
            return;
        }
        let mut s = self.top_scope.clone();
        while let Some(scope) = s {
            if let Some(obj) = scope.lookup(&ident.name) {
                assert_(!obj.name.is_empty(), "obj with no name");
                // Identifiers (for receiver type parameters) are written to
                // the scope, but never set as the resolved object.
                // See go.dev/issue/50956.
                if !matches!(obj.decl, Some(ObjectDecl::Ident(_))) {
                    ident.obj = Some(obj);
                }
                return;
            }
            s = scope.outer.clone();
        }
        // all local scopes are known, so any unresolved identifier
        // must be found either in the file scope, package scope
        // (perhaps in another file), or universe scope --- collect
        // them so that they can be resolved later
        if collect_unresolved {
            ident.obj = Some(self.sentinel.clone());
            self.unresolved.push(ident.clone());
        }
    }
}

fn assert_(cond: bool, msg: &str) {
    if !cond {
        panic!("go/parser internal error: {msg}");
    }
}

// ----------------------------------------------------------------------------
// Traversal
//
// The dispatch layer below is the Rust equivalent of Go's `Visit`
// (resolver.go:312-575): every node kind that Go's Visit handles
// specially gets its own arm; every other node is descended in the child
// order of `ast::walk::children` (which encodes go/ast/walk.go's order).
// The `&mut`-recursion replaces Go's `ast.Walk` over shared pointers.

impl<'a> Resolver<'a> {
    /// Go's `Visit` case for `*ast.Ident` (resolver.go:320-321).
    fn walk_expr(&mut self, x: &mut Expr) {
        match x {
            Expr::Ident(id) => self.resolve(id, true),
            Expr::FuncLit(fl) => {
                let pos = fl.pos();
                self.open_scope(pos);
                self.walk_func_type(&mut fl.typ);
                self.walk_body(&mut fl.body);
                self.close_scope();
            }
            Expr::SelectorExpr(se) => self.walk_expr(&mut se.x),
            // (Go's comment: don't try to resolve n.Sel, as we don't support
            // qualified resolution.)
            Expr::StructType(st) => {
                let pos = st.pos();
                self.open_scope(pos);
                if let Some(fields) = &mut st.fields {
                    self.walk_field_list(fields, ObjKind::Var);
                }
                self.close_scope();
            }
            Expr::FuncType(ft) => {
                let pos = ft.pos();
                self.open_scope(pos);
                self.walk_func_type(ft);
                self.close_scope();
            }
            Expr::CompositeLit(cl) => {
                if let Some(t) = &mut cl.typ {
                    self.walk_expr(t);
                }
                for e in &mut cl.elts {
                    match e {
                        Expr::KeyValueExpr(kv) => {
                            // See go.dev/issue/45160: try to resolve composite
                            // lit keys, but don't collect them as unresolved if
                            // resolution failed. This replicates existing
                            // behavior when resolving during parsing.
                            match &mut kv.key {
                                Expr::Ident(id) => self.resolve(id, false),
                                other => self.walk_expr(other),
                            }
                            self.walk_expr(&mut kv.value);
                        }
                        other => self.walk_expr(other),
                    }
                }
            }
            Expr::InterfaceType(it) => {
                let pos = it.pos();
                self.open_scope(pos);
                if let Some(methods) = &mut it.methods {
                    self.walk_field_list(methods, ObjKind::Fun);
                }
                self.close_scope();
            }
            // (all remaining expression/type nodes are walked generically in
            // the children() order of walk.rs)
            Expr::Ellipsis(ell) => {
                if let Some(elt) = &mut ell.elt {
                    self.walk_expr(elt);
                }
            }
            Expr::ParenExpr(pe) => self.walk_expr(&mut pe.x),
            Expr::IndexExpr(ix) => {
                self.walk_expr(&mut ix.x);
                self.walk_expr(&mut ix.index);
            }
            Expr::IndexListExpr(il) => {
                self.walk_expr(&mut il.x);
                for i in &mut il.indices {
                    self.walk_expr(i);
                }
            }
            Expr::SliceExpr(sl) => {
                self.walk_expr(&mut sl.x);
                if let Some(low) = &mut sl.low {
                    self.walk_expr(low);
                }
                if let Some(high) = &mut sl.high {
                    self.walk_expr(high);
                }
                if let Some(max) = &mut sl.max {
                    self.walk_expr(max);
                }
            }
            Expr::TypeAssertExpr(ta) => {
                self.walk_expr(&mut ta.x);
                if let Some(t) = &mut ta.typ {
                    self.walk_expr(t);
                }
            }
            Expr::CallExpr(cl) => {
                self.walk_expr(&mut cl.fun);
                for a in &mut cl.args {
                    self.walk_expr(a);
                }
            }
            Expr::StarExpr(se) => self.walk_expr(&mut se.x),
            Expr::UnaryExpr(ue) => self.walk_expr(&mut ue.x),
            Expr::BinaryExpr(be) => {
                self.walk_expr(&mut be.x);
                self.walk_expr(&mut be.y);
            }
            Expr::KeyValueExpr(kv) => {
                self.walk_expr(&mut kv.key);
                self.walk_expr(&mut kv.value);
            }
            Expr::ArrayType(at) => {
                if let Some(len) = &mut at.len {
                    self.walk_expr(len);
                }
                self.walk_expr(&mut at.elt);
            }
            Expr::MapType(mt) => {
                self.walk_expr(&mut mt.key);
                self.walk_expr(&mut mt.value);
            }
            Expr::ChanType(ct) => self.walk_expr(&mut ct.value),
            Expr::BadExpr(_) | Expr::BasicLit(_) => {}
        }
    }

    /// Go's `walkExprs` (resolver.go:291-295).
    fn walk_exprs(&mut self, list: &mut [Expr]) {
        for x in list {
            self.walk_expr(x);
        }
    }

    /// Go's `walkLHS` (resolver.go:297-304): identifiers on the left-hand
    /// side of `:=` are skipped (they are declared, not resolved).
    fn walk_lhs(&mut self, list: &mut [Expr]) {
        for expr in list {
            // (Go: expr = ast.Unparen(expr); if not an identifier and not
            // nil, walk it. The parens themselves never hold identifiers.)
            let mut cur = expr;
            while let Expr::ParenExpr(pe) = cur {
                cur = &mut pe.x;
            }
            if !matches!(cur, Expr::Ident(_)) {
                self.walk_expr(cur);
            }
        }
    }

    /// Go's `walkStmts` (resolver.go:306-310).
    fn walk_stmts(&mut self, list: &mut [Stmt]) {
        for s in list {
            self.walk_stmt(s);
        }
    }

    /// Go's `Visit` case for `*ast.FuncLit` helpers (resolver.go:577-583):
    /// type parameters must be walked separately for FuncDecls.
    fn walk_func_type(&mut self, typ: &mut FuncType) {
        if let Some(p) = &mut typ.params {
            self.resolve_list(p);
        }
        if let Some(r) = &mut typ.results {
            self.resolve_list(r);
        }
        if let Some(p) = &mut typ.params {
            self.declare_list(p, ObjKind::Var);
        }
        if let Some(r) = &mut typ.results {
            self.declare_list(r, ObjKind::Var);
        }
    }

    /// Go's `resolveList` (resolver.go:585-594).
    fn resolve_list(&mut self, list: &mut FieldList) {
        for f in &mut list.list {
            if let Some(t) = &mut f.typ {
                self.walk_expr(t);
            }
        }
    }

    /// Go's `declareList` (resolver.go:596-603).
    fn declare_list(&mut self, list: &mut FieldList, kind: ObjKind) {
        let top = self.top_scope.clone();
        let top = top.as_ref().expect("top scope");
        for f in &mut list.list {
            let decl = ObjectDecl::Field(f.clone());
            self.declare(&decl, None, top, kind, &mut f.names);
        }
    }

    /// Go's `walkFieldList` (resolver.go:651-657).
    fn walk_field_list(&mut self, list: &mut FieldList, kind: ObjKind) {
        self.resolve_list(list);
        self.declare_list(list, kind);
    }

    /// Go's `walkTParams` (resolver.go:659-665): declares type parameters
    /// eagerly so that they may be resolved in the constraint expressions
    /// held in the field Type.
    fn walk_tparams(&mut self, tparams: &mut Option<FieldList>) {
        if let Some(fl) = tparams {
            self.declare_list(fl, ObjKind::Typ);
            self.resolve_list(fl);
        }
    }

    /// Go's `walkBody` (resolver.go:667-674): the body BlockStmt itself
    /// does not contribute a scope.
    fn walk_body(&mut self, body: &mut BlockStmt) {
        self.open_label_scope();
        self.walk_stmts(&mut body.list);
        self.close_label_scope();
    }

    /// Go's `Visit` case for `*ast.FuncLit` bodies etc. - the plain BlockStmt
    /// case (resolver.go:389-392): opens a scope around the statement list.
    fn walk_block_stmt(&mut self, bl: &mut BlockStmt) {
        let pos = bl.pos();
        self.open_scope(pos);
        self.walk_stmts(&mut bl.list);
        self.close_scope();
    }

    /// Go's `Visit` cases for statements (resolver.go:369-509).
    fn walk_stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::LabeledStmt(ls) => {
                // Go: r.declare(n, nil, r.labelScope, ast.Lbl, n.Label)
                let label_scope = self.label_scope.clone().expect("label scope");
                let decl = ObjectDecl::LabeledStmt((**ls).clone());
                self.declare(
                    &decl,
                    None,
                    &label_scope,
                    ObjKind::Lbl,
                    std::slice::from_mut(&mut ls.label),
                );
                self.walk_stmt(&mut ls.stmt);
            }
            Stmt::AssignStmt(as_) => {
                self.walk_exprs(&mut as_.rhs);
                if as_.tok == Token::Define {
                    self.short_var_decl(as_);
                } else {
                    self.walk_exprs(&mut as_.lhs);
                }
            }
            Stmt::BranchStmt(bs) => {
                // add to list of unresolved targets
                if bs.tok != Token::FallThrough {
                    if let Some(label) = &mut bs.label {
                        let n = self.target_stack.len() - 1;
                        self.target_stack[n].push(label.clone());
                    }
                }
            }
            Stmt::BlockStmt(bl) => self.walk_block_stmt(bl),
            Stmt::IfStmt(if_) => {
                let pos = if_.pos();
                self.open_scope(pos);
                if let Some(init) = &mut if_.init {
                    self.walk_stmt(init);
                }
                self.walk_expr(&mut if_.cond);
                self.walk_block_stmt(&mut if_.body);
                if let Some(else_) = &mut if_.else_ {
                    self.walk_stmt(else_);
                }
                self.close_scope();
            }
            Stmt::CaseClause(cc) => {
                self.walk_exprs(&mut cc.list);
                let pos = cc.pos();
                self.open_scope(pos);
                self.walk_stmts(&mut cc.body);
                self.close_scope();
            }
            Stmt::SwitchStmt(sw) => {
                let pos = sw.pos();
                self.open_scope(pos);
                if let Some(init) = &mut sw.init {
                    self.walk_stmt(init);
                }
                let has_init = sw.init.is_some();
                if let Some(tag) = &mut sw.tag {
                    // The scope below reproduces some unnecessary behavior of
                    // the parser, opening an extra scope in case this is a
                    // type switch. It's not needed for expression switches.
                    // (Go's TODO: remove this once we've matched the parser
                    // resolution exactly.)
                    if has_init {
                        let tag_pos = tag.pos();
                        self.open_scope(tag_pos);
                        self.walk_expr(tag);
                        self.close_scope();
                    } else {
                        self.walk_expr(tag);
                    }
                }
                // s.Body consists only of case clauses, so does not get its
                // own scope.
                self.walk_stmts(&mut sw.body.list);
                self.close_scope();
            }
            Stmt::TypeSwitchStmt(ts) => {
                // (Go closes the Init and Assign scopes with deferred calls
                // at the end of the case: the Assign scope therefore stays
                // open while the case clauses in the body are walked, and
                // the Init scope nests around both when present.)
                let ts_pos = ts.pos();
                let init_open = ts.init.is_some();
                if init_open {
                    self.open_scope(ts_pos);
                }
                if let Some(init) = &mut ts.init {
                    self.walk_stmt(init);
                }
                let assign_pos = ts.assign.pos();
                self.open_scope(assign_pos);
                self.walk_stmt(&mut ts.assign);
                // s.Body consists only of case clauses, so does not get its
                // own scope.
                self.walk_stmts(&mut ts.body.list);
                self.close_scope();
                if init_open {
                    self.close_scope();
                }
            }
            Stmt::CommClause(cc) => {
                let pos = cc.pos();
                self.open_scope(pos);
                if let Some(comm) = &mut cc.comm {
                    self.walk_stmt(comm);
                }
                self.walk_stmts(&mut cc.body);
                self.close_scope();
            }
            Stmt::SelectStmt(se) => {
                // as for switch statements, select statement bodies don't
                // get their own scope.
                self.walk_stmts(&mut se.body.list);
            }
            Stmt::ForStmt(f) => {
                let pos = f.pos();
                self.open_scope(pos);
                if let Some(init) = &mut f.init {
                    self.walk_stmt(init);
                }
                if let Some(cond) = &mut f.cond {
                    self.walk_expr(cond);
                }
                if let Some(post) = &mut f.post {
                    self.walk_stmt(post);
                }
                self.walk_block_stmt(&mut f.body);
                self.close_scope();
            }
            Stmt::RangeStmt(r) => self.walk_range_stmt(r),
            Stmt::DeclStmt(ds) => self.walk_decl(&mut ds.decl),
            // generic statement children
            Stmt::ExprStmt(es) => self.walk_expr(&mut es.x),
            Stmt::SendStmt(sd) => {
                self.walk_expr(&mut sd.chan_);
                self.walk_expr(&mut sd.value);
            }
            Stmt::IncDecStmt(id) => self.walk_expr(&mut id.x),
            Stmt::GoStmt(gs) => self.walk_call_expr(&mut gs.call),
            Stmt::DeferStmt(ds) => self.walk_call_expr(&mut ds.call),
            Stmt::ReturnStmt(rs) => self.walk_exprs(&mut rs.results),
            Stmt::BadStmt(_) | Stmt::EmptyStmt(_) => {}
        }
    }

    /// CallExpr children (fun, args) - used by the generic GoStmt/DeferStmt
    /// descent.
    fn walk_call_expr(&mut self, call: &mut CallExpr) {
        self.walk_expr(&mut call.fun);
        for a in &mut call.args {
            self.walk_expr(a);
        }
    }

    /// Go's `Visit` case for `*ast.RangeStmt` (resolver.go:477-509).
    fn walk_range_stmt(&mut self, r: &mut RangeStmt) {
        let pos = r.pos();
        self.open_scope(pos);
        self.walk_expr(&mut r.x);

        // Move the key/value out of the AST so that the `:=` declaration
        // machinery can own them (Go builds a temporary *ast.AssignStmt over
        // the same nodes).
        let mut lhs: Vec<Expr> = Vec::new();
        if let Some(key) = r.key.take() {
            lhs.push(key);
        }
        if let Some(value) = r.value.take() {
            lhs.push(value);
        }
        if !lhs.is_empty() {
            let mut lhs = lhs;
            if r.tok == Token::Define {
                // Note: we can't exactly match the behavior of object
                // resolution during the parsing pass here, as it uses the
                // position of the RANGE token for the RHS OpPos. That
                // information is not contained within the AST. (Go comment)
                let mut as_ = AssignStmt {
                    lhs,
                    tok_pos: r.tok_pos,
                    tok: Token::Define,
                    rhs: vec![Expr::UnaryExpr(Box::new(UnaryExpr {
                        op_pos: r.range,
                        op: Token::Range,
                        x: r.x.clone(),
                    }))],
                };
                self.walk_lhs(&mut as_.lhs);
                self.short_var_decl(&mut as_);
                lhs = as_.lhs;
            } else {
                self.walk_exprs(&mut lhs);
            }
            // Put the (possibly resolved/declared) key/value back.
            let mut it = lhs.into_iter();
            r.key = it.next();
            r.value = it.next();
        }
        self.walk_block_stmt(&mut r.body);
        self.close_scope();
    }

    /// Go's `Visit` case for declarations (resolver.go:511-568).
    fn walk_decl(&mut self, d: &mut Decl) {
        match d {
            Decl::GenDecl(gd) => {
                match gd.tok {
                    Token::Const | Token::Var => {
                        let kind = if gd.tok == Token::Var {
                            ObjKind::Var
                        } else {
                            ObjKind::Con
                        };
                        for (i, spec) in gd.specs.iter_mut().enumerate() {
                            if let Spec::ValueSpec(vs) = spec {
                                self.walk_exprs(&mut vs.values);
                                if let Some(t) = &mut vs.typ {
                                    self.walk_expr(t);
                                }
                                let decl = ObjectDecl::ValueSpec(vs.clone());
                                let top = self.top_scope.clone();
                                let top = top.as_ref().expect("top scope");
                                self.declare(&decl, Some(i as i64), top, kind, &mut vs.names);
                            }
                        }
                    }
                    Token::Type => {
                        // (Go closes each spec's type-parameter scope with a
                        // `defer` inside the loop, so all such scopes stay
                        // open until the whole case ends and are then closed
                        // in reverse order - meaning the scope of a typed
                        // spec still covers the following specs. This is
                        // reproduced by collecting the closes.)
                        let mut open_tparam_scopes = 0;
                        for spec in &mut gd.specs {
                            if let Spec::TypeSpec(ts) = spec {
                                // Go spec: The scope of a type identifier
                                // declared inside a function begins at the
                                // identifier in the TypeSpec and ends at the
                                // end of the innermost containing block.
                                let decl = ObjectDecl::TypeSpec(ts.clone());
                                let top = self.top_scope.clone();
                                let top = top.as_ref().expect("top scope");
                                self.declare(
                                    &decl,
                                    None,
                                    top,
                                    ObjKind::Typ,
                                    std::slice::from_mut(&mut ts.name),
                                );
                                if ts.type_params.is_some() {
                                    let spec_pos = ts.pos();
                                    self.open_scope(spec_pos);
                                    open_tparam_scopes += 1;
                                    self.walk_tparams(&mut ts.type_params);
                                }
                                self.walk_expr(&mut ts.typ);
                            }
                        }
                        for _ in 0..open_tparam_scopes {
                            self.close_scope();
                        }
                    }
                    Token::Import => {
                        // (Go's resolver does nothing for import
                        // declarations: the whole subtree is skipped.)
                    }
                    _ => unreachable!("GenDecl.Tok must be IMPORT, CONST, TYPE, or VAR"),
                }
            }
            Decl::FuncDecl(fd) => {
                // Open the function scope.
                let pos = fd.pos();
                self.open_scope(pos);

                self.walk_recv(&mut fd.recv);

                // Type parameters are walked normally: they can reference
                // each other, and can be referenced by normal parameters.
                self.walk_tparams(&mut fd.typ.type_params);

                // Resolve and declare parameters in a specific order to get
                // duplicate declaration errors in the correct location.
                if let Some(params) = &mut fd.typ.params {
                    self.resolve_list(params);
                }
                if let Some(results) = &mut fd.typ.results {
                    self.resolve_list(results);
                }
                if let Some(recv) = &mut fd.recv {
                    self.declare_list(recv, ObjKind::Var);
                }
                if let Some(params) = &mut fd.typ.params {
                    self.declare_list(params, ObjKind::Var);
                }
                if let Some(results) = &mut fd.typ.results {
                    self.declare_list(results, ObjKind::Var);
                }

                if let Some(body) = &mut fd.body {
                    self.walk_body(body);
                }
                if fd.recv.is_none() && fd.name.name != "init" {
                    let decl = ObjectDecl::FuncDecl(fd.clone());
                    let pkg = self.pkg_scope.clone();
                    self.declare(
                        &decl,
                        None,
                        &pkg,
                        ObjKind::Fun,
                        std::slice::from_mut(&mut fd.name),
                    );
                }
                self.close_scope();
            }
            Decl::BadDecl(_) => {}
        }
    }

    /// Go's `walkRecv` (resolver.go:605-649).
    fn walk_recv(&mut self, recv: &mut Option<FieldList>) {
        // If our receiver has receiver type parameters, we must declare them
        // before trying to resolve the rest of the receiver, and avoid
        // re-resolving the type parameter identifiers.
        let Some(fl) = recv else { return };
        if fl.list.is_empty() {
            return; // nothing to do
        }

        // Split off the first field (its type holds the receiver type
        // parameters).
        let (first, rest) = fl.list.split_at_mut(1);
        let first_field = &mut first[0];
        let mut typ_opt = first_field.typ.take();

        // Peel one level of pointer: Go's `if ptr, ok := typ.(*ast.StarExpr)`.
        let base: &mut Expr;
        match typ_opt.as_mut() {
            Some(Expr::StarExpr(star)) => base = &mut star.x,
            Some(e) => base = e,
            None => {
                // no receiver type at all; nothing to declare or resolve
                first_field.typ = typ_opt;
                return;
            }
        }

        match base {
            Expr::IndexExpr(ix) => {
                let index_is_ident = matches!(ix.index, Expr::Ident(_));
                if index_is_ident {
                    if let Expr::Ident(id) = &mut ix.index {
                        // The receiver type parameter identifiers are written
                        // to the scope, but never set as the resolved object.
                        // See go.dev/issue/50956.
                        let decl = ObjectDecl::Ident(id.clone());
                        let top = self.top_scope.clone();
                        let top = top.as_ref().expect("top scope");
                        self.declare(&decl, None, top, ObjKind::Typ, std::slice::from_mut(id));
                    }
                }
                // (Go resolves X first, then the invalid type parameter
                // expression, if any.)
                self.walk_expr(&mut ix.x);
                if !index_is_ident {
                    self.walk_expr(&mut ix.index);
                }
            }
            Expr::IndexListExpr(il) => {
                // Declare all receiver type parameters first...
                for idx in il.indices.iter_mut() {
                    if let Expr::Ident(id) = idx {
                        let decl = ObjectDecl::Ident(id.clone());
                        let top = self.top_scope.clone();
                        let top = top.as_ref().expect("top scope");
                        self.declare(&decl, None, top, ObjKind::Typ, std::slice::from_mut(id));
                    }
                }
                // ...then resolve X and any invalid type parameter
                // expressions.
                self.walk_expr(&mut il.x);
                for idx in &mut il.indices {
                    if !matches!(idx, Expr::Ident(_)) {
                        self.walk_expr(idx);
                    }
                }
            }
            other => self.walk_expr(other),
        }

        // The receiver is invalid, but try to resolve it anyway for
        // consistency (the remaining fields).
        for f in rest {
            if let Some(t) = &mut f.typ {
                self.walk_expr(t);
            }
        }
        first_field.typ = typ_opt;
    }
}

/// Resolves the identifiers of a parsed file (Go's `resolveFile`,
/// resolver.go:83-117). `handle` is used to render positions in message
/// text; `decl_err`, when given, receives declaration errors (Go wires
/// `parser.error` here, gated on DeclarationErrors).
///
/// A file with `scope` already set is left untouched (Go's `ResolveFile`
/// guard; repeated calls are no-ops after success).
pub(crate) fn resolve_file(
    file: &mut crate::ast::File,
    handle: Rc<TokenFile>,
    decl_err: Option<&dyn Fn(Pos, String)>,
) {
    if file.scope.is_some() {
        return; // already resolved
    }

    let pkg_scope = Rc::new(Scope::new_scope(None));
    let mut r = Resolver {
        handle,
        decl_err,
        pkg_scope: pkg_scope.clone(),
        top_scope: Some(pkg_scope.clone()),
        unresolved: Vec::new(),
        depth: 1,
        label_scope: None,
        target_stack: Vec::new(),
        label_resolutions: Vec::new(),
        sentinel: Rc::new(Object::new_obj(ObjKind::Bad, "")),
    };

    for decl in &mut file.decls {
        r.walk_decl(decl);
    }

    r.close_scope();
    assert_(r.top_scope.is_none(), "unbalanced scopes");
    assert_(r.label_scope.is_none(), "unbalanced label scopes");

    // resolve global identifiers within the same file
    let mut unresolved: Vec<Ident> = Vec::new();
    for ident in r.unresolved.drain(..) {
        // (Go asserts ident.Obj == sentinel here and patches the object
        // through the retained pointer; the tree patch happens below in
        // `finalize_*`.)
        if r.pkg_scope.lookup(&ident.name).is_none() {
            unresolved.push(Ident {
                name_pos: ident.name_pos,
                name: ident.name,
                obj: None,
            });
        }
    }

    // Patch the tree: identifiers still marked with the unresolved sentinel
    // get the package-scope lookup result; label references get their label
    // object (by the position of the referencing identifier).
    let labels: BTreeMap<Pos, Rc<Object>> = r.label_resolutions.into_iter().collect();
    finalize_decls(&mut file.decls, &r.sentinel, &r.pkg_scope, &labels);

    file.scope = Some(pkg_scope);
    file.unresolved = unresolved;
}

// ----------------------------------------------------------------------------
// Finalize layer
//
// A second &mut pass over the tree, touching only the Ident.obj fields. It
// mirrors the children() order of walk.rs so that it cannot drift from the
// dispatch layer; the tests (TestUnresolved, TestResolution) lock the
// behavior.

use std::collections::BTreeMap;

fn finalize_ident(
    id: &mut Ident,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    if let Some(obj) = id.obj.as_ref() {
        if Rc::ptr_eq(obj, sentinel) {
            // (Go also removes the sentinel here; a failed lookup leaves
            // Obj == nil.)
            id.obj = pkg.lookup(&id.name);
        }
    }
    if let Some(obj) = labels.get(&id.name_pos) {
        id.obj = Some(obj.clone());
    }
}

fn finalize_expr(
    x: &mut Expr,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    match x {
        Expr::Ident(id) => finalize_ident(id, sentinel, pkg, labels),
        Expr::FuncLit(fl) => {
            finalize_func_type(&mut fl.typ, sentinel, pkg, labels);
            finalize_block(&mut fl.body, sentinel, pkg, labels);
        }
        Expr::SelectorExpr(se) => {
            finalize_expr(&mut se.x, sentinel, pkg, labels);
            finalize_ident(&mut se.sel, sentinel, pkg, labels);
        }
        Expr::StructType(st) => {
            if let Some(fields) = &mut st.fields {
                finalize_field_list(fields, sentinel, pkg, labels);
            }
        }
        Expr::FuncType(ft) => finalize_func_type(ft, sentinel, pkg, labels),
        Expr::CompositeLit(cl) => {
            if let Some(t) = &mut cl.typ {
                finalize_expr(t, sentinel, pkg, labels);
            }
            for e in &mut cl.elts {
                finalize_expr(e, sentinel, pkg, labels);
            }
        }
        Expr::InterfaceType(it) => {
            if let Some(methods) = &mut it.methods {
                finalize_field_list(methods, sentinel, pkg, labels);
            }
        }
        Expr::ParenExpr(pe) => finalize_expr(&mut pe.x, sentinel, pkg, labels),
        Expr::IndexExpr(ix) => {
            finalize_expr(&mut ix.x, sentinel, pkg, labels);
            finalize_expr(&mut ix.index, sentinel, pkg, labels);
        }
        Expr::IndexListExpr(il) => {
            finalize_expr(&mut il.x, sentinel, pkg, labels);
            for i in &mut il.indices {
                finalize_expr(i, sentinel, pkg, labels);
            }
        }
        Expr::SliceExpr(sl) => {
            finalize_expr(&mut sl.x, sentinel, pkg, labels);
            if let Some(low) = &mut sl.low {
                finalize_expr(low, sentinel, pkg, labels);
            }
            if let Some(high) = &mut sl.high {
                finalize_expr(high, sentinel, pkg, labels);
            }
            if let Some(max) = &mut sl.max {
                finalize_expr(max, sentinel, pkg, labels);
            }
        }
        Expr::TypeAssertExpr(ta) => {
            finalize_expr(&mut ta.x, sentinel, pkg, labels);
            if let Some(t) = &mut ta.typ {
                finalize_expr(t, sentinel, pkg, labels);
            }
        }
        Expr::CallExpr(cl) => {
            finalize_expr(&mut cl.fun, sentinel, pkg, labels);
            for a in &mut cl.args {
                finalize_expr(a, sentinel, pkg, labels);
            }
        }
        Expr::StarExpr(se) => finalize_expr(&mut se.x, sentinel, pkg, labels),
        Expr::UnaryExpr(ue) => finalize_expr(&mut ue.x, sentinel, pkg, labels),
        Expr::BinaryExpr(be) => {
            finalize_expr(&mut be.x, sentinel, pkg, labels);
            finalize_expr(&mut be.y, sentinel, pkg, labels);
        }
        Expr::KeyValueExpr(kv) => {
            finalize_expr(&mut kv.key, sentinel, pkg, labels);
            finalize_expr(&mut kv.value, sentinel, pkg, labels);
        }
        Expr::Ellipsis(ell) => {
            if let Some(elt) = &mut ell.elt {
                finalize_expr(elt, sentinel, pkg, labels);
            }
        }
        Expr::ArrayType(at) => {
            if let Some(len) = &mut at.len {
                finalize_expr(len, sentinel, pkg, labels);
            }
            finalize_expr(&mut at.elt, sentinel, pkg, labels);
        }
        Expr::MapType(mt) => {
            finalize_expr(&mut mt.key, sentinel, pkg, labels);
            finalize_expr(&mut mt.value, sentinel, pkg, labels);
        }
        Expr::ChanType(ct) => finalize_expr(&mut ct.value, sentinel, pkg, labels),
        Expr::BadExpr(_) | Expr::BasicLit(_) => {}
    }
}

fn finalize_field(
    f: &mut Field,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    for n in &mut f.names {
        finalize_ident(n, sentinel, pkg, labels);
    }
    if let Some(t) = &mut f.typ {
        finalize_expr(t, sentinel, pkg, labels);
    }
}

fn finalize_field_list(
    fl: &mut FieldList,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    for f in &mut fl.list {
        finalize_field(f, sentinel, pkg, labels);
    }
}

fn finalize_func_type(
    ft: &mut FuncType,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    if let Some(tp) = &mut ft.type_params {
        finalize_field_list(tp, sentinel, pkg, labels);
    }
    if let Some(p) = &mut ft.params {
        finalize_field_list(p, sentinel, pkg, labels);
    }
    if let Some(r) = &mut ft.results {
        finalize_field_list(r, sentinel, pkg, labels);
    }
}

fn finalize_block(
    b: &mut BlockStmt,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    for s in &mut b.list {
        finalize_stmt(s, sentinel, pkg, labels);
    }
}

fn finalize_stmt(
    s: &mut Stmt,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    match s {
        Stmt::LabeledStmt(ls) => {
            finalize_ident(&mut ls.label, sentinel, pkg, labels);
            finalize_stmt(&mut ls.stmt, sentinel, pkg, labels);
        }
        Stmt::AssignStmt(as_) => {
            for x in &mut as_.lhs {
                finalize_expr(x, sentinel, pkg, labels);
            }
            for y in &mut as_.rhs {
                finalize_expr(y, sentinel, pkg, labels);
            }
        }
        Stmt::BranchStmt(bs) => {
            if let Some(label) = &mut bs.label {
                finalize_ident(label, sentinel, pkg, labels);
            }
        }
        Stmt::BlockStmt(bl) => finalize_block(bl, sentinel, pkg, labels),
        Stmt::IfStmt(if_) => {
            if let Some(init) = &mut if_.init {
                finalize_stmt(init, sentinel, pkg, labels);
            }
            finalize_expr(&mut if_.cond, sentinel, pkg, labels);
            finalize_block(&mut if_.body, sentinel, pkg, labels);
            if let Some(else_) = &mut if_.else_ {
                finalize_stmt(else_, sentinel, pkg, labels);
            }
        }
        Stmt::CaseClause(cc) => {
            for x in &mut cc.list {
                finalize_expr(x, sentinel, pkg, labels);
            }
            for s2 in &mut cc.body {
                finalize_stmt(s2, sentinel, pkg, labels);
            }
        }
        Stmt::SwitchStmt(sw) => {
            if let Some(init) = &mut sw.init {
                finalize_stmt(init, sentinel, pkg, labels);
            }
            if let Some(tag) = &mut sw.tag {
                finalize_expr(tag, sentinel, pkg, labels);
            }
            finalize_block(&mut sw.body, sentinel, pkg, labels);
        }
        Stmt::TypeSwitchStmt(ts) => {
            if let Some(init) = &mut ts.init {
                finalize_stmt(init, sentinel, pkg, labels);
            }
            finalize_stmt(&mut ts.assign, sentinel, pkg, labels);
            finalize_block(&mut ts.body, sentinel, pkg, labels);
        }
        Stmt::CommClause(cc) => {
            if let Some(comm) = &mut cc.comm {
                finalize_stmt(comm, sentinel, pkg, labels);
            }
            for s2 in &mut cc.body {
                finalize_stmt(s2, sentinel, pkg, labels);
            }
        }
        Stmt::SelectStmt(se) => finalize_block(&mut se.body, sentinel, pkg, labels),
        Stmt::ForStmt(f) => {
            if let Some(init) = &mut f.init {
                finalize_stmt(init, sentinel, pkg, labels);
            }
            if let Some(cond) = &mut f.cond {
                finalize_expr(cond, sentinel, pkg, labels);
            }
            if let Some(post) = &mut f.post {
                finalize_stmt(post, sentinel, pkg, labels);
            }
            finalize_block(&mut f.body, sentinel, pkg, labels);
        }
        Stmt::RangeStmt(r) => {
            if let Some(key) = &mut r.key {
                finalize_expr(key, sentinel, pkg, labels);
            }
            if let Some(value) = &mut r.value {
                finalize_expr(value, sentinel, pkg, labels);
            }
            finalize_expr(&mut r.x, sentinel, pkg, labels);
            finalize_block(&mut r.body, sentinel, pkg, labels);
        }
        Stmt::DeclStmt(ds) => finalize_decl(&mut ds.decl, sentinel, pkg, labels),
        Stmt::ExprStmt(es) => finalize_expr(&mut es.x, sentinel, pkg, labels),
        Stmt::SendStmt(sd) => {
            finalize_expr(&mut sd.chan_, sentinel, pkg, labels);
            finalize_expr(&mut sd.value, sentinel, pkg, labels);
        }
        Stmt::IncDecStmt(id) => finalize_expr(&mut id.x, sentinel, pkg, labels),
        Stmt::GoStmt(gs) => {
            finalize_expr(&mut gs.call.fun, sentinel, pkg, labels);
            for a in &mut gs.call.args {
                finalize_expr(a, sentinel, pkg, labels);
            }
        }
        Stmt::DeferStmt(ds) => {
            finalize_expr(&mut ds.call.fun, sentinel, pkg, labels);
            for a in &mut ds.call.args {
                finalize_expr(a, sentinel, pkg, labels);
            }
        }
        Stmt::ReturnStmt(rs) => {
            for x in &mut rs.results {
                finalize_expr(x, sentinel, pkg, labels);
            }
        }
        Stmt::BadStmt(_) | Stmt::EmptyStmt(_) => {}
    }
}

fn finalize_decl(
    d: &mut Decl,
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    match d {
        Decl::GenDecl(gd) => {
            for spec in &mut gd.specs {
                match spec {
                    Spec::ImportSpec(is) => {
                        if let Some(name) = &mut is.name {
                            finalize_ident(name, sentinel, pkg, labels);
                        }
                    }
                    Spec::ValueSpec(vs) => {
                        for n in &mut vs.names {
                            finalize_ident(n, sentinel, pkg, labels);
                        }
                        if let Some(t) = &mut vs.typ {
                            finalize_expr(t, sentinel, pkg, labels);
                        }
                        for v in &mut vs.values {
                            finalize_expr(v, sentinel, pkg, labels);
                        }
                    }
                    Spec::TypeSpec(ts) => {
                        finalize_ident(&mut ts.name, sentinel, pkg, labels);
                        if let Some(tp) = &mut ts.type_params {
                            finalize_field_list(tp, sentinel, pkg, labels);
                        }
                        finalize_expr(&mut ts.typ, sentinel, pkg, labels);
                    }
                }
            }
        }
        Decl::FuncDecl(fd) => {
            if let Some(recv) = &mut fd.recv {
                finalize_field_list(recv, sentinel, pkg, labels);
            }
            finalize_ident(&mut fd.name, sentinel, pkg, labels);
            finalize_func_type(&mut fd.typ, sentinel, pkg, labels);
            if let Some(body) = &mut fd.body {
                finalize_block(body, sentinel, pkg, labels);
            }
        }
        Decl::BadDecl(_) => {}
    }
}

fn finalize_decls(
    decls: &mut [Decl],
    sentinel: &Rc<Object>,
    pkg: &Rc<Scope>,
    labels: &BTreeMap<Pos, Rc<Object>>,
) {
    for d in decls {
        finalize_decl(d, sentinel, pkg, labels);
    }
}
