//! Shared diagnostic data structures for the Gane toolchain.
//!
//! Source locations are represented by the parser's [`FileSet`] and `Pos`,
//! which remain the single source of truth for file and line information.

use std::cmp::Ordering;
use std::fmt;

use gane_parser::token::{AstNodeId, FileSet, Pos, Position};

/// The impact of a diagnostic on analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

/// A stable, producer-defined diagnostic identifier such as `E2001`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DiagnosticCode(pub &'static str);

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A diagnostic location anchored to an AST node.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Label {
    pub node: Option<AstNodeId>,
    pub message: String,
    position: Option<Pos>,
}

impl Label {
    pub fn new(node: Option<AstNodeId>, message: impl Into<String>) -> Self {
        Self {
            node,
            message: message.into(),
            position: None,
        }
    }

    fn capture_position(&mut self, locate: &mut impl FnMut(AstNodeId) -> Option<Pos>) {
        self.position = self.node.and_then(locate);
    }

    fn position(&self, files: &FileSet) -> Position {
        self.position
            .map(|pos| files.position(pos))
            .unwrap_or_default()
    }
}

/// A user-visible diagnostic anchored to parser-assigned AST node identities.
///
/// Source positions are resolved only when a caller supplies the [`FileSet`]
/// that parsed the source. Diagnostics store AST identities rather than source
/// ranges.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub primary: Label,
    pub secondary: Vec<Label>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: DiagnosticCode, primary: Label) -> Self {
        Self {
            severity,
            code,
            primary,
            secondary: Vec::new(),
        }
    }

    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    /// Captures source positions for this diagnostic's node anchors.
    ///
    /// This is called while the producer still owns the AST. The captured
    /// positions are private diagnostic data; [`AstNodeId`] is not resolved
    /// through [`FileSet`] during rendering.
    pub fn capture_positions(&mut self, mut locate: impl FnMut(AstNodeId) -> Option<Pos>) {
        self.primary.capture_position(&mut locate);
        for label in &mut self.secondary {
            label.capture_position(&mut locate);
        }
    }

    pub fn position(&self, files: &FileSet) -> Position {
        self.primary.position(files)
    }

    pub fn message(&self) -> &str {
        &self.primary.message
    }

    /// Returns a diagnostic renderer that resolves source positions through
    /// the parser's file set.
    pub fn display_with<'a>(&'a self, files: &'a FileSet) -> impl fmt::Display + 'a {
        DiagnosticDisplay {
            diagnostic: self,
            files,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}]: {}",
            severity_name(self.severity),
            self.code,
            self.message()
        )
    }
}

struct DiagnosticDisplay<'a> {
    diagnostic: &'a Diagnostic,
    files: &'a FileSet,
}

impl fmt::Display for DiagnosticDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let diagnostic = self.diagnostic;
        writeln!(f, "{diagnostic}")?;
        writeln!(f, " --> {}", diagnostic.position(self.files))?;

        for label in &diagnostic.secondary {
            let position = label.position(self.files);
            writeln!(f, "  = note: {}\n     --> {}", label.message, position)?;
        }

        Ok(())
    }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
        Severity::Help => "help",
    }
}

/// In-memory collection of node-anchored diagnostics.
///
/// `Diagnostics` is the compiler-facing collection. Source positions are
/// resolved on demand through [`Diagnostic::display_with`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn error(
        &mut self,
        code: DiagnosticCode,
        primary: Option<AstNodeId>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic::new(
            Severity::Error,
            code,
            Label::new(primary, message),
        ));
    }

    pub fn warning(
        &mut self,
        code: DiagnosticCode,
        primary: Option<AstNodeId>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic::new(
            Severity::Warning,
            code,
            Label::new(primary, message),
        ));
    }

    pub fn note(
        &mut self,
        code: DiagnosticCode,
        primary: Option<AstNodeId>,
        message: impl Into<String>,
    ) {
        self.push(Diagnostic::new(
            Severity::Note,
            code,
            Label::new(primary, message),
        ));
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Captures source positions before the AST is released.
    pub fn capture_positions(&mut self, mut locate: impl FnMut(AstNodeId) -> Option<Pos>) {
        for diagnostic in &mut self.items {
            diagnostic.capture_positions(&mut locate);
        }
    }

    /// Sorts deterministically, removes exact duplicates, and returns the items.
    pub fn finish(mut self) -> Vec<Diagnostic> {
        self.items.sort_by(diagnostic_order);
        self.items.dedup();
        self.items
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

fn diagnostic_order(a: &Diagnostic, b: &Diagnostic) -> Ordering {
    a.primary
        .node
        .map(AstNodeId::raw)
        .cmp(&b.primary.node.map(AstNodeId::raw))
        .then(a.severity.cmp(&b.severity))
        .then(a.code.cmp(&b.code))
        .then(a.message().cmp(b.message()))
        .then(a.secondary.cmp(&b.secondary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gane_parser::{
        parser::{Mode, parse_file},
        token::FileSet,
    };

    const UNDEFINED_NAME: DiagnosticCode = DiagnosticCode("E2001");
    const DUPLICATE_DECLARATION: DiagnosticCode = DiagnosticCode("E2002");

    fn locate(file: &gane_parser::ast::File, id: AstNodeId) -> Option<Pos> {
        gane_parser::ast::preorder(gane_parser::ast::NodeRef::File(file))
            .find(|node| node.node_id() == id)
            .map(|node| node.pos())
    }

    fn parsed_file() -> (FileSet, gane_parser::ast::File) {
        let mut files = FileSet::new();
        let (file, errors) = parse_file(
            &mut files,
            "main.go",
            b"package main\nvar value int\n",
            Mode::default(),
        );
        assert!(errors.is_none());
        (files, file)
    }

    #[test]
    fn primary_label_carries_the_diagnostic_message_and_anchor() {
        let label = Label::new(Some(AstNodeId::INVALID), "missing name");
        let diagnostic = Diagnostic::new(Severity::Error, UNDEFINED_NAME, label);

        assert_eq!(diagnostic.message(), "missing name");
        assert_eq!(diagnostic.primary.node, Some(AstNodeId::INVALID));
        assert!(diagnostic.secondary.is_empty());
    }

    #[test]
    fn captures_primary_and_secondary_positions_in_their_labels() {
        let (files, file) = parsed_file();
        let mut diagnostic = Diagnostic::new(
            Severity::Error,
            DUPLICATE_DECLARATION,
            Label::new(Some(file.decls[0].node_id()), "duplicate declaration"),
        )
        .with_secondary(Label::new(
            Some(file.name.node_id()),
            "previous declaration is here",
        ));
        diagnostic.capture_positions(|id| locate(&file, id));

        assert_eq!(diagnostic.position(&files).line, 2);
        assert_eq!(
            diagnostic.display_with(&files).to_string(),
            "error[E2002]: duplicate declaration\n --> main.go:2:1\n  = note: previous declaration is here\n     --> main.go:1:9\n"
        );
    }

    #[test]
    fn collection_captures_positions_sorts_and_deduplicates() {
        let (files, file) = parsed_file();
        let mut diagnostics = Diagnostics::default();
        diagnostics.error(UNDEFINED_NAME, Some(file.decls[0].node_id()), "missing");
        diagnostics.error(UNDEFINED_NAME, Some(file.name.node_id()), "first");
        diagnostics.error(UNDEFINED_NAME, Some(file.name.node_id()), "first");
        diagnostics.capture_positions(|id| locate(&file, id));

        let diagnostics = diagnostics.finish();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].message(), "first");
        assert_eq!(diagnostics[0].position(&files).column, 9);
    }

    #[test]
    fn diagnostics_without_an_anchor_render_an_invalid_position() {
        let files = FileSet::new();
        let diagnostic =
            Diagnostic::new(Severity::Error, UNDEFINED_NAME, Label::new(None, "missing"));

        assert!(!diagnostic.position(&files).is_valid());
    }
}
