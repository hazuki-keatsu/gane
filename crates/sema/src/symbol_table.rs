// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Hazuki Keatsu

//! Name interning and object storage for semantic analysis.
//!
//! This module is analogous to the object side of `go/types`: it provides
//! stable identities for declared entities and their names. It deliberately
//! does not resolve lexical visibility; `scope` owns declaration and lookup.

use crate::types::{NameId, Object, ObjectId, ObjectKind, PackageId, ScopeId, TypeId};
use gane_parser::token::AstNodeId;
use std::collections::HashMap;

/// Bidirectional, per-analysis name interner.
///
/// `NameId(0)` is the empty sentinel; normal source identifiers are allocated
/// from one upwards. Equal source text always receives the same ID.
#[derive(Clone, Debug)]
pub struct NameInterner {
    names: Vec<String>,
    ids: HashMap<String, NameId>,
}

impl Default for NameInterner {
    fn default() -> Self {
        Self {
            names: vec![String::new()],
            ids: HashMap::new(),
        }
    }
}

impl NameInterner {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, text: &str) -> NameId {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = NameId::from_raw(self.names.len() as u32);
        let owned = text.to_owned();
        self.names.push(owned.clone());
        self.ids.insert(owned, id);
        id
    }

    pub fn get(&self, id: NameId) -> Option<&str> {
        self.names.get(id.raw() as usize).map(String::as_str)
    }
}

/// Stable arena for [`Object`] values. The invalid object occupies index zero,
/// so callers have a non-null error-recovery object when required.
#[derive(Clone, Debug)]
pub struct ObjectArena {
    objects: Vec<Object>,
}

impl Default for ObjectArena {
    fn default() -> Self {
        Self {
            objects: vec![invalid_object()],
        }
    }
}

impl ObjectArena {
    pub fn alloc(&mut self, object: Object) -> ObjectId {
        let id = ObjectId::from_raw(self.objects.len() as u32);
        self.objects.push(object);
        id
    }

    pub fn get(&self, id: ObjectId) -> &Object {
        self.objects
            .get(id.raw() as usize)
            .unwrap_or(&self.objects[ObjectId::INVALID.raw() as usize])
    }

    pub fn get_mut(&mut self, id: ObjectId) -> Option<&mut Object> {
        self.objects.get_mut(id.raw() as usize)
    }
}

/// The object/name portion of Gane's `go/types`-inspired semantic model.
///
/// Its interface intentionally stays small: callers intern spelling, allocate
/// an object, and query either identity. Scope insertion and parent walking
/// happen in `ScopeArena`, preventing name shadowing rules from leaking here.
#[derive(Clone, Debug, Default)]
pub struct SymbolTable {
    names: NameInterner,
    objects: ObjectArena,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, text: &str) -> NameId {
        self.names.intern(text)
    }

    pub fn name(&self, id: NameId) -> Option<&str> {
        self.names.get(id)
    }

    pub fn alloc(&mut self, object: Object) -> ObjectId {
        self.objects.alloc(object)
    }

    pub fn object(&self, id: ObjectId) -> &Object {
        self.objects.get(id)
    }

    pub fn object_mut(&mut self, id: ObjectId) -> Option<&mut Object> {
        self.objects.get_mut(id)
    }
}

/// Builds an object with common declaration metadata. The checker chooses the
/// [`ObjectKind`] and inserts the resulting ID into a lexical scope.
pub fn declared_object(
    kind: ObjectKind,
    name: NameId,
    package: Option<PackageId>,
    parent: ScopeId,
    declaration: Option<AstNodeId>,
    typ: TypeId,
) -> Object {
    Object {
        kind,
        name,
        package,
        parent,
        declaration,
        typ,
    }
}

fn invalid_object() -> Object {
    declared_object(
        ObjectKind::Invalid,
        NameId::default(),
        None,
        ScopeId::default(),
        None,
        TypeId::INVALID,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_and_reversible() {
        let mut names = NameInterner::new();
        let first = names.intern("count");
        let second = names.intern("count");
        let other = names.intern("total");

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(names.get(first), Some("count"));
        assert_eq!(names.get(NameId::from_raw(99)), None);
    }

    #[test]
    fn objects_are_stable_and_invalid_object_is_reserved() {
        let mut table = SymbolTable::new();
        let count = table.intern("count");
        let object = table.alloc(declared_object(
            ObjectKind::Var { embedded: false },
            count,
            None,
            ScopeId::from_raw(1),
            None,
            TypeId::from_raw(1),
        ));

        assert_eq!(object.raw(), 1);
        assert_eq!(table.object(object).name, count);
        assert!(matches!(
            table.object(ObjectId::INVALID).kind,
            ObjectKind::Invalid
        ));
    }
}
