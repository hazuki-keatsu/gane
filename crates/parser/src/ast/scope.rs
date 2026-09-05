//! This module implements scopes and the objects they contain.
//!
//! Ported from Go's standard `go/ast/scope.go` package (the deprecated
//! syntactic scope/object machinery, kept for structural compatibility with
//! `go/ast`). Deprecated in favor of the type checker.
//!
//! `parser::resolver` (go/parser/resolver.go) populates
//! [`Object::decl`]/[`Object::data`] and [`Ident::obj`] during deprecated
//! identifier resolution. Go's `Object.Decl` points at the very node that
//! lives inside the file's declaration list; in this port the AST is
//! owned-by-value, so `Object::decl` holds a by-value copy made at
//! declaration time (it is never updated afterwards, which also keeps the
//! `Scope`/`Object`/`decl` graph free of cycles).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::token::{NO_POS, Pos};

use super::ast::{
    AssignStmt, Expr, Field, FuncDecl, Ident, ImportSpec, LabeledStmt, TypeSpec, ValueSpec,
};

/// A Scope maintains the set of named language entities declared
/// in the scope and a link to the immediately surrounding (outer)
/// scope.
///
/// Deprecated: use the type checker instead; see [`Object`].
///
/// The object map is interior-mutable (`RefCell`): scopes are shared through
/// `Rc` (a child scope's `outer` link holds an `Rc` of its parent), and the
/// resolver inserts into a scope that may already be shared - exactly like
/// Go, where scopes are plain pointers and maps are mutated in place.
#[derive(Clone, Debug)]
pub struct Scope {
    pub outer: Option<Rc<Scope>>,
    objects: RefCell<BTreeMap<String, Rc<Object>>>,
}

impl Scope {
    /// Creates a new scope nested in the outer scope.
    pub fn new_scope(outer: Option<Rc<Scope>>) -> Scope {
        Scope {
            outer,
            objects: RefCell::new(BTreeMap::new()),
        }
    }

    /// Returns the object with the given name if it is found in scope `s`,
    /// otherwise it returns `None`. Outer scopes are ignored.
    pub fn lookup(&self, name: &str) -> Option<Rc<Object>> {
        self.objects.borrow().get(name).cloned()
    }

    /// Attempts to insert a named object `obj` into the scope.
    /// If the scope already contains an object `alt` with the same name,
    /// Insert leaves the scope unchanged and returns `alt`. Otherwise
    /// it inserts `obj` and returns `None`.
    pub fn insert(&self, obj: Rc<Object>) -> Option<Rc<Object>> {
        let mut objects = self.objects.borrow_mut();
        if let Some(alt) = objects.get(&obj.name) {
            return Some(alt.clone());
        }
        objects.insert(obj.name.clone(), obj);
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
        let objects = self.objects.borrow();
        if !objects.is_empty() {
            writeln!(f)?;
            for obj in objects.values() {
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
/// XxxSpec, FuncDecl, LabeledStmt, AssignStmt, Scope, or Ident; or nil.
///
/// [`ObjectDecl::Ident`] carries receiver type parameters
/// (go.dev/issue/50956): the resolver writes such objects into scopes but
/// never sets them as the resolved object of an Ident. The values are
/// by-value copies of the nodes in the file's declaration list (see the
/// module documentation).
#[derive(Clone, Debug)]
pub enum ObjectDecl {
    Field(Field),
    ImportSpec(ImportSpec),
    ValueSpec(ValueSpec),
    TypeSpec(TypeSpec),
    FuncDecl(FuncDecl),
    LabeledStmt(LabeledStmt),
    AssignStmt(AssignStmt),
    Ident(Ident),
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
            Some(ObjectDecl::Ident(id)) => {
                if &id.name == name {
                    return id.name_pos;
                }
            }
            Some(ObjectDecl::Scope(_)) => {
                // predeclared object - nothing to do for now
            }
            None => {}
        }
        NO_POS
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
        let outer_scope = Scope::new_scope(None);
        let outer = Rc::new(outer_scope);
        outer.insert(obj("y"));
        let s = Scope::new_scope(Some(outer.clone()));

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
        // Pos is NO_POS when decl is nil.
        let o = Object::new_obj(ObjKind::Var, "x");
        assert_eq!(o.pos(), NO_POS);

        // AssignStmt decl: position of the matching Ident on the lhs.
        let id = crate::ast::Ident {
            name_pos: Pos::from_int(10),
            name: "x".into(),
            obj: None,
        };
        let mut o = Object::new_obj(ObjKind::Var, "x");
        o.decl = Some(ObjectDecl::AssignStmt(AssignStmt {
            lhs: vec![Expr::Ident(id.clone())],
            tok_pos: NO_POS,
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
                value_end: NO_POS,
                kind: crate::token::Token::String,
                value: "\"x\"".into(),
            },
        }));
        assert_eq!(o.pos(), Pos::from_int(30));
    }
}
