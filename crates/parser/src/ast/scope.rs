//! This module implements scopes and the objects they contain.
//!
//! Ported from Go's standard `go/ast/scope.go` package (the deprecated
//! syntactic scope/object machinery, kept for structural compatibility with
//! `go/ast`). Deprecated in favor of the type checker; nothing in this round
//! populates [`Object::decl`]/[`Object::data`].

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::token::{NoPos, Pos};

use super::ast::{AssignStmt, Expr, Field, FuncDecl, ImportSpec, LabeledStmt, TypeSpec, ValueSpec};

/// A Scope maintains the set of named language entities declared
/// in the scope and a link to the immediately surrounding (outer)
/// scope.
///
/// Deprecated: use the type checker instead; see [`Object`].
#[derive(Clone, Debug)]
pub struct Scope {
    pub outer: Option<Rc<Scope>>,
    pub objects: BTreeMap<String, Rc<Object>>,
}

impl Scope {
    /// Creates a new scope nested in the outer scope.
    pub fn new_scope(outer: Option<Rc<Scope>>) -> Scope {
        Scope {
            outer,
            objects: BTreeMap::new(),
        }
    }

    /// Returns the object with the given name if it is found in scope `s`,
    /// otherwise it returns `None`. Outer scopes are ignored.
    pub fn lookup(&self, name: &str) -> Option<Rc<Object>> {
        self.objects.get(name).cloned()
    }

    /// Attempts to insert a named object `obj` into the scope.
    /// If the scope already contains an object `alt` with the same name,
    /// Insert leaves the scope unchanged and returns `alt`. Otherwise
    /// it inserts `obj` and returns `None`.
    pub fn insert(&mut self, obj: Rc<Object>) -> Option<Rc<Object>> {
        if let Some(alt) = self.objects.get(&obj.name) {
            return Some(alt.clone());
        }
        self.objects.insert(obj.name.clone(), obj);
        None
    }
}

// Debugging support.
//
// Go prints "scope %p { ... }" with the raw pointer; Rust has no pointer
// formatting for values held by `Rc`, so a stable placeholder is used.
// Objects are printed sorted by name (Go's map iteration order is random).
impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scope @<no-address> {{")?;
        if !self.objects.is_empty() {
            writeln!(f)?;
            for obj in self.objects.values() {
                writeln!(f, "\t{} {}\n", obj.kind, obj.name)?;
            }
        }
        write!(f, "}}\n")
    }
}

/// An Object describes a named language entity such as a package,
/// constant, type, variable, function (incl. methods), or label.
///
/// The Data fields contains object-specific data:
///
/// | Kind | Data type | Data value            |
/// |------|-----------|-----------------------|
/// | Pkg  | Scope     | package scope         |
/// | Con  | i64       | iota for the decl     |
///
/// Deprecated: The relationship between Idents and Objects cannot be
/// correctly computed without type information. New programs should set the
/// go/parser `SkipObjectResolution` flag instead.
///
/// Note: Go's `Decl`/`Data`/`Type` fields are `any`. Only the closed set of
/// types go/ast itself ever stores is represented here (see [`ObjectDecl`],
/// [`ObjectData`]). Go's `Type` field is never read by go/ast itself and is
/// deferred to the resolve.go port round.
#[derive(Clone, Debug)]
pub struct Object {
    pub kind: ObjKind,
    pub name: String, // declared name
    pub decl: Option<ObjectDecl>,
    pub data: Option<ObjectData>,
}

/// The closed set of Go's `Object.Decl any` values: the corresponding Field,
/// XxxSpec, FuncDecl, LabeledStmt, AssignStmt, or Scope; or nil.
#[derive(Clone, Debug)]
pub enum ObjectDecl {
    Field(Field),
    ImportSpec(ImportSpec),
    ValueSpec(ValueSpec),
    TypeSpec(TypeSpec),
    FuncDecl(FuncDecl),
    LabeledStmt(LabeledStmt),
    AssignStmt(AssignStmt),
    Scope(Rc<Scope>),
}

/// The closed set of Go's `Object.Data any` values.
#[derive(Clone, Debug)]
pub enum ObjectData {
    /// Kind == ObjKind::Pkg.
    PkgScope(Rc<Scope>),
    /// Kind == ObjKind::Con: the iota value for the respective declaration.
    Iota(i64),
}

impl Object {
    /// Creates a new object of a given kind and name.
    pub fn new_obj(kind: ObjKind, name: impl Into<String>) -> Object {
        Object {
            kind,
            name: name.into(),
            decl: None,
            data: None,
        }
    }

    /// Computes the source position of the declaration of an object name.
    /// The result may be an invalid position if it cannot be computed
    /// (decl may be nil or not correct).
    pub fn pos(&self) -> Pos {
        let name = &self.name;
        match &self.decl {
            Some(ObjectDecl::Field(f)) => {
                for n in &f.names {
                    if &n.name == name {
                        return n.name_pos;
                    }
                }
            }
            Some(ObjectDecl::ImportSpec(s)) => {
                if let Some(n) = &s.name {
                    if &n.name == name {
                        return n.name_pos;
                    }
                }
                return s.path.pos();
            }
            Some(ObjectDecl::ValueSpec(s)) => {
                for n in &s.names {
                    if &n.name == name {
                        return n.name_pos;
                    }
                }
            }
            Some(ObjectDecl::TypeSpec(s)) => {
                if &s.name.name == name {
                    return s.name.name_pos;
                }
            }
            Some(ObjectDecl::FuncDecl(d)) => {
                if &d.name.name == name {
                    return d.name.name_pos;
                }
            }
            Some(ObjectDecl::LabeledStmt(s)) => {
                if &s.label.name == name {
                    return s.label.name_pos;
                }
            }
            Some(ObjectDecl::AssignStmt(s)) => {
                for x in &s.lhs {
                    if let Expr::Ident(id) = x {
                        if &id.name == name {
                            return id.name_pos;
                        }
                    }
                }
            }
            Some(ObjectDecl::Scope(_)) => {
                // predeclared object - nothing to do for now
            }
            None => {}
        }
        NoPos
    }
}

/// ObjKind describes what an [`Object`] represents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjKind {
    /// for error handling
    Bad,
    /// package
    Pkg,
    /// constant
    Con,
    /// type
    Typ,
    /// variable
    Var,
    /// function or method
    Fun,
    /// label
    Lbl,
}

// objKindStrings
impl fmt::Display for ObjKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ObjKind::Bad => "bad",
            ObjKind::Pkg => "package",
            ObjKind::Con => "const",
            ObjKind::Typ => "type",
            ObjKind::Var => "var",
            ObjKind::Fun => "func",
            ObjKind::Lbl => "label",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(name: &str) -> Rc<Object> {
        Rc::new(Object::new_obj(ObjKind::Var, name))
    }

    #[test]
    fn obj_kind_display() {
        let table = [
            (ObjKind::Bad, "bad"),
            (ObjKind::Pkg, "package"),
            (ObjKind::Con, "const"),
            (ObjKind::Typ, "type"),
            (ObjKind::Var, "var"),
            (ObjKind::Fun, "func"),
            (ObjKind::Lbl, "label"),
        ];
        for (kind, want) in table {
            assert_eq!(kind.to_string(), want);
        }
    }

    #[test]
    fn scope_lookup_insert() {
        let mut outer_scope = Scope::new_scope(None);
        outer_scope.objects.insert("y".into(), obj("y"));
        let outer = Rc::new(outer_scope);
        let mut s = Scope::new_scope(Some(outer.clone()));

        // Lookup in an empty scope returns None.
        assert!(s.lookup("x").is_none());

        // Insert a new object.
        let x = obj("x");
        assert!(s.insert(x.clone()).is_none());
        assert!(Rc::ptr_eq(&s.lookup("x").unwrap(), &x));

        // Inserting a duplicate leaves the scope unchanged and returns alt.
        let x2 = obj("x");
        let alt = s.insert(x2).unwrap();
        assert!(Rc::ptr_eq(&alt, &x));

        // Lookup only considers this scope; outer scopes are ignored.
        assert!(s.lookup("y").is_none());
        assert!(outer.lookup("y").is_some());
    }

    #[test]
    fn object_pos_with_decls() {
        // Pos is NoPos when decl is nil.
        let o = Object::new_obj(ObjKind::Var, "x");
        assert_eq!(o.pos(), NoPos);

        // AssignStmt decl: position of the matching Ident on the lhs.
        let id = crate::ast::Ident {
            name_pos: Pos::from_int(10),
            name: "x".into(),
            obj: None,
        };
        let mut o = Object::new_obj(ObjKind::Var, "x");
        o.decl = Some(ObjectDecl::AssignStmt(AssignStmt {
            lhs: vec![Expr::Ident(id.clone())],
            tok_pos: NoPos,
            tok: crate::token::Token::Assign,
            rhs: vec![],
        }));
        assert_eq!(o.pos(), Pos::from_int(10));

        // ImportSpec decl: pos of path when the name doesn't match...
        let mut o = Object::new_obj(ObjKind::Pkg, "other");
        o.decl = Some(ObjectDecl::ImportSpec(ImportSpec {
            name: None,
            path: crate::ast::BasicLit {
                value_pos: Pos::from_int(30),
                value_end: NoPos,
                kind: crate::token::Token::String,
                value: "\"x\"".into(),
            },
        }));
        assert_eq!(o.pos(), Pos::from_int(30));
    }
}
