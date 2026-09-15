//! Lexical scope storage and name lookup.
//!
//! ScopeArena owns declaration insertion, duplicate detection, and parent
//! walking. Object creation belongs to SymbolTable; callers therefore pass
//! stable NameId/ObjectId pairs instead of strings or object references.

use crate::types::{NameId, ObjectId, Scope, ScopeId, ScopeKind};
use gane_parser::token::AstNodeId;

/// A duplicate declaration in one lexical scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DuplicateDeclaration {
    pub scope: ScopeId,
    pub name: NameId,
    pub existing: ObjectId,
}

/// A failure to insert a declaration into a lexical scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclareError {
    Duplicate(DuplicateDeclaration),
    UnknownScope(ScopeId),
}

/// The result of a lexical lookup including the scope that supplied the name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedName {
    pub scope: ScopeId,
    pub object: ObjectId,
}

/// Arena of lexical scopes. Index zero is always the universe scope.
#[derive(Clone, Debug)]
pub struct ScopeArena {
    scopes: Vec<Scope>,
}

impl Default for ScopeArena {
    fn default() -> Self {
        Self {
            scopes: vec![Scope {
                parent: None,
                kind: ScopeKind::Universe,
                anchor: None,
                names: Default::default(),
            }],
        }
    }
}

impl ScopeArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn universe(&self) -> ScopeId {
        ScopeId::UNIVERSE
    }

    /// Allocates a child scope. Parents always precede children in the arena,
    /// which makes the parent chain acyclic by construction.
    pub fn child(
        &mut self,
        parent: ScopeId,
        kind: ScopeKind,
        anchor: Option<AstNodeId>,
    ) -> ScopeId {
        assert!(self.contains(parent), "parent scope must exist");
        let id = ScopeId::from_raw(self.scopes.len() as u32);
        self.scopes.push(Scope {
            parent: Some(parent),
            kind,
            anchor,
            names: Default::default(),
        });
        id
    }

    /// Inserts a declaration into exactly one scope. Existing declarations are
    /// never overwritten, so the checker can issue a diagnostic at both sites.
    pub fn declare(
        &mut self,
        scope: ScopeId,
        name: NameId,
        object: ObjectId,
    ) -> Result<(), DeclareError> {
        let Some(scope_data) = self.scopes.get_mut(scope.raw() as usize) else {
            return Err(DeclareError::UnknownScope(scope));
        };

        if let Some(&existing) = scope_data.names.get(&name) {
            return Err(DeclareError::Duplicate(DuplicateDeclaration {
                scope,
                name,
                existing,
            }));
        }
        scope_data.names.insert(name, object);
        Ok(())
    }

    pub fn lookup_local(&self, scope: ScopeId, name: NameId) -> Option<ObjectId> {
        self.scopes
            .get(scope.raw() as usize)
            .and_then(|scope| scope.names.get(&name).copied())
    }

    /// Finds the closest declaration of `name`, naturally implementing Go's
    /// lexical shadowing rules.
    pub fn lookup(&self, scope: ScopeId, name: NameId) -> Option<ResolvedName> {
        let mut current = self.contains(scope).then_some(scope);
        while let Some(id) = current {
            let scope_data = self.scopes.get(id.raw() as usize)?;
            if let Some(&object) = scope_data.names.get(&name) {
                return Some(ResolvedName { scope: id, object });
            }
            current = scope_data.parent;
        }
        None
    }

    pub fn scope(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.raw() as usize)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    fn contains(&self, id: ScopeId) -> bool {
        (id.raw() as usize) < self.scopes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universe_scope_is_reserved() {
        let scopes = ScopeArena::new();
        let universe = scopes.scope(scopes.universe()).unwrap();
        assert_eq!(universe.kind, ScopeKind::Universe);
        assert_eq!(scopes.len(), 1);
    }

    #[test]
    fn duplicate_declaration_keeps_the_first_object() {
        let mut scopes = ScopeArena::new();
        let universe = scopes.universe();
        let name = NameId::from_raw(1);
        let first = ObjectId::from_raw(1);
        let second = ObjectId::from_raw(2);

        scopes.declare(universe, name, first).unwrap();
        let DeclareError::Duplicate(duplicate) =
            scopes.declare(universe, name, second).unwrap_err()
        else {
            panic!("expected duplicate declaration");
        };

        assert_eq!(duplicate.existing, first);
        assert_eq!(scopes.lookup_local(universe, name), Some(first));
    }

    #[test]
    fn lookup_walks_parents_and_prefers_the_nearest_declaration() {
        let mut scopes = ScopeArena::new();
        let universe = scopes.universe();
        let package = scopes.child(universe, ScopeKind::Package, None);
        let block = scopes.child(package, ScopeKind::Block, None);
        let name = NameId::from_raw(1);
        let outer = ObjectId::from_raw(1);
        let inner = ObjectId::from_raw(2);

        scopes.declare(package, name, outer).unwrap();
        assert_eq!(scopes.lookup(block, name).unwrap().object, outer);

        scopes.declare(block, name, inner).unwrap();
        let resolved = scopes.lookup(block, name).unwrap();
        assert_eq!(resolved.scope, block);
        assert_eq!(resolved.object, inner);
    }
}
