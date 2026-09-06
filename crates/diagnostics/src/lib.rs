//! Shared diagnostic data structures for the Gane toolchain.
//!
//! Source locations are represented by the parser's [`FileSet`] and [`Pos`],
//! which remain the single source of truth for file and line information.

use std::cmp::Ordering;
use std::fmt;

use gane_parser::token::{FileSet, Pos, Position};

/// A half-open source range represented in the parser's file set.
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

/// A source range explaining or qualifying a diagnostic.
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

/// One user-visible diagnostic with a primary source range.
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

    /// Returns a diagnostic renderer that resolves source positions through
    /// the parser's file set.
    pub fn display_with<'a>(&'a self, files: &'a FileSet) -> DiagnosticDisplay<'a> {
        DiagnosticDisplay {
            diagnostic: self,
            files,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", severity_name(self.severity), self.code, self.message)
    }
}

/// A source-aware [`Diagnostic`] renderer.
///
/// [`Diagnostic`] itself cannot render line and column information because a
/// [`FileSet`] is owned by the parsing or loading context, not by a diagnostic.
pub struct DiagnosticDisplay<'a> {
    diagnostic: &'a Diagnostic,
    files: &'a FileSet,
}

impl fmt::Display for DiagnosticDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let diagnostic = self.diagnostic;
        writeln!(f, "{diagnostic}")?;
        writeln!(f, " --> {}", diagnostic.position(self.files))?;

        for label in &diagnostic.secondary {
            writeln!(
                f,
                "  = note: {}\n     --> {}",
                label.message,
                label.span.start_position(self.files)
            )?;
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

/// A destination for diagnostics emitted by parser adapters, loaders, and sema.
pub trait DiagnosticSink {
    fn emit(&mut self, diagnostic: Diagnostic);
}

impl<T: DiagnosticSink + ?Sized> DiagnosticSink for &mut T {
    fn emit(&mut self, diagnostic: Diagnostic) {
        (**self).emit(diagnostic);
    }
}

/// In-memory diagnostics collection.
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

    /// Sorts deterministically, removes exact duplicates, and returns the items.
    pub fn finish(mut self) -> Vec<Diagnostic> {
        self.items.sort_by(diagnostic_order);
        self.items.dedup();
        self.items
    }
}

impl DiagnosticSink for Diagnostics {
    fn emit(&mut self, diagnostic: Diagnostic) {
        self.push(diagnostic);
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

    const UNDEFINED_NAME: DiagnosticCode = DiagnosticCode("E2001");
    const DUPLICATE_DECLARATION: DiagnosticCode = DiagnosticCode("E2002");

    #[test]
    fn finish_sorts_and_deduplicates() {
        let mut files = FileSet::new();
        let file = files.add_file("main.go", -1, 20);
        let first = Span::point(file.pos(2));
        let second = Span::point(file.pos(8));
        let mut diagnostics = Diagnostics::default();
        diagnostics.error(UNDEFINED_NAME, second, "missing");
        diagnostics.error(UNDEFINED_NAME, first, "first");
        diagnostics.error(UNDEFINED_NAME, first, "first");

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
        let diagnostic =
            Diagnostic::new(Severity::Error, DUPLICATE_DECLARATION, primary, "duplicate")
                .with_secondary(related);

        assert_eq!(diagnostic.secondary.len(), 1);
        assert_eq!(diagnostic.position(&files).line, 2);
        assert_eq!(diagnostic.position(&files).column, 2);
    }

    #[test]
    fn invalid_position_is_safe() {
        let files = FileSet::new();
        assert!(!Span::point(Pos::default())
            .start_position(&files)
            .is_valid());
    }

    #[test]
    fn sink_receives_diagnostics() {
        fn report(sink: &mut dyn DiagnosticSink, span: Span) {
            sink.emit(Diagnostic::new(
                Severity::Error,
                UNDEFINED_NAME,
                span,
                "missing",
            ));
        }

        let mut diagnostics = Diagnostics::default();
        report(&mut diagnostics, Span::point(Pos::default()));
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn display_renders_summary_and_source_aware_labels() {
        let mut files = FileSet::new();
        let file = files.add_file("main.go", -1, 24);
        file.set_lines_for_content(b"var x int\nvar x string\n");
        let first = Span::point(file.pos(4));
        let second = Span::point(file.pos(14));
        let diagnostic = Diagnostic::new(
            Severity::Error,
            DUPLICATE_DECLARATION,
            second,
            "duplicate declaration of `x`",
        )
        .with_secondary(Label::new(first, "previous declaration is here"));

        assert_eq!(
            diagnostic.to_string(),
            "error[E2002]: duplicate declaration of `x`"
        );
        assert_eq!(
            diagnostic.display_with(&files).to_string(),
            "error[E2002]: duplicate declaration of `x`\n --> main.go:2:5\n  = note: previous declaration is here\n     --> main.go:1:5\n"
        );
    }
}
