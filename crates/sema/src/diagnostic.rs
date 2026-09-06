use std::cmp::Ordering;
use std::fmt;

use gane_parser::token::{FileSet, Pos, Position};

/// The impact of a diagnostic on analysis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// Stable identifiers for semantic diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DiagnosticCode {
    Internal,
    UndefinedName,
    DuplicateDeclaration,
    TypeMismatch,
    InvalidContext,
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::Internal => "E0000",
            Self::UndefinedName => "E0001",
            Self::DuplicateDeclaration => "E0002",
            Self::TypeMismatch => "E0003",
            Self::InvalidContext => "E0004",
        };
        f.write_str(code)
    }
}

/// A half-open source range, represented in the parser's file set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    pub const fn new(start: Pos, end: Pos) -> Self {
        Self { start, end }
    }

    pub const fn point(pos: Pos) -> Self {
        Self::new(pos, pos)
    }

    pub fn start_position(self, files: &FileSet) -> Position {
        files.position(self.start)
    }

    pub fn end_position(self, files: &FileSet) -> Position {
        files.position(self.end)
    }
}

/// A secondary source range explaining or qualifying a diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// One semantic diagnostic with a primary source range.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    pub primary: Span,
    pub secondary: Vec<Label>,
}

impl Diagnostic {
    pub fn new(
        severity: Severity,
        code: DiagnosticCode,
        primary: Span,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            primary,
            secondary: Vec::new(),
        }
    }

    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    pub fn position(&self, files: &FileSet) -> Position {
        self.primary.start_position(files)
    }
}

/// A phase-local diagnostic sink. Calling `finish` consumes and releases it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn error(&mut self, code: DiagnosticCode, primary: Span, message: impl Into<String>) {
        self.push(Diagnostic::new(Severity::Error, code, primary, message));
    }

    pub fn warning(&mut self, code: DiagnosticCode, primary: Span, message: impl Into<String>) {
        self.push(Diagnostic::new(Severity::Warning, code, primary, message));
    }

    pub fn note(&mut self, code: DiagnosticCode, primary: Span, message: impl Into<String>) {
        self.push(Diagnostic::new(Severity::Note, code, primary, message));
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

    /// Sorts deterministically, removes exact duplicates, and releases the sink.
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
        .cmp(&b.primary)
        .then(a.severity.cmp(&b.severity))
        .then(a.code.cmp(&b.code))
        .then(a.message.cmp(&b.message))
        .then(a.secondary.cmp(&b.secondary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gane_parser::token::FileSet;

    #[test]
    fn finish_sorts_and_deduplicates() {
        let mut files = FileSet::new();
        let file = files.add_file("main.go", -1, 20);
        let first = Span::point(file.pos(2));
        let second = Span::point(file.pos(8));
        let mut diagnostics = Diagnostics::default();
        diagnostics.error(DiagnosticCode::UndefinedName, second, "missing");
        diagnostics.error(DiagnosticCode::UndefinedName, first, "first");
        diagnostics.error(DiagnosticCode::UndefinedName, first, "first");

        let result = diagnostics.finish();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].message, "first");
        assert_eq!(result[1].position(&files).file_name, "main.go");
    }

    #[test]
    fn secondary_labels_and_positions_are_preserved() {
        let mut files = FileSet::new();
        let file = files.add_file("main.go", -1, 24);
        file.set_lines_for_content(b"package p\nvar x int\n");
        let primary = Span::new(file.pos(11), file.pos(12));
        let related = Label::new(Span::point(file.pos(4)), "declared here");
        let diagnostic = Diagnostic::new(
            Severity::Error,
            DiagnosticCode::DuplicateDeclaration,
            primary,
            "duplicate",
        )
        .with_secondary(related);

        assert_eq!(diagnostic.secondary.len(), 1);
        assert_eq!(diagnostic.position(&files).line, 2);
        assert_eq!(diagnostic.position(&files).column, 2);
    }

    #[test]
    fn invalid_position_is_safe() {
        let files = FileSet::new();
        assert!(
            !Span::point(Pos::default())
                .start_position(&files)
                .is_valid()
        );
    }
}
