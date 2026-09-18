//! Shared diagnostic data structures for the Gane toolchain.
//!
//! Labels retain their source anchor until callers resolve it to a user-facing
//! parser [`Position`].

use std::cmp::Ordering;
use std::fmt;
use std::ops::{BitAnd, BitOr};

use colored::Colorize;
use gane_parser::{
    ErrorList,
    token::{AstNodeId, FileSet, Pos, Position},
};

/// Controls diagnostic rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticMode(u8);

impl DiagnosticMode {
    pub const PLAIN: Self = Self(0);
    pub const COLOR: Self = Self(1 << 0);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitAnd for DiagnosticMode {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitOr for DiagnosticMode {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

pub const PARSER_ERROR: DiagnosticCode = DiagnosticCode("E1001");

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

/// A label's source location before or after resolution.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum Source {
    AstNode(AstNodeId),
    Pos(Pos),
    Position(Position),
}

/// An error resolving a label's source anchor to a source position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveError {
    MissingAstNode(AstNodeId),
    InvalidPos(Pos),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAstNode(node) => {
                write!(formatter, "cannot resolve source node {}", node.raw())
            }
            Self::InvalidPos(pos) => write!(formatter, "cannot resolve source position {pos}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// A diagnostic location with an unresolved or resolved source anchor.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Label {
    pub message: String,
    source: Source,
}

impl Label {
    pub fn from_node(node: AstNodeId, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: Source::AstNode(node),
        }
    }

    pub fn from_pos(pos: Pos, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: Source::Pos(pos),
        }
    }

    pub fn from_position(position: Position, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: Source::Position(position),
        }
    }

    pub fn ast_node(&self) -> Option<AstNodeId> {
        match &self.source {
            Source::AstNode(node) => Some(*node),
            Source::Pos(_) | Source::Position(_) => None,
        }
    }

    fn resolve_position(
        &mut self,
        files: &FileSet,
        locate: &mut impl FnMut(AstNodeId) -> Option<Pos>,
    ) -> Result<(), ResolveError> {
        let pos = match &self.source {
            Source::AstNode(node) => locate(*node).ok_or(ResolveError::MissingAstNode(*node))?,
            Source::Pos(pos) => *pos,
            Source::Position(_) => return Ok(()),
        };
        if files.file(pos).is_none() {
            return Err(ResolveError::InvalidPos(pos));
        }
        self.source = Source::Position(files.position(pos));
        Ok(())
    }

    pub fn position(&self) -> Result<&Position, ResolveError> {
        match &self.source {
            Source::Position(position) => Ok(position),
            Source::AstNode(node) => Err(ResolveError::MissingAstNode(*node)),
            Source::Pos(pos) => Err(ResolveError::InvalidPos(*pos)),
        }
    }
}

/// A user-visible diagnostic anchored to parser-assigned AST node identities.
///
/// Source anchors must be resolved before source-aware rendering.
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

    fn resolve_positions(
        &mut self,
        files: &FileSet,
        locate: &mut impl FnMut(AstNodeId) -> Option<Pos>,
    ) -> Result<(), ResolveError> {
        self.primary.resolve_position(files, locate)?;
        for label in &mut self.secondary {
            label.resolve_position(files, locate)?;
        }
        Ok(())
    }

    pub fn position(&self) -> Result<&Position, ResolveError> {
        self.primary.position()
    }

    pub fn message(&self) -> &str {
        &self.primary.message
    }

    /// Returns a source-aware renderer after every label has been resolved.
    pub fn display<'a>(
        &'a self,
        mode: DiagnosticMode,
    ) -> Result<impl fmt::Display + 'a, ResolveError> {
        self.primary.position()?;
        for label in &self.secondary {
            label.position()?;
        }
        Ok(DiagnosticDisplay {
            diagnostic: self,
            mode,
        })
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
    mode: DiagnosticMode,
}

impl fmt::Display for DiagnosticDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let diagnostic = self.diagnostic;
        if self.mode.contains(DiagnosticMode::COLOR) {
            let severity = severity_name(diagnostic.severity);
            let heading = format!("{severity}[{}]: {}", diagnostic.code, diagnostic.message());
            writeln!(
                f,
                "{}",
                match diagnostic.severity {
                    Severity::Error => heading.red().bold().to_string(),
                    Severity::Warning => heading.yellow().bold().to_string(),
                    Severity::Note => heading.cyan().to_string(),
                    Severity::Help => heading.green().to_string(),
                }
            )?;
        } else {
            writeln!(f, "{diagnostic}")?;
        }
        writeln!(
            f,
            " --> {}",
            diagnostic.position().expect("validated diagnostic")
        )?;

        for label in &diagnostic.secondary {
            let position = label.position().expect("validated diagnostic");
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

/// In-memory collection of diagnostics under construction.
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
            label_from_node(primary, message),
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
            label_from_node(primary, message),
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
            label_from_node(primary, message),
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

    /// Sorts deterministically, removes exact duplicates, and returns the items.
    pub fn finish(mut self) -> Vec<Diagnostic> {
        self.items.sort_by(diagnostic_order);
        self.items.dedup();
        self.items
    }
}

fn label_from_node(node: Option<AstNodeId>, message: impl Into<String>) -> Label {
    match node {
        Some(node) => Label::from_node(node, message),
        None => Label::from_position(Position::default(), message),
    }
}

impl From<&ErrorList> for Diagnostics {
    fn from(errors: &ErrorList) -> Self {
        let mut diagnostics = Self::default();
        for error in errors {
            diagnostics.push(Diagnostic::new(
                Severity::Error,
                PARSER_ERROR,
                Label::from_position(error.pos.clone(), error.msg.clone()),
            ));
        }
        diagnostics
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// Resolves every diagnostic atomically using the AST locator and file set.
pub fn resolve_positions(
    diagnostics: &mut Vec<Diagnostic>,
    files: &FileSet,
    mut locate: impl FnMut(AstNodeId) -> Option<Pos>,
) -> Result<(), ResolveError> {
    let mut resolved = diagnostics.clone();
    for diagnostic in &mut resolved {
        diagnostic.resolve_positions(files, &mut locate)?;
    }
    *diagnostics = resolved;
    Ok(())
}

fn diagnostic_order(a: &Diagnostic, b: &Diagnostic) -> Ordering {
    a.primary
        .source
        .cmp(&b.primary.source)
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
        token::{FileSet, NO_POS},
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
        let label = Label::from_node(AstNodeId::INVALID, "missing name");
        let diagnostic = Diagnostic::new(Severity::Error, UNDEFINED_NAME, label);

        assert_eq!(diagnostic.message(), "missing name");
        assert_eq!(
            diagnostic.position(),
            Err(ResolveError::MissingAstNode(AstNodeId::INVALID))
        );
        assert!(diagnostic.secondary.is_empty());
    }

    #[test]
    fn resolves_primary_and_secondary_positions_in_their_labels() {
        let (files, file) = parsed_file();
        let mut diagnostics = vec![
            Diagnostic::new(
                Severity::Error,
                DUPLICATE_DECLARATION,
                Label::from_node(file.decls[0].node_id(), "duplicate declaration"),
            )
            .with_secondary(Label::from_node(
                file.name.node_id(),
                "previous declaration is here",
            )),
        ];
        resolve_positions(&mut diagnostics, &files, |id| locate(&file, id)).unwrap();
        let diagnostic = &diagnostics[0];

        assert_eq!(diagnostic.position().unwrap().line, 2);
        assert_eq!(
            diagnostic
                .display(DiagnosticMode::PLAIN)
                .unwrap()
                .to_string(),
            "error[E2002]: duplicate declaration\n --> main.go:2:1\n  = note: previous declaration is here\n     --> main.go:1:9\n"
        );
    }

    #[test]
    fn collection_resolves_positions_sorts_and_deduplicates() {
        let (files, file) = parsed_file();
        let mut diagnostics = Diagnostics::default();
        diagnostics.error(UNDEFINED_NAME, Some(file.decls[0].node_id()), "missing");
        diagnostics.error(UNDEFINED_NAME, Some(file.name.node_id()), "first");
        diagnostics.error(UNDEFINED_NAME, Some(file.name.node_id()), "first");

        let mut diagnostics = diagnostics.finish();
        resolve_positions(&mut diagnostics, &files, |id| locate(&file, id)).unwrap();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].message(), "first");
        assert_eq!(diagnostics[0].position().unwrap().column, 9);
    }

    #[test]
    fn unresolved_labels_return_errors() {
        let files = FileSet::new();
        let mut diagnostics = vec![Diagnostic::new(
            Severity::Error,
            UNDEFINED_NAME,
            Label::from_pos(NO_POS, "missing"),
        )];

        assert_eq!(
            resolve_positions(&mut diagnostics, &files, |_| None),
            Err(ResolveError::InvalidPos(NO_POS))
        );
        assert!(matches!(
            diagnostics[0].display(DiagnosticMode::PLAIN),
            Err(ResolveError::InvalidPos(pos)) if pos == NO_POS
        ));
    }

    #[test]
    fn failed_batch_resolution_leaves_every_label_unresolved() {
        let mut files = FileSet::new();
        let file = files.add_file("main.go", -1, 1);
        let valid = file.pos(0);
        let mut diagnostics = vec![
            Diagnostic::new(
                Severity::Error,
                UNDEFINED_NAME,
                Label::from_pos(valid, "first"),
            ),
            Diagnostic::new(
                Severity::Error,
                UNDEFINED_NAME,
                Label::from_pos(NO_POS, "second"),
            ),
        ];

        assert_eq!(
            resolve_positions(&mut diagnostics, &files, |_| None),
            Err(ResolveError::InvalidPos(NO_POS))
        );
        assert!(matches!(
            diagnostics[0].position(),
            Err(ResolveError::InvalidPos(pos)) if pos == valid
        ));
    }

    #[test]
    fn parser_errors_become_source_positioned_diagnostics() {
        let errors = ErrorList::from_iter([gane_parser::Error::new(
            Position {
                file_name: "main.go".into(),
                line: 3,
                column: 7,
                ..Position::default()
            },
            "unexpected token",
        )]);
        let diagnostics = Diagnostics::from(&errors);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = diagnostics.iter().next().unwrap();
        assert_eq!(diagnostic.code, PARSER_ERROR);
        assert_eq!(diagnostic.message(), "unexpected token");
        assert_eq!(diagnostic.position().unwrap().line, 3);
    }

    #[test]
    fn color_mode_styles_the_diagnostic_heading() {
        colored::control::set_override(true);
        let diagnostic = Diagnostic::new(
            Severity::Error,
            UNDEFINED_NAME,
            Label::from_position(Position::default(), "missing"),
        );
        let rendered = diagnostic
            .display(DiagnosticMode::COLOR)
            .unwrap()
            .to_string();
        assert!(rendered.contains("\u{1b}["));
        colored::control::unset_override();
    }
}
