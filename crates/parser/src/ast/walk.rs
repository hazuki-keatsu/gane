// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2009 The Go Authors.
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu
//
// Adapted from the Go standard library for Gane.

//! Traversal of ASTs in depth-first order.
//!
//! Ported from Go's standard `go/ast/walk.go`. Go's `Visitor` interface is
//! only ever implemented by the internal `inspector` inside package `ast`, so
//! it is replaced by a closure protocol over [`NodeRef`]:
//!
//! - [`inspect`] calls `f(Some(n))` before entering each node; returning
//!   `false` skips that node's whole subtree (and no trailing `f(None)` is
//!   emitted for it, exactly like Go's inspector returning nil); returning
//!   `true` visits the children and then calls `f(None)` once, recursively.
//!
//! Note: Go's `Walk` dereferences several always-non-nil pointer fields
//! unconditionally (e.g. `StructType.Fields`), which panics on malformed
//! trees. In this port those fields are `Option`s and are simply skipped.

use super::types::*;
use crate::token::{AstNodeId, Pos};

/// A lightweight, reference-based view of any AST node, used to traverse
/// trees uniformly (one variant per Go node struct, mirroring Go's `Node`
/// interface).
#[derive(Clone, Copy, Debug)]
pub enum NodeRef<'a> {
    // Fields
    Field(&'a Field),
    FieldList(&'a FieldList),

    // Expressions and types
    BadExpr(&'a BadExpr),
    Ident(&'a Ident),
    Ellipsis(&'a Ellipsis),
    BasicLit(&'a BasicLit),
    FuncLit(&'a FuncLit),
    CompositeLit(&'a CompositeLit),
    ParenExpr(&'a ParenExpr),
    SelectorExpr(&'a SelectorExpr),
    IndexExpr(&'a IndexExpr),
    IndexListExpr(&'a IndexListExpr),
    SliceExpr(&'a SliceExpr),
    TypeAssertExpr(&'a TypeAssertExpr),
    CallExpr(&'a CallExpr),
    StarExpr(&'a StarExpr),
    UnaryExpr(&'a UnaryExpr),
    BinaryExpr(&'a BinaryExpr),
    KeyValueExpr(&'a KeyValueExpr),
    ArrayType(&'a ArrayType),
    StructType(&'a StructType),
    FuncType(&'a FuncType),
    InterfaceType(&'a InterfaceType),
    MapType(&'a MapType),
    ChanType(&'a ChanType),

    // Statements
    BadStmt(&'a BadStmt),
    DeclStmt(&'a DeclStmt),
    EmptyStmt(&'a EmptyStmt),
    LabeledStmt(&'a LabeledStmt),
    ExprStmt(&'a ExprStmt),
    SendStmt(&'a SendStmt),
    IncDecStmt(&'a IncDecStmt),
    AssignStmt(&'a AssignStmt),
    GoStmt(&'a GoStmt),
    DeferStmt(&'a DeferStmt),
    ReturnStmt(&'a ReturnStmt),
    BranchStmt(&'a BranchStmt),
    BlockStmt(&'a BlockStmt),
    IfStmt(&'a IfStmt),
    CaseClause(&'a CaseClause),
    SwitchStmt(&'a SwitchStmt),
    TypeSwitchStmt(&'a TypeSwitchStmt),
    CommClause(&'a CommClause),
    SelectStmt(&'a SelectStmt),
    ForStmt(&'a ForStmt),
    RangeStmt(&'a RangeStmt),

    // Specs and declarations
    ImportSpec(&'a ImportSpec),
    ValueSpec(&'a ValueSpec),
    TypeSpec(&'a TypeSpec),
    BadDecl(&'a BadDecl),
    GenDecl(&'a GenDecl),
    FuncDecl(&'a FuncDecl),

    // Files and packages
    File(&'a File),
    Package(&'a Package),
}

impl<'a> From<&'a Expr> for NodeRef<'a> {
    fn from(e: &'a Expr) -> Self {
        match e {
            Expr::BadExpr(x) => NodeRef::BadExpr(x),
            Expr::Ident(x) => NodeRef::Ident(x),
            Expr::Ellipsis(x) => NodeRef::Ellipsis(x),
            Expr::BasicLit(x) => NodeRef::BasicLit(x),
            Expr::FuncLit(x) => NodeRef::FuncLit(x),
            Expr::CompositeLit(x) => NodeRef::CompositeLit(x),
            Expr::ParenExpr(x) => NodeRef::ParenExpr(x),
            Expr::SelectorExpr(x) => NodeRef::SelectorExpr(x),
            Expr::IndexExpr(x) => NodeRef::IndexExpr(x),
            Expr::IndexListExpr(x) => NodeRef::IndexListExpr(x),
            Expr::SliceExpr(x) => NodeRef::SliceExpr(x),
            Expr::TypeAssertExpr(x) => NodeRef::TypeAssertExpr(x),
            Expr::CallExpr(x) => NodeRef::CallExpr(x),
            Expr::StarExpr(x) => NodeRef::StarExpr(x),
            Expr::UnaryExpr(x) => NodeRef::UnaryExpr(x),
            Expr::BinaryExpr(x) => NodeRef::BinaryExpr(x),
            Expr::KeyValueExpr(x) => NodeRef::KeyValueExpr(x),
            Expr::ArrayType(x) => NodeRef::ArrayType(x),
            Expr::StructType(x) => NodeRef::StructType(x),
            Expr::FuncType(x) => NodeRef::FuncType(x),
            Expr::InterfaceType(x) => NodeRef::InterfaceType(x),
            Expr::MapType(x) => NodeRef::MapType(x),
            Expr::ChanType(x) => NodeRef::ChanType(x),
        }
    }
}

impl<'a> From<&'a Stmt> for NodeRef<'a> {
    fn from(s: &'a Stmt) -> Self {
        match s {
            Stmt::BadStmt(x) => NodeRef::BadStmt(x),
            Stmt::DeclStmt(x) => NodeRef::DeclStmt(x),
            Stmt::EmptyStmt(x) => NodeRef::EmptyStmt(x),
            Stmt::LabeledStmt(x) => NodeRef::LabeledStmt(x),
            Stmt::ExprStmt(x) => NodeRef::ExprStmt(x),
            Stmt::SendStmt(x) => NodeRef::SendStmt(x),
            Stmt::IncDecStmt(x) => NodeRef::IncDecStmt(x),
            Stmt::AssignStmt(x) => NodeRef::AssignStmt(x),
            Stmt::GoStmt(x) => NodeRef::GoStmt(x),
            Stmt::DeferStmt(x) => NodeRef::DeferStmt(x),
            Stmt::ReturnStmt(x) => NodeRef::ReturnStmt(x),
            Stmt::BranchStmt(x) => NodeRef::BranchStmt(x),
            Stmt::BlockStmt(x) => NodeRef::BlockStmt(x),
            Stmt::IfStmt(x) => NodeRef::IfStmt(x),
            Stmt::CaseClause(x) => NodeRef::CaseClause(x),
            Stmt::SwitchStmt(x) => NodeRef::SwitchStmt(x),
            Stmt::TypeSwitchStmt(x) => NodeRef::TypeSwitchStmt(x),
            Stmt::CommClause(x) => NodeRef::CommClause(x),
            Stmt::SelectStmt(x) => NodeRef::SelectStmt(x),
            Stmt::ForStmt(x) => NodeRef::ForStmt(x),
            Stmt::RangeStmt(x) => NodeRef::RangeStmt(x),
        }
    }
}

impl<'a> From<&'a Spec> for NodeRef<'a> {
    fn from(s: &'a Spec) -> Self {
        match s {
            Spec::ImportSpec(x) => NodeRef::ImportSpec(x),
            Spec::ValueSpec(x) => NodeRef::ValueSpec(x),
            Spec::TypeSpec(x) => NodeRef::TypeSpec(x),
        }
    }
}

impl<'a> From<&'a Decl> for NodeRef<'a> {
    fn from(d: &'a Decl) -> Self {
        match d {
            Decl::BadDecl(x) => NodeRef::BadDecl(x),
            Decl::GenDecl(x) => NodeRef::GenDecl(x),
            Decl::FuncDecl(x) => NodeRef::FuncDecl(x),
        }
    }
}

impl NodeRef<'_> {
    /// Returns the first source position belonging to the referenced node.
    pub fn pos(&self) -> Pos {
        match self {
            NodeRef::Field(node) => node.pos(),
            NodeRef::FieldList(node) => node.pos(),
            NodeRef::BadExpr(node) => node.pos(),
            NodeRef::Ident(node) => node.pos(),
            NodeRef::Ellipsis(node) => node.pos(),
            NodeRef::BasicLit(node) => node.pos(),
            NodeRef::FuncLit(node) => node.pos(),
            NodeRef::CompositeLit(node) => node.pos(),
            NodeRef::ParenExpr(node) => node.pos(),
            NodeRef::SelectorExpr(node) => node.pos(),
            NodeRef::IndexExpr(node) => node.pos(),
            NodeRef::IndexListExpr(node) => node.pos(),
            NodeRef::SliceExpr(node) => node.pos(),
            NodeRef::TypeAssertExpr(node) => node.pos(),
            NodeRef::CallExpr(node) => node.pos(),
            NodeRef::StarExpr(node) => node.pos(),
            NodeRef::UnaryExpr(node) => node.pos(),
            NodeRef::BinaryExpr(node) => node.pos(),
            NodeRef::KeyValueExpr(node) => node.pos(),
            NodeRef::ArrayType(node) => node.pos(),
            NodeRef::StructType(node) => node.pos(),
            NodeRef::FuncType(node) => node.pos(),
            NodeRef::InterfaceType(node) => node.pos(),
            NodeRef::MapType(node) => node.pos(),
            NodeRef::ChanType(node) => node.pos(),
            NodeRef::BadStmt(node) => node.pos(),
            NodeRef::DeclStmt(node) => node.pos(),
            NodeRef::EmptyStmt(node) => node.pos(),
            NodeRef::LabeledStmt(node) => node.pos(),
            NodeRef::ExprStmt(node) => node.pos(),
            NodeRef::SendStmt(node) => node.pos(),
            NodeRef::IncDecStmt(node) => node.pos(),
            NodeRef::AssignStmt(node) => node.pos(),
            NodeRef::GoStmt(node) => node.pos(),
            NodeRef::DeferStmt(node) => node.pos(),
            NodeRef::ReturnStmt(node) => node.pos(),
            NodeRef::BranchStmt(node) => node.pos(),
            NodeRef::BlockStmt(node) => node.pos(),
            NodeRef::IfStmt(node) => node.pos(),
            NodeRef::CaseClause(node) => node.pos(),
            NodeRef::SwitchStmt(node) => node.pos(),
            NodeRef::TypeSwitchStmt(node) => node.pos(),
            NodeRef::CommClause(node) => node.pos(),
            NodeRef::SelectStmt(node) => node.pos(),
            NodeRef::ForStmt(node) => node.pos(),
            NodeRef::RangeStmt(node) => node.pos(),
            NodeRef::ImportSpec(node) => node.pos(),
            NodeRef::ValueSpec(node) => node.pos(),
            NodeRef::TypeSpec(node) => node.pos(),
            NodeRef::BadDecl(node) => node.pos(),
            NodeRef::GenDecl(node) => node.pos(),
            NodeRef::FuncDecl(node) => node.pos(),
            NodeRef::File(node) => node.pos(),
            NodeRef::Package(node) => node.pos(),
        }
    }

    /// Returns the parser-assigned identity of the referenced syntax node.
    pub fn node_id(&self) -> AstNodeId {
        match self {
            NodeRef::Field(node) => node.node_id(),
            NodeRef::FieldList(node) => node.node_id(),
            NodeRef::BadExpr(node) => node.node_id(),
            NodeRef::Ident(node) => node.node_id(),
            NodeRef::Ellipsis(node) => node.node_id(),
            NodeRef::BasicLit(node) => node.node_id(),
            NodeRef::FuncLit(node) => node.node_id(),
            NodeRef::CompositeLit(node) => node.node_id(),
            NodeRef::ParenExpr(node) => node.node_id(),
            NodeRef::SelectorExpr(node) => node.node_id(),
            NodeRef::IndexExpr(node) => node.node_id(),
            NodeRef::IndexListExpr(node) => node.node_id(),
            NodeRef::SliceExpr(node) => node.node_id(),
            NodeRef::TypeAssertExpr(node) => node.node_id(),
            NodeRef::CallExpr(node) => node.node_id(),
            NodeRef::StarExpr(node) => node.node_id(),
            NodeRef::UnaryExpr(node) => node.node_id(),
            NodeRef::BinaryExpr(node) => node.node_id(),
            NodeRef::KeyValueExpr(node) => node.node_id(),
            NodeRef::ArrayType(node) => node.node_id(),
            NodeRef::StructType(node) => node.node_id(),
            NodeRef::FuncType(node) => node.node_id(),
            NodeRef::InterfaceType(node) => node.node_id(),
            NodeRef::MapType(node) => node.node_id(),
            NodeRef::ChanType(node) => node.node_id(),
            NodeRef::BadStmt(node) => node.node_id(),
            NodeRef::DeclStmt(node) => node.node_id(),
            NodeRef::EmptyStmt(node) => node.node_id(),
            NodeRef::LabeledStmt(node) => node.node_id(),
            NodeRef::ExprStmt(node) => node.node_id(),
            NodeRef::SendStmt(node) => node.node_id(),
            NodeRef::IncDecStmt(node) => node.node_id(),
            NodeRef::AssignStmt(node) => node.node_id(),
            NodeRef::GoStmt(node) => node.node_id(),
            NodeRef::DeferStmt(node) => node.node_id(),
            NodeRef::ReturnStmt(node) => node.node_id(),
            NodeRef::BranchStmt(node) => node.node_id(),
            NodeRef::BlockStmt(node) => node.node_id(),
            NodeRef::IfStmt(node) => node.node_id(),
            NodeRef::CaseClause(node) => node.node_id(),
            NodeRef::SwitchStmt(node) => node.node_id(),
            NodeRef::TypeSwitchStmt(node) => node.node_id(),
            NodeRef::CommClause(node) => node.node_id(),
            NodeRef::SelectStmt(node) => node.node_id(),
            NodeRef::ForStmt(node) => node.node_id(),
            NodeRef::RangeStmt(node) => node.node_id(),
            NodeRef::ImportSpec(node) => node.node_id(),
            NodeRef::ValueSpec(node) => node.node_id(),
            NodeRef::TypeSpec(node) => node.node_id(),
            NodeRef::BadDecl(node) => node.node_id(),
            NodeRef::GenDecl(node) => node.node_id(),
            NodeRef::FuncDecl(node) => node.node_id(),
            NodeRef::File(node) => node.node_id(),
            NodeRef::Package(node) => node.node_id(),
        }
    }

    /// Returns the Go struct name of the referenced node (e.g. `"GenDecl"`,
    /// `"Ident"`), matching Go's `%T` on the pointer type minus the `*ast.`
    /// prefix.
    pub fn kind_name(&self) -> &'static str {
        match self {
            NodeRef::Field(_) => "Field",
            NodeRef::FieldList(_) => "FieldList",
            NodeRef::BadExpr(_) => "BadExpr",
            NodeRef::Ident(_) => "Ident",
            NodeRef::Ellipsis(_) => "Ellipsis",
            NodeRef::BasicLit(_) => "BasicLit",
            NodeRef::FuncLit(_) => "FuncLit",
            NodeRef::CompositeLit(_) => "CompositeLit",
            NodeRef::ParenExpr(_) => "ParenExpr",
            NodeRef::SelectorExpr(_) => "SelectorExpr",
            NodeRef::IndexExpr(_) => "IndexExpr",
            NodeRef::IndexListExpr(_) => "IndexListExpr",
            NodeRef::SliceExpr(_) => "SliceExpr",
            NodeRef::TypeAssertExpr(_) => "TypeAssertExpr",
            NodeRef::CallExpr(_) => "CallExpr",
            NodeRef::StarExpr(_) => "StarExpr",
            NodeRef::UnaryExpr(_) => "UnaryExpr",
            NodeRef::BinaryExpr(_) => "BinaryExpr",
            NodeRef::KeyValueExpr(_) => "KeyValueExpr",
            NodeRef::ArrayType(_) => "ArrayType",
            NodeRef::StructType(_) => "StructType",
            NodeRef::FuncType(_) => "FuncType",
            NodeRef::InterfaceType(_) => "InterfaceType",
            NodeRef::MapType(_) => "MapType",
            NodeRef::ChanType(_) => "ChanType",
            NodeRef::BadStmt(_) => "BadStmt",
            NodeRef::DeclStmt(_) => "DeclStmt",
            NodeRef::EmptyStmt(_) => "EmptyStmt",
            NodeRef::LabeledStmt(_) => "LabeledStmt",
            NodeRef::ExprStmt(_) => "ExprStmt",
            NodeRef::SendStmt(_) => "SendStmt",
            NodeRef::IncDecStmt(_) => "IncDecStmt",
            NodeRef::AssignStmt(_) => "AssignStmt",
            NodeRef::GoStmt(_) => "GoStmt",
            NodeRef::DeferStmt(_) => "DeferStmt",
            NodeRef::ReturnStmt(_) => "ReturnStmt",
            NodeRef::BranchStmt(_) => "BranchStmt",
            NodeRef::BlockStmt(_) => "BlockStmt",
            NodeRef::IfStmt(_) => "IfStmt",
            NodeRef::CaseClause(_) => "CaseClause",
            NodeRef::SwitchStmt(_) => "SwitchStmt",
            NodeRef::TypeSwitchStmt(_) => "TypeSwitchStmt",
            NodeRef::CommClause(_) => "CommClause",
            NodeRef::SelectStmt(_) => "SelectStmt",
            NodeRef::ForStmt(_) => "ForStmt",
            NodeRef::RangeStmt(_) => "RangeStmt",
            NodeRef::ImportSpec(_) => "ImportSpec",
            NodeRef::ValueSpec(_) => "ValueSpec",
            NodeRef::TypeSpec(_) => "TypeSpec",
            NodeRef::BadDecl(_) => "BadDecl",
            NodeRef::GenDecl(_) => "GenDecl",
            NodeRef::FuncDecl(_) => "FuncDecl",
            NodeRef::File(_) => "File",
            NodeRef::Package(_) => "Package",
        }
    }
}

/// Returns the children of `node` in visit order, mirroring the per-node
/// cases of Go's `Walk` type switch (whose case order matches the order of
/// the corresponding node types in ast.go). Comment attachments are not
/// ported and are never visited.
fn children<'a>(node: NodeRef<'a>) -> Vec<NodeRef<'a>> {
    match node {
        // Fields
        NodeRef::Field(f) => {
            let mut v = Vec::new();
            for n in &f.names {
                v.push(NodeRef::Ident(n));
            }
            if let Some(t) = &f.typ {
                v.push(NodeRef::from(t));
            }
            if let Some(t) = &f.tag {
                v.push(NodeRef::BasicLit(t));
            }
            v
        }
        NodeRef::FieldList(f) => f.list.iter().map(NodeRef::Field).collect(),

        // Expressions
        NodeRef::BadExpr(_) | NodeRef::Ident(_) | NodeRef::BasicLit(_) => Vec::new(),

        NodeRef::Ellipsis(x) => match &x.elt {
            Some(elt) => vec![NodeRef::from(elt)],
            None => Vec::new(),
        },
        NodeRef::FuncLit(x) => vec![NodeRef::FuncType(&x.typ), NodeRef::BlockStmt(&x.body)],
        NodeRef::CompositeLit(x) => {
            let mut v = Vec::new();
            if let Some(t) = &x.typ {
                v.push(NodeRef::from(t));
            }
            v.extend(x.elts.iter().map(NodeRef::from));
            v
        }
        NodeRef::ParenExpr(x) => vec![NodeRef::from(&x.x)],
        NodeRef::SelectorExpr(x) => vec![NodeRef::from(&x.x), NodeRef::Ident(&x.sel)],
        NodeRef::IndexExpr(x) => vec![NodeRef::from(&x.x), NodeRef::from(&x.index)],
        NodeRef::IndexListExpr(x) => {
            let mut v = vec![NodeRef::from(&x.x)];
            v.extend(x.indices.iter().map(NodeRef::from));
            v
        }
        NodeRef::SliceExpr(x) => {
            let mut v = vec![NodeRef::from(&x.x)];
            if let Some(low) = &x.low {
                v.push(NodeRef::from(low));
            }
            if let Some(high) = &x.high {
                v.push(NodeRef::from(high));
            }
            if let Some(max) = &x.max {
                v.push(NodeRef::from(max));
            }
            v
        }
        NodeRef::TypeAssertExpr(x) => {
            let mut v = vec![NodeRef::from(&x.x)];
            if let Some(t) = &x.typ {
                v.push(NodeRef::from(t));
            }
            v
        }
        NodeRef::CallExpr(x) => {
            let mut v = vec![NodeRef::from(&x.fun)];
            v.extend(x.args.iter().map(NodeRef::from));
            v
        }
        NodeRef::StarExpr(x) => vec![NodeRef::from(&x.x)],
        NodeRef::UnaryExpr(x) => vec![NodeRef::from(&x.x)],
        NodeRef::BinaryExpr(x) => vec![NodeRef::from(&x.x), NodeRef::from(&x.y)],
        NodeRef::KeyValueExpr(x) => vec![NodeRef::from(&x.key), NodeRef::from(&x.value)],

        // Types
        NodeRef::ArrayType(x) => {
            let mut v = Vec::new();
            if let Some(len) = &x.len {
                v.push(NodeRef::from(len));
            }
            v.push(NodeRef::from(&x.elt));
            v
        }
        NodeRef::StructType(x) => match &x.fields {
            Some(fields) => vec![NodeRef::FieldList(fields)],
            // Go walks n.Fields unconditionally (a nil *FieldList would
            // panic); here nil is simply skipped.
            None => Vec::new(),
        },
        NodeRef::FuncType(x) => {
            let mut v = Vec::new();
            if let Some(tp) = &x.type_params {
                v.push(NodeRef::FieldList(tp));
            }
            if let Some(p) = &x.params {
                v.push(NodeRef::FieldList(p));
            }
            if let Some(r) = &x.results {
                v.push(NodeRef::FieldList(r));
            }
            v
        }
        NodeRef::InterfaceType(x) => match &x.methods {
            Some(methods) => vec![NodeRef::FieldList(methods)],
            None => Vec::new(),
        },
        NodeRef::MapType(x) => vec![NodeRef::from(&x.key), NodeRef::from(&x.value)],
        NodeRef::ChanType(x) => vec![NodeRef::from(&x.value)],

        // Statements
        NodeRef::BadStmt(_) | NodeRef::EmptyStmt(_) => Vec::new(),

        NodeRef::DeclStmt(x) => vec![NodeRef::from(&x.decl)],
        NodeRef::LabeledStmt(x) => vec![NodeRef::Ident(&x.label), NodeRef::from(&x.stmt)],
        NodeRef::ExprStmt(x) => vec![NodeRef::from(&x.x)],
        NodeRef::SendStmt(x) => vec![NodeRef::from(&x.chan_), NodeRef::from(&x.value)],
        NodeRef::IncDecStmt(x) => vec![NodeRef::from(&x.x)],
        NodeRef::AssignStmt(x) => {
            let mut v: Vec<NodeRef<'_>> = Vec::new();
            v.extend(x.lhs.iter().map(NodeRef::from));
            v.extend(x.rhs.iter().map(NodeRef::from));
            v
        }
        NodeRef::GoStmt(x) => vec![NodeRef::CallExpr(&x.call)],
        NodeRef::DeferStmt(x) => vec![NodeRef::CallExpr(&x.call)],
        NodeRef::ReturnStmt(x) => x.results.iter().map(NodeRef::from).collect(),
        NodeRef::BranchStmt(x) => match &x.label {
            Some(label) => vec![NodeRef::Ident(label)],
            None => Vec::new(),
        },
        NodeRef::BlockStmt(x) => x.list.iter().map(NodeRef::from).collect(),
        NodeRef::IfStmt(x) => {
            let mut v = Vec::new();
            if let Some(init) = &x.init {
                v.push(NodeRef::from(init));
            }
            v.push(NodeRef::from(&x.cond));
            v.push(NodeRef::BlockStmt(&x.body));
            if let Some(else_) = &x.else_ {
                v.push(NodeRef::from(else_));
            }
            v
        }
        NodeRef::CaseClause(x) => {
            let mut v: Vec<NodeRef<'_>> = Vec::new();
            v.extend(x.list.iter().map(NodeRef::from));
            v.extend(x.body.iter().map(NodeRef::from));
            v
        }
        NodeRef::SwitchStmt(x) => {
            let mut v = Vec::new();
            if let Some(init) = &x.init {
                v.push(NodeRef::from(init));
            }
            if let Some(tag) = &x.tag {
                v.push(NodeRef::from(tag));
            }
            v.push(NodeRef::BlockStmt(&x.body));
            v
        }
        NodeRef::TypeSwitchStmt(x) => {
            let mut v = Vec::new();
            if let Some(init) = &x.init {
                v.push(NodeRef::from(init));
            }
            v.push(NodeRef::from(&x.assign));
            v.push(NodeRef::BlockStmt(&x.body));
            v
        }
        NodeRef::CommClause(x) => {
            let mut v = Vec::new();
            if let Some(comm) = &x.comm {
                v.push(NodeRef::from(comm));
            }
            v.extend(x.body.iter().map(NodeRef::from));
            v
        }
        NodeRef::SelectStmt(x) => vec![NodeRef::BlockStmt(&x.body)],
        NodeRef::ForStmt(x) => {
            let mut v = Vec::new();
            if let Some(init) = &x.init {
                v.push(NodeRef::from(init));
            }
            if let Some(cond) = &x.cond {
                v.push(NodeRef::from(cond));
            }
            if let Some(post) = &x.post {
                v.push(NodeRef::from(post));
            }
            v.push(NodeRef::BlockStmt(&x.body));
            v
        }
        NodeRef::RangeStmt(x) => {
            let mut v = Vec::new();
            if let Some(key) = &x.key {
                v.push(NodeRef::from(key));
            }
            if let Some(value) = &x.value {
                v.push(NodeRef::from(value));
            }
            v.push(NodeRef::from(&x.x));
            v.push(NodeRef::BlockStmt(&x.body));
            v
        }

        // Specs
        NodeRef::ImportSpec(x) => {
            let mut v = Vec::new();
            if let Some(name) = &x.name {
                v.push(NodeRef::Ident(name));
            }
            v.push(NodeRef::BasicLit(&x.path));
            v
        }
        NodeRef::ValueSpec(x) => {
            let mut v: Vec<NodeRef<'_>> = Vec::new();
            v.extend(x.names.iter().map(NodeRef::Ident));
            if let Some(t) = &x.typ {
                v.push(NodeRef::from(t));
            }
            v.extend(x.values.iter().map(NodeRef::from));
            v
        }
        NodeRef::TypeSpec(x) => {
            let mut v = vec![NodeRef::Ident(&x.name)];
            if let Some(tp) = &x.type_params {
                v.push(NodeRef::FieldList(tp));
            }
            v.push(NodeRef::from(&x.typ));
            v
        }

        // Declarations
        NodeRef::BadDecl(_) => Vec::new(),
        NodeRef::GenDecl(x) => x.specs.iter().map(NodeRef::from).collect(),
        NodeRef::FuncDecl(x) => {
            let mut v = Vec::new();
            if let Some(recv) = &x.recv {
                v.push(NodeRef::FieldList(recv));
            }
            v.push(NodeRef::Ident(&x.name));
            v.push(NodeRef::FuncType(&x.typ));
            if let Some(body) = &x.body {
                v.push(NodeRef::BlockStmt(body));
            }
            v
        }

        // Files and packages
        NodeRef::File(f) => {
            let mut v = vec![NodeRef::Ident(&f.name)];
            v.extend(f.decls.iter().map(NodeRef::from));
            // don't walk imports/unresolved - they have been visited already
            // through the individual decls (mirrors Go, which skips Comments).
            v
        }
        NodeRef::Package(p) => p.files.values().map(NodeRef::File).collect(),
    }
}

/// Traverses an AST in depth-first order: starts by calling `f(Some(node))`.
/// If `f` returns `true`, the children of `node` are visited recursively,
/// followed by a call of `f(None)`; if it returns `false`, the whole subtree
/// of `node` is skipped (and no `f(None)` is called for it).
///
/// This reproduces Go's `ast.Inspect` (via the internal `inspector` visitor).
pub fn inspect<'a>(root: NodeRef<'a>, f: &mut impl FnMut(Option<NodeRef<'a>>) -> bool) {
    walk(root, f);
}

/// The recursive walk behind [`inspect`].
fn walk<'a>(node: NodeRef<'a>, f: &mut impl FnMut(Option<NodeRef<'a>>) -> bool) {
    if !f(Some(node)) {
        return;
    }
    // walk children
    // (the order of the cases matches the order
    // of the corresponding node types in ast.go)
    for child in children(node) {
        walk(child, f);
    }
    f(None);
}

/// An iterator over all the nodes of the syntax tree beneath (and including)
/// the specified root, in depth-first preorder.
///
/// For greater control over the traversal of each subtree, use [`inspect`] or
/// [`preorder_stack`].
pub struct PreorderIter<'a> {
    stack: Vec<NodeRef<'a>>,
}

/// Returns an iterator over all the nodes of the syntax tree beneath (and
/// including) the specified root, in depth-first preorder.
///
/// This corresponds to Go's `ast.Preorder` (which returns an `iter.Seq`);
/// `break` just drops the iterator.
pub fn preorder<'a>(root: NodeRef<'a>) -> PreorderIter<'a> {
    PreorderIter { stack: vec![root] }
}

impl<'a> Iterator for PreorderIter<'a> {
    type Item = NodeRef<'a>;

    fn next(&mut self) -> Option<NodeRef<'a>> {
        let node = self.stack.pop()?;
        // Push the children in reverse so that they are yielded in source
        // order (depth-first preorder).
        for child in children(node).into_iter().rev() {
            self.stack.push(child);
        }
        Some(node)
    }
}

/// Traverses the tree rooted at `root`, calling `f` before visiting each node.
///
/// Each call to `f` provides the current node and the traversal stack,
/// consisting of the original value of `stack` appended with all nodes from
/// `root` to the node, excluding the node itself. (This design allows calls
/// to [`preorder_stack`] to be nested without double counting.)
///
/// If `f` returns false, the traversal skips over that subtree. Unlike
/// [`inspect`], no second call to `f` is made after visiting a node for which
/// `f` returned false. (In practice, the second call is nearly always used
/// only to pop the stack, and it is surprisingly tricky to do this correctly.)
///
/// `stack` is passed by value like Go's slice header: it is only used as the
/// seed of the traversal stack, and an unbalanced push/pop panics with
/// "push/pop mismatch" after the traversal.
pub fn preorder_stack<'a>(
    root: NodeRef<'a>,
    mut stack: Vec<NodeRef<'a>>,
    mut f: impl FnMut(NodeRef<'a>, &[NodeRef<'a>]) -> bool,
) {
    let before = stack.len();
    inspect(root, &mut |n: Option<NodeRef<'a>>| {
        match n {
            Some(node) => {
                if !f(node, &stack) {
                    // Do not push, as there will be no corresponding pop.
                    return false;
                }
                stack.push(node); // push
            }
            None => {
                stack.pop(); // pop
            }
        }
        true
    });
    if stack.len() != before {
        panic!("push/pop mismatch");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::{AstNodeId, File as TokenFile, FileSet, NO_POS, Pos, Token};
    use std::rc::Rc;

    // ------------------------------------------------------------------
    // Test helpers (substitute for go/parser.ParseFile: the parser is not
    // ported yet, so the trees below are built by hand, with positions
    // derived from byte offsets in the fixture source text).

    /// Returns the byte offset of the first occurrence of `pat` in `src`.
    fn idx(src: &str, pat: &str) -> i64 {
        src.find(pat)
            .unwrap_or_else(|| panic!("pattern {pat:?} not found in fixture source")) as i64
    }

    /// Registers `src` as a file in `fset` with a proper line table, so that
    /// positions obtained via [`TokenFile::pos`] map to Go-identical
    /// line:column values.
    fn add_fixture_file(fset: &mut FileSet, name: &str, src: &str) -> Rc<TokenFile> {
        let file = fset.add_file(name, -1, src.len() as i64);
        file.set_lines_for_content(src.as_bytes());
        file
    }

    fn ident_at(name: &str, name_pos: Pos) -> Ident {
        Ident {
            node_id: AstNodeId::INVALID,
            name_pos,
            name: name.into(),
        }
    }

    fn ident(name: &str) -> Ident {
        ident_at(name, NO_POS)
    }

    fn lit(value: &str) -> BasicLit {
        BasicLit {
            node_id: AstNodeId::INVALID,
            value_pos: NO_POS,
            value_end: NO_POS,
            kind: Token::String,
            value: value.into(),
        }
    }

    fn func_decl(name: &str, body: Vec<Stmt>) -> FuncDecl {
        FuncDecl {
            node_id: AstNodeId::INVALID,
            commands: vec![],
            recv: None,
            name: ident(name),
            typ: FuncType {
                node_id: AstNodeId::INVALID,
                func: NO_POS,
                type_params: None,
                params: Some(FieldList {
                    node_id: AstNodeId::INVALID,
                    opening: NO_POS,
                    list: vec![],
                    closing: NO_POS,
                }),
                results: None,
            },
            body: Some(Box::new(BlockStmt {
                node_id: AstNodeId::INVALID,
                lbrace: NO_POS,
                list: body,
                rbrace: NO_POS,
            })),
        }
    }

    fn call_stmt(fun: &str, args: Vec<Expr>) -> Stmt {
        Stmt::ExprStmt(ExprStmt {
            node_id: AstNodeId::INVALID,
            x: Expr::CallExpr(Box::new(CallExpr {
                node_id: AstNodeId::INVALID,
                fun: Expr::Ident(ident(fun)),
                lparen: NO_POS,
                args,
                ellipsis: NO_POS,
                rparen: NO_POS,
            })),
        })
    }

    fn preorder_file(decls: Vec<Decl>) -> File {
        File {
            node_id: AstNodeId::INVALID,
            commands: vec![],
            package: NO_POS,
            name: ident("p"),
            decls,
            file_start: NO_POS,
            file_end: NO_POS,
            imports: vec![],
        }
    }

    // TestPreorder_Break: Preorder must handle a break in the middle of
    // walking a node (the Go test failed with a runtime panic when the
    // iterator kept yielding siblings after yield had returned false).
    #[test]
    fn test_preorder_break() {
        let f = preorder_file(vec![Decl::GenDecl(GenDecl {
            node_id: AstNodeId::INVALID,
            commands: vec![],
            tok_pos: NO_POS,
            tok: Token::Type,
            lparen: NO_POS,
            specs: vec![Spec::TypeSpec(TypeSpec {
                node_id: AstNodeId::INVALID,
                commands: vec![],
                name: ident("T"),
                type_params: None,
                assign: NO_POS,
                typ: Expr::StructType(StructType {
                    node_id: AstNodeId::INVALID,
                    struct_: NO_POS,
                    fields: Some(FieldList {
                        node_id: AstNodeId::INVALID,
                        opening: NO_POS,
                        list: vec![Field {
                            node_id: AstNodeId::INVALID,
                            commands: vec![],
                            names: vec![ident("F")],
                            typ: Some(Expr::Ident(ident("int"))),
                            tag: Some(BasicLit {
                                node_id: AstNodeId::INVALID,
                                value_pos: NO_POS,
                                value_end: NO_POS,
                                kind: Token::String,
                                value: "`json:\"f\"`".into(),
                            }),
                        }],
                        closing: NO_POS,
                    }),
                    incomplete: false,
                }),
            })],
            rparen: NO_POS,
        })]);

        let mut found_f = false;
        for n in preorder(NodeRef::File(&f)) {
            if let NodeRef::Ident(id) = n
                && id.name == "F"
            {
                found_f = true;
                break;
            }
        }
        assert!(found_f, "expected to find ident F and break");
    }

    // TestPreorderStack: event sequence and stack capture.
    #[test]
    fn test_preorder_stack() {
        // Source (Go):
        //	package a
        //	func f() {}
        //	func g() { print("hello"); panic("oops") }
        let f = preorder_file(vec![
            Decl::FuncDecl(func_decl("f", vec![])),
            Decl::FuncDecl(func_decl(
                "g",
                vec![
                    call_stmt("print", vec![Expr::BasicLit(lit("\"hello\""))]),
                    call_stmt("panic", vec![Expr::BasicLit(lit("\"oops\""))]),
                ],
            )),
        ]);

        let mut events: Vec<&'static str> = vec![];
        let mut got_stack: Vec<&'static str> = vec![];
        preorder_stack(NodeRef::File(&f), vec![], |n, stack| {
            events.push(n.kind_name());
            if let NodeRef::FuncDecl(d) = n
                && d.name.name == "f"
            {
                return false; // skip subtree of f()
            }
            if let NodeRef::BasicLit(l) = n
                && l.value == "\"oops\""
            {
                for n in stack {
                    got_stack.push(n.kind_name());
                }
            }
            true
        });

        // Check sequence of events.
        let want_events: Vec<&str> = vec![
            "File",
            "Ident",    // package a
            "FuncDecl", // func f()  [pruned]
            "FuncDecl",
            "Ident",
            "FuncType",
            "FieldList",
            "BlockStmt", // func g()
            "ExprStmt",
            "CallExpr",
            "Ident",
            "BasicLit", // print...
            "ExprStmt",
            "CallExpr",
            "Ident",
            "BasicLit", // panic...
        ];
        assert_eq!(
            events, want_events,
            "PreorderStack events:\ngot:  {events:?}\nwant: {want_events:?}"
        );

        // Check captured stack.
        let want_stack: Vec<&str> = vec!["File", "FuncDecl", "BlockStmt", "ExprStmt", "CallExpr"];
        assert_eq!(
            got_stack, want_stack,
            "PreorderStack stack:\ngot:  {got_stack:?}\nwant: {want_stack:?}"
        );
    }

    // ExampleInspect: print all identifiers and literals with positions.
    #[test]
    fn example_inspect() {
        let src = "\npackage p\nconst c = 1.0\nvar X = f(3.14)*2 + c\n";
        let mut fset = FileSet::new();
        let file = add_fixture_file(&mut fset, "src.go", src);
        let at = |pat: &str| file.pos(idx(src, pat));

        // var X = f(3.14)*2 + c
        let var_value = Expr::BinaryExpr(Box::new(BinaryExpr {
            node_id: AstNodeId::INVALID,
            x: Expr::BinaryExpr(Box::new(BinaryExpr {
                node_id: AstNodeId::INVALID,
                x: Expr::CallExpr(Box::new(CallExpr {
                    node_id: AstNodeId::INVALID,
                    fun: Expr::Ident(ident_at("f", at("f(3.14)"))),
                    lparen: at("(3.14)"),
                    args: vec![Expr::BasicLit(BasicLit {
                        node_id: AstNodeId::INVALID,
                        value_pos: at("3.14"),
                        value_end: NO_POS,
                        kind: Token::Float,
                        value: "3.14".into(),
                    })],
                    ellipsis: NO_POS,
                    rparen: at(")*2"),
                })),
                op_pos: at("*2"),
                op: Token::Mul,
                y: Expr::BasicLit(BasicLit {
                    node_id: AstNodeId::INVALID,
                    value_pos: at("*2") + 1,
                    value_end: NO_POS,
                    kind: Token::Int,
                    value: "2".into(),
                }),
            })),
            op_pos: at("+ c"),
            op: Token::Add,
            y: Expr::Ident(ident_at("c", at("+ c") + 2)),
        }));

        let f = File {
            node_id: AstNodeId::INVALID,
            commands: vec![],
            package: at("package"),
            name: ident_at("p", at("package p") + 8),
            decls: vec![
                Decl::GenDecl(GenDecl {
                    node_id: AstNodeId::INVALID,
                    commands: vec![],
                    tok_pos: at("const"),
                    tok: Token::Const,
                    lparen: NO_POS,
                    specs: vec![Spec::ValueSpec(ValueSpec {
                        node_id: AstNodeId::INVALID,
                        commands: vec![],
                        names: vec![ident_at("c", at("const c") + 6)],
                        typ: None,
                        values: vec![Expr::BasicLit(BasicLit {
                            node_id: AstNodeId::INVALID,
                            value_pos: at("1.0"),
                            value_end: NO_POS,
                            kind: Token::Float,
                            value: "1.0".into(),
                        })],
                    })],
                    rparen: NO_POS,
                }),
                Decl::GenDecl(GenDecl {
                    node_id: AstNodeId::INVALID,
                    commands: vec![],
                    tok_pos: at("var"),
                    tok: Token::Var,
                    lparen: NO_POS,
                    specs: vec![Spec::ValueSpec(ValueSpec {
                        node_id: AstNodeId::INVALID,
                        commands: vec![],
                        names: vec![ident_at("X", at("var X") + 4)],
                        typ: None,
                        values: vec![var_value],
                    })],
                    rparen: NO_POS,
                }),
            ],
            file_start: file.pos(0),
            file_end: file.pos(src.len() as i64),
            imports: vec![],
        };

        // Inspect the AST and print all identifiers and literals.
        let mut got: Vec<String> = vec![];
        inspect(NodeRef::File(&f), &mut |n| {
            let mut s: Option<&str> = None;
            match n {
                Some(NodeRef::BasicLit(x)) => s = Some(&x.value),
                Some(NodeRef::Ident(x)) => s = Some(&x.name),
                _ => {}
            }
            if let Some(s) = s {
                let n = n.expect("some node");
                let pos = match n {
                    NodeRef::Ident(x) => x.pos(),
                    NodeRef::BasicLit(x) => x.pos(),
                    _ => unreachable!(),
                };
                got.push(format!("{}:\t{s}", fset.position(pos)));
            }
            true
        });

        let want = [
            "src.go:2:9:\tp",
            "src.go:3:7:\tc",
            "src.go:3:11:\t1.0",
            "src.go:4:5:\tX",
            "src.go:4:9:\tf",
            "src.go:4:11:\t3.14",
            "src.go:4:17:\t2",
            "src.go:4:21:\tc",
        ];
        assert_eq!(got, want, "Inspect output:\ngot:  {got:?}\nwant: {want:?}");
    }

    // ExamplePreorder: print identifiers in order.
    #[test]
    fn example_preorder() {
        let src = "\npackage p\n\nfunc f(x, y int) {\n\tprint(x + y)\n}\n";
        let mut fset = FileSet::new();
        let file = add_fixture_file(&mut fset, "", src);
        let at = |pat: &str| file.pos(idx(src, pat));

        let f = File {
            node_id: AstNodeId::INVALID,
            commands: vec![],
            package: at("package"),
            name: ident_at("p", at("package p") + 8),
            decls: vec![Decl::FuncDecl(FuncDecl {
                node_id: AstNodeId::INVALID,
                commands: vec![],
                recv: None,
                name: ident_at("f", at("f(x, y")),
                typ: FuncType {
                    node_id: AstNodeId::INVALID,
                    func: at("func f"),
                    type_params: None,
                    params: Some(FieldList {
                        node_id: AstNodeId::INVALID,
                        opening: at("(x"),
                        list: vec![Field {
                            node_id: AstNodeId::INVALID,
                            commands: vec![],
                            names: vec![
                                ident_at("x", at("f(x") + 2),
                                ident_at("y", at(", y int") + 2),
                            ],
                            typ: Some(Expr::Ident(ident_at("int", at("int)")))),
                            tag: None,
                        }],
                        closing: at("int)") + 3,
                    }),
                    results: None,
                },
                body: Some(Box::new(BlockStmt {
                    node_id: AstNodeId::INVALID,
                    lbrace: at("{"),
                    list: vec![call_stmt_binary(&fset, &file, src)],
                    rbrace: at("}"),
                })),
            })],
            file_start: file.pos(0),
            file_end: file.pos(src.len() as i64),
            imports: vec![],
        };

        // Print identifiers in order.
        let mut names: Vec<String> = vec![];
        for n in preorder(NodeRef::File(&f)) {
            if let NodeRef::Ident(id) = n {
                names.push(id.name.clone());
            }
        }

        let want = ["p", "f", "x", "y", "int", "print", "x", "y"];
        assert_eq!(
            names, want,
            "Preorder identifiers:\ngot:  {names:?}\nwant: {want:?}"
        );
    }

    // Helper for example_preorder: the `print(x + y)` statement inside the
    // function body.
    fn call_stmt_binary(_fset: &FileSet, file: &TokenFile, src: &str) -> Stmt {
        let at = |pat: &str| file.pos(idx(src, pat));
        Stmt::ExprStmt(ExprStmt {
            node_id: AstNodeId::INVALID,
            x: Expr::CallExpr(Box::new(CallExpr {
                node_id: AstNodeId::INVALID,
                fun: Expr::Ident(ident_at("print", at("print("))),
                lparen: at("print(") + 5,
                args: vec![Expr::BinaryExpr(Box::new(BinaryExpr {
                    node_id: AstNodeId::INVALID,
                    x: Expr::Ident(ident_at("x", at("(x + y)") + 1)),
                    op_pos: at("+ y"),
                    op: Token::Add,
                    y: Expr::Ident(ident_at("y", at("+ y)") + 2)),
                }))],
                ellipsis: NO_POS,
                rparen: at("x + y)") + 6,
            })),
        })
    }
}
