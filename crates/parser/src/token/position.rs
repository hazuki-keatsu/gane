//! Source position tracking
//!
//! This module is ported from Go's standard `go/token` package
//! adapted to Rust conventions:
//! - single-threaded [`Rc`]/[`RefCell`] instead of `sync.Mutex`/`atomic.Pointer`;
//! - the AVL tree of files is replaced by a `BTreeMap` keyed by file base offset
//!   (same ascending-base order semantics);
//! - methods take `&self` (except mutations of the file set itself).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

// If debug is set, invalid offset and position values cause a panic
const DEBUG: bool = false;

// Positions

/// An arbitrary source position including the file, line, and column location.
/// A `Position` is valid if the line number is > 0.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub file_name: String, // file name, if any
    pub offset: i64,       // offset, starting at 0
    pub line: i64,         // line number, starting at 1
    pub column: i64,       // column number, starting at 1 (byte count)
}

impl Position {
    /// Reports whether the position is valid.
    pub fn is_valid(&self) -> bool {
        self.line > 0
    }
}

// String returns a string in one of several forms:
//
// file:line:column    valid position with file name
// file:line           valid position with file name but no column (column == 0)
// line:column         valid position without file name
// line                valid position without file name and no column (column == 0)
// file                invalid position with file name
// -                   invalid position without file name
impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = self.file_name.clone();
        if self.is_valid() {
            if !s.is_empty() {
                s.push(':');
            }
            s.push_str(&self.line.to_string());
            if self.column != 0 {
                s.push(':');
                s.push_str(&self.column.to_string());
            }
        }
        if s.is_empty() {
            s.push('-');
        }
        write!(f, "{s}")
    }
}

/// A compact encoding of a source position within a file set. It can be
/// converted into a [`Position`] for a more convenient but much larger
/// representation.
///
/// The `Pos` value for a given file is a number in the range [base, base+size],
/// where base and size are specified when a file is added to the file set.
/// The difference between a `Pos` value and the corresponding file base
/// corresponds to the byte offset of that position (represented by the `Pos`
/// value) from the beginning of the file. Thus, the file base offset is the
/// `Pos` value representing the first byte in the file.
///
/// To create the `Pos` value for a specific source offset (measured in bytes),
/// first add the respective file to the current file set using
/// [`FileSet::add_file`] and then call [`File::pos`] for that file.
/// Given a `Pos` value p for a specific file set fset, the corresponding
/// [`Position`] value is obtained by calling `fset.position(p)`.
///
/// `Pos` values can be compared directly with the usual comparison operators:
/// if two `Pos` values p and q are in the same file, comparing p and q is
/// equivalent to comparing the respective source file offsets. If p and q are
/// in different files, p < q is true if the file implied by p was added to the
/// respective file set before the file implied by q.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos(i64);

/// The zero value for [`Pos`] is `NoPos`; there is no file and line information
/// associated with it, and `NoPos.is_valid()` is false. `NoPos` is always
/// smaller than any other `Pos` value. The corresponding [`Position`] value
/// for `NoPos` is the zero value for [`Position`].
pub const NoPos: Pos = Pos(0);

impl Pos {
    /// Reports whether the position is valid.
    pub fn is_valid(self) -> bool {
        self != NoPos
    }

    /// Constructs a `Pos` from a raw integer.
    ///
    /// Crate-internal only: the `go/ast` port needs to build positions from
    /// raw offsets (mirroring Go's `token.Pos(n)` conversions).
    pub(crate) const fn from_int(n: i64) -> Pos {
        Pos(n)
    }
}

// Pos arithmetic: Go's `ast` code does raw integer arithmetic on token.Pos
// (e.g. `End() = p + int(len(...))`), and Go tests write token.Pos(n) directly.
impl core::ops::Add<i64> for Pos {
    type Output = Pos;

    fn add(self, n: i64) -> Pos {
        Pos(self.0 + n)
    }
}

impl core::ops::Sub<i64> for Pos {
    type Output = Pos;

    fn sub(self, n: i64) -> Pos {
        Pos(self.0 - n)
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// File

/// A handle for a file belonging to a [`FileSet`].
/// A `File` has a name, size, and line offset table.
///
/// Use [`FileSet::add_file`] to create a `File`.
#[derive(Debug)]
pub struct File {
    name: String, // file name as provided to add_file
    base: i64,    // Pos value range for this file is [base...base+size]
    size: i64,    // file size as provided to add_file

    // lines and infos are protected by RefCell
    inner: RefCell<FileInner>,
}

#[derive(Debug, Default)]
struct FileInner {
    // lines contains the offset of the first character for each line
    // (the first entry is always 0)
    lines: Vec<i64>,
    infos: Vec<LineInfo>,
}

/// Describes alternative file, line, and column number information
/// (such as provided via a `//line` directive) for a given file offset.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LineInfo {
    offset: i64,
    file_name: String,
    line: i64,
    column: i64,
}

impl fmt::Display for File {
    /// Returns a brief description of the `File`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({}-{})", self.name, self.base, self.end())
    }
}

impl File {
    /// Returns the file name of this `File` as registered with [`FileSet::add_file`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the base offset of this `File` as registered with [`FileSet::add_file`].
    pub fn base(&self) -> i64 {
        self.base
    }

    /// Returns the size of this `File` as registered with [`FileSet::add_file`].
    pub fn size(&self) -> i64 {
        self.size
    }

    /// Returns the end position of this `File` as registered with [`FileSet::add_file`].
    pub fn end(&self) -> Pos {
        Pos(self.base + self.size)
    }

    /// Returns the number of lines in this `File`.
    pub fn line_count(&self) -> usize {
        self.inner.borrow().lines.len()
    }

    /// Adds the line offset for a new line. The line offset must be larger than
    /// the offset for the previous line and smaller than the file size;
    /// otherwise the line offset is ignored.
    pub fn add_line(&self, offset: i64) {
        let mut inner = self.inner.borrow_mut();
        let n = inner.lines.len();
        if (n == 0 || inner.lines[n - 1] < offset) && offset < self.size {
            inner.lines.push(offset);
        }
    }

    /// Merges a line with the following line. It is akin to replacing the
    /// newline character at the end of the line with a space (to not change the
    /// remaining offsets). To obtain the line number, consult e.g.
    /// [`Position::line`]. `merge_line` will panic if given an invalid line number.
    pub fn merge_line(&self, line: i64) {
        if line < 1 {
            panic!("invalid line number {line} (should be >= 1)");
        }
        let mut inner = self.inner.borrow_mut();
        if line as usize >= inner.lines.len() {
            panic!(
                "invalid line number {line} (should be < {})",
                inner.lines.len()
            );
        }
        // To merge the line numbered <line> with the line numbered <line+1>,
        // we need to remove the entry in lines corresponding to the line
        // numbered <line+1>. The entry in lines corresponding to the line
        // numbered <line+1> is located at index <line>, since indices in lines
        // are 0-based and line numbers are 1-based.
        inner.lines.remove(line as usize);
    }

    /// Returns the effective line offset table of the form described by
    /// [`File::set_lines`].
    pub fn lines(&self) -> Vec<i64> {
        self.inner.borrow().lines.clone()
    }

    /// Sets the line offsets for a file and reports whether it succeeded.
    /// The line offsets are the offsets of the first character of each line;
    /// for instance for the content `"ab\nc\n"` the line offsets are `{0, 3}`.
    /// An empty file has an empty line offset table.
    /// Each line offset must be larger than the offset for the previous line
    /// and smaller than the file size; otherwise `set_lines` fails and returns
    /// false.
    pub fn set_lines(&self, lines: &[i64]) -> bool {
        // verify validity of lines table
        let size = self.size;
        for (i, &offset) in lines.iter().enumerate() {
            if (i > 0 && offset <= lines[i - 1]) || size <= offset {
                return false;
            }
        }
        // set lines table
        self.inner.borrow_mut().lines = lines.to_vec();
        true
    }

    /// Sets the line offsets for the given file content.
    /// It ignores position-altering `//line` comments.
    pub fn set_lines_for_content(&self, content: &[u8]) {
        let mut lines: Vec<i64> = Vec::new();
        let mut line: i64 = 0;
        for (offset, &b) in content.iter().enumerate() {
            if line >= 0 {
                lines.push(line);
            }
            line = -1;
            if b == b'\n' {
                line = offset as i64 + 1;
            }
        }
        // set lines table
        self.inner.borrow_mut().lines = lines;
    }

    /// Returns the [`Pos`] value of the start of the specified line.
    /// It ignores any alternative positions set using [`File::add_line_column_info`].
    /// `line_start` panics if the 1-based line number is invalid.
    pub fn line_start(&self, line: i64) -> Pos {
        if line < 1 {
            panic!("invalid line number {line} (should be >= 1)");
        }
        let inner = self.inner.borrow();
        if line > inner.lines.len() as i64 {
            panic!(
                "invalid line number {line} (should be < {})",
                inner.lines.len()
            );
        }
        Pos(self.base + inner.lines[line as usize - 1])
    }

    /// Like [`File::add_line_column_info`] with a column = 1 argument.
    /// It is here for backward-compatibility for code prior to Go 1.11.
    pub fn add_line_info(&self, offset: i64, file_name: &str, line: i64) {
        self.add_line_column_info(offset, file_name, line, 1);
    }

    /// Adds alternative file, line, and column number information for a given
    /// file offset. The offset must be larger than the offset for the previously
    /// added alternative line info and smaller than the file size; otherwise
    /// the information is ignored.
    ///
    /// `add_line_column_info` is typically used to register alternative position
    /// information for line directives such as `//line filename:line:column`.
    pub fn add_line_column_info(&self, offset: i64, file_name: &str, line: i64, column: i64) {
        let mut inner = self.inner.borrow_mut();
        let n = inner.infos.len();
        if (n == 0 || inner.infos[n - 1].offset < offset) && offset < self.size {
            inner.infos.push(LineInfo {
                offset,
                file_name: file_name.to_string(),
                line,
                column,
            });
        }
    }

    /// Fixes an out-of-bounds offset such that 0 <= offset <= f.size.
    fn fix_offset(&self, offset: i64) -> i64 {
        if DEBUG && !(0 <= offset && offset <= self.size) {
            panic!(
                "offset {offset} out of bounds [0, {}] (position {} out of bounds [{}, {}])",
                self.size,
                self.base + offset,
                self.base,
                self.base + self.size
            );
        }
        offset.clamp(0, self.size)
    }

    /// Returns the [`Pos`] value for the given file offset.
    ///
    /// If offset is negative, the result is the file's start position; if the
    /// offset is too large, the result is the file's end position (see also
    /// go.dev/issue/57490).
    ///
    /// The following invariant, though not true for `Pos` values in general,
    /// holds for the result p: `f.pos(f.offset(p)) == p`.
    pub fn pos(&self, offset: i64) -> Pos {
        Pos(self.base + self.fix_offset(offset))
    }

    /// Returns the offset for the given file position p.
    ///
    /// If p is before the file's start position (or if p is `NoPos`),
    /// the result is 0; if p is past the file's end position, the result is
    /// the file size (see also go.dev/issue/57490).
    ///
    /// The following invariant, though not true for offset values in general,
    /// holds for the result offset: `f.offset(f.pos(offset)) == offset`.
    pub fn offset(&self, p: Pos) -> i64 {
        self.fix_offset(p.0 - self.base)
    }

    /// Returns the line number for the given file position p;
    /// p must be a [`Pos`] value in that file or `NoPos`.
    pub fn line(&self, p: Pos) -> i64 {
        self.position(p).line
    }

    /// Returns the file name and line and column number for a file offset.
    /// If adjusted is set, the result will contain the file name and line
    /// information possibly adjusted by `//line` comments; otherwise those
    /// comments are ignored.
    fn unpack(&self, offset: i64, adjusted: bool) -> (String, i64, i64) {
        let inner = self.inner.borrow();
        let mut file_name = self.name.clone();
        let mut line = 0;
        let mut column = 0;
        if let i @ 0.. = search_ints(&inner.lines, offset) {
            line = i + 1;
            column = offset - inner.lines[i as usize] + 1;
        }
        if adjusted && !inner.infos.is_empty() {
            // few files have extra line infos
            if let i @ 0.. = search_line_infos(&inner.infos, offset) {
                let alt = &inner.infos[i as usize];
                file_name = alt.file_name.clone();
                if let k @ 0.. = search_ints(&inner.lines, alt.offset) {
                    // k+1 is the line at which the alternative position was recorded
                    let d = line - (k + 1); // line distance from alternative position base
                    line = alt.line + d;
                    if alt.column == 0 {
                        // alternative column is unknown => relative column is unknown
                        // (the current specification for line directives requires
                        // this to apply until the next PosBase/line directive,
                        // not just until the new newline)
                        column = 0;
                    } else if d == 0 {
                        // the alternative position base is on the current line
                        // => column is relative to alternative column
                        column = alt.column + (offset - alt.offset);
                    }
                }
            }
        }
        (file_name, line, column)
    }

    fn position_adj(&self, p: Pos, adjusted: bool) -> Position {
        let offset = self.fix_offset(p.0 - self.base);
        let (file_name, line, column) = self.unpack(offset, adjusted);
        Position {
            file_name,
            offset,
            line,
            column,
        }
    }

    /// Returns the [`Position`] value for the given file position p.
    /// If p is out of bounds, it is adjusted to match the [`File::offset`]
    /// behavior. If adjusted is set, the position may be adjusted by
    /// position-altering `//line` comments; otherwise those comments are ignored.
    /// p must be a [`Pos`] value in this file or `NoPos`.
    pub fn position_for(&self, p: Pos, adjusted: bool) -> Position {
        if p != NoPos {
            self.position_adj(p, adjusted)
        } else {
            Position::default()
        }
    }

    /// Returns the [`Position`] value for the given file position p.
    /// If p is out of bounds, it is adjusted to match the [`File::offset`]
    /// behavior. Calling `f.position(p)` is equivalent to calling
    /// `f.position_for(p, true)`.
    pub fn position(&self, p: Pos) -> Position {
        self.position_for(p, true)
    }
}

// -----------------------------------------------------------------------------
// FileSet

/// A `FileSet` represents a set of source files.
///
/// The byte offsets for each file in a file set are mapped into distinct
/// (integer) intervals, one interval [base, base+size] per file.
/// [`FileSet::base`] represents the first byte in the file, and size is the
/// corresponding file size. A [`Pos`] value is a value in such an interval.
/// By determining the interval a [`Pos`] value belongs to, the file, its file
/// base, and thus the byte offset (position) the [`Pos`] value is representing
/// can be computed.
///
/// When adding a new file, a file base must be provided. That can be any
/// integer value that is past the end of any interval of any file already in
/// the file set. For convenience, [`FileSet::base`] provides such a value,
/// which is simply the end of the `Pos` interval of the most recently added
/// file, plus one. Unless there is a need to extend an interval later, using
/// the [`FileSet::base`] should be used as argument for [`FileSet::add_file`].
///
/// A [`File`] may be removed from a `FileSet` when it is no longer needed.
/// This may reduce memory usage in a long-running application.
///
/// Unlike Go, this port is not thread-safe: files are held as `Rc<File>`, and
/// `iterate` must not mutate the file set from its callback.
#[derive(Debug, Default)]
pub struct FileSet {
    base: i64,                       // base offset for the next file
    files: BTreeMap<i64, Rc<File>>,  // files in ascending base order (keys are file base offsets)
    last: RefCell<Option<Rc<File>>>, // cache of last file looked up
}

impl FileSet {
    /// Creates a new file set.
    pub fn new() -> FileSet {
        FileSet {
            base: 1, // 0 == NoPos
            ..FileSet::default()
        }
    }

    /// Returns the minimum base offset that must be provided to
    /// [`FileSet::add_file`] when adding the next file.
    pub fn base(&self) -> i64 {
        self.base
    }

    /// Adds a new file with a given file name, base offset, and file size to
    /// the file set and returns the file. Multiple files may have the same
    /// name. The base offset must not be smaller than the [`FileSet::base`],
    /// and size must not be negative. As a special case, if a negative base is
    /// provided, the current value of the [`FileSet::base`] is used instead.
    ///
    /// Adding the file will set the file set's [`FileSet::base`] value to
    /// `base + size + 1` as the minimum base value for the next file. The
    /// following relationship exists between a [`Pos`] value p for a given
    /// file offset offs:
    ///
    /// `p = base + offs`
    ///
    /// with offs in the range [0, size] and thus p in the range [base, base+size].
    /// For convenience, [`File::pos`] may be used to create file-specific
    /// position values from a file offset.
    pub fn add_file(&mut self, file_name: &str, mut base: i64, size: i64) -> Rc<File> {
        if base < 0 {
            base = self.base;
        }
        if base < self.base {
            panic!("invalid base {base} (should be >= {})", self.base);
        }
        if size < 0 {
            panic!("invalid size {size} (should be >= 0)");
        }
        // base >= s.base && size >= 0
        let next_base = base
            .checked_add(size)
            .and_then(|b| b.checked_add(1)) // +1 because EOF also has a position
            .expect("token.Pos offset overflow (> 2G of source code in file set)");
        // add the file to the file set
        self.base = next_base;
        let f = Rc::new(File {
            name: file_name.to_string(),
            base,
            size,
            inner: RefCell::new(FileInner {
                lines: vec![0],
                infos: Vec::new(),
            }),
        });
        self.files.insert(base, f.clone());
        *self.last.borrow_mut() = Some(f.clone());
        f
    }

    /// Adds the specified files to the `FileSet` if they are not already
    /// present. The caller must ensure that no pair of files that would appear
    /// in the resulting `FileSet` overlap.
    ///
    /// All calls to `add_file` must be in increasing order, so files created
    /// from another file set cannot be re-added one by one. `add_existing_files`
    /// lets us augment an existing `FileSet` sequentially, so long as all sets
    /// of files have disjoint ranges.
    pub fn add_existing_files(&mut self, files: &[Rc<File>]) {
        for f in files {
            self.files.insert(f.base, f.clone());
            self.base = self.base.max(f.base + f.size + 1);
        }
    }

    /// Removes a file from the `FileSet` so that subsequent queries for its
    /// [`Pos`] interval yield a negative result. This reduces the memory usage
    /// of a long-lived `FileSet` that encounters an unbounded stream of files.
    ///
    /// Removing a file that does not belong to the set has no effect.
    pub fn remove_file(&mut self, file: &Rc<File>) {
        // clear the last file cache if it refers to the removed file
        if self
            .last
            .borrow()
            .as_ref()
            .is_some_and(|last| Rc::ptr_eq(last, file))
        {
            *self.last.borrow_mut() = None;
        }
        if self
            .files
            .get(&file.base)
            .is_some_and(|f| Rc::ptr_eq(f, file))
        {
            self.files.remove(&file.base);
        }
    }

    /// Calls `yield_fn` for the files in the file set in ascending `base`
    /// order until `yield_fn` returns false.
    ///
    /// Note: unlike Go, the callback must not mutate the file set, since the
    /// iteration borrows it.
    pub fn iterate(&self, mut yield_fn: impl FnMut(&File) -> bool) {
        // snapshot the files so the callback only borrows the set immutably
        let files: Vec<Rc<File>> = self.files.values().cloned().collect();
        for f in files {
            if !yield_fn(&f) {
                break;
            }
        }
    }

    /// Returns the file that contains the position p, if any.
    fn lookup(&self, p: Pos) -> Option<Rc<File>> {
        // common case: p is in last file.
        if let Some(f) = self.last.borrow().as_ref() {
            if f.base <= p.0 && p.0 <= f.base + f.size {
                return Some(f.clone());
            }
        }
        // files are sorted by base; the file containing p, if any, is the one
        // with the largest base <= p.
        let (_, f) = self.files.range(..=p.0).next_back()?;
        if p.0 <= f.base + f.size {
            // update cache of last file
            *self.last.borrow_mut() = Some(f.clone());
            return Some(f.clone());
        }
        None
    }

    /// Returns the file that contains the position p.
    /// If no such file is found (for instance for p == `NoPos`), the result is `None`.
    pub fn file(&self, p: Pos) -> Option<Rc<File>> {
        if p != NoPos { self.lookup(p) } else { None }
    }

    /// Converts a [`Pos`] p in the file set into a [`Position`] value.
    /// If adjusted is set, the position may be adjusted by position-altering
    /// `//line` comments; otherwise those comments are ignored.
    /// p must be a [`Pos`] value in this file set or `NoPos`.
    pub fn position_for(&self, p: Pos, adjusted: bool) -> Position {
        if p != NoPos {
            if let Some(f) = self.lookup(p) {
                return f.position_adj(p, adjusted);
            }
        }
        Position::default()
    }

    /// Converts a [`Pos`] p in the file set into a [`Position`] value.
    /// Calling `s.position(p)` is equivalent to calling `s.position_for(p, true)`.
    pub fn position(&self, p: Pos) -> Position {
        self.position_for(p, true)
    }
}

// -----------------------------------------------------------------------------
// Helper functions

/// Returns the index of the last element in the sorted slice `a` that is
/// <= x, or -1 if there is none.
fn search_ints(a: &[i64], x: i64) -> i64 {
    a.partition_point(|&v| v <= x) as i64 - 1
}

/// Returns the index of the last `lineInfo` in the sorted slice `a` whose
/// offset is <= x, or -1 if there is none.
fn search_line_infos(a: &[LineInfo], x: i64) -> i64 {
    a.partition_point(|info| info.offset <= x) as i64 - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_pos_eq(msg: &str, got: &Position, want: &Position) {
        assert_eq!(got, want, "{msg}");
    }

    #[test]
    fn no_pos() {
        assert!(!NoPos.is_valid());
        assert!(Pos(1).is_valid());
        let fset = FileSet::new();
        let want = Position::default();
        assert_pos_eq("fset NoPos", &fset.position(NoPos), &want);
        assert_eq!(fset.position(NoPos).to_string(), "-");
    }

    struct TestCase {
        filename: &'static str,
        source: Option<&'static [u8]>, // may be None
        size: i64,
        lines: &'static [i64],
    }

    fn test_cases() -> Vec<TestCase> {
        vec![
            TestCase {
                filename: "a",
                source: Some(b""),
                size: 0,
                lines: &[],
            },
            TestCase {
                filename: "b",
                source: Some(b"01234"),
                size: 5,
                lines: &[0],
            },
            TestCase {
                filename: "c",
                source: Some(b"\n\n\n\n\n\n\n\n\n"),
                size: 9,
                lines: &[0, 1, 2, 3, 4, 5, 6, 7, 8],
            },
            TestCase {
                filename: "d",
                source: None,
                size: 100,
                lines: &[0, 5, 10, 20, 30, 70, 71, 72, 80, 85, 90, 99],
            },
            TestCase {
                filename: "e",
                source: None,
                size: 777,
                lines: &[0, 80, 100, 120, 130, 180, 267, 455, 500, 567, 620],
            },
            TestCase {
                filename: "f",
                source: Some(b"package p\n\nimport \"fmt\""),
                size: 23,
                lines: &[0, 10, 11],
            },
            TestCase {
                filename: "g",
                source: Some(b"package p\n\nimport \"fmt\"\n"),
                size: 24,
                lines: &[0, 10, 11],
            },
            TestCase {
                filename: "h",
                source: Some(b"package p\n\nimport \"fmt\"\n "),
                size: 25,
                lines: &[0, 10, 11, 24],
            },
        ]
    }

    fn linecol(lines: &[i64], offs: i64) -> (i64, i64) {
        let mut prev_line_offs = 0;
        for (line, &line_offs) in lines.iter().enumerate() {
            if offs < line_offs {
                return (line as i64, offs - prev_line_offs + 1);
            }
            prev_line_offs = line_offs;
        }
        (lines.len() as i64, offs - prev_line_offs + 1)
    }

    fn make_test_source(size: i64, lines: &[i64]) -> Vec<u8> {
        let mut src = vec![0u8; size as usize];
        for &offs in lines {
            if offs > 0 {
                src[(offs - 1) as usize] = b'\n';
            }
        }
        src
    }

    fn verify_positions(fset: &FileSet, f: &File, lines: &[i64]) {
        for offs in 0..f.size() {
            let p = f.pos(offs);
            assert_eq!(f.offset(p), offs, "{}: Offset", f.name());
            let (line, col) = linecol(lines, offs);
            let msg = format!("{} (offs = {offs}, p = {p})", f.name());
            let want = Position {
                file_name: f.name().to_string(),
                offset: offs,
                line,
                column: col,
            };
            assert_pos_eq(
                &format!("{msg} f.Position"),
                &f.position(f.pos(offs)),
                &want,
            );
            assert_pos_eq(&format!("{msg} fset.Position"), &fset.position(p), &want);
        }
    }

    #[test]
    fn positions() {
        const DELTA: i64 = 7; // a non-zero base offset increment
        let mut fset = FileSet::new();
        for tc in test_cases() {
            // verify consistency of test case
            if let Some(src) = tc.source {
                assert_eq!(
                    src.len() as i64,
                    tc.size,
                    "{}: inconsistent test case",
                    tc.filename
                );
            }

            // add file and verify name and size
            let f = fset.add_file(tc.filename, fset.base() + DELTA, tc.size);
            assert_eq!(f.name(), tc.filename);
            assert_eq!(f.size(), tc.size, "{}: file size", tc.filename);
            let found = fset.file(f.pos(0)).expect("f.pos(0) was not found in f");
            assert!(
                Rc::ptr_eq(&found, &f),
                "{}: f.pos(0) was not found in f",
                tc.filename
            );

            // add lines individually and verify all positions
            for (i, &offset) in tc.lines.iter().enumerate() {
                f.add_line(offset);
                assert_eq!(
                    f.line_count(),
                    i + 1,
                    "{}, AddLine: line count",
                    tc.filename
                );
                // adding the same offset again should be ignored
                f.add_line(offset);
                assert_eq!(
                    f.line_count(),
                    i + 1,
                    "{}, AddLine: adding dup offset ignored",
                    tc.filename
                );
                verify_positions(&fset, &f, &tc.lines[..=i]);
            }

            // add lines with set_lines and verify all positions
            assert!(f.set_lines(tc.lines), "{}: set_lines failed", tc.filename);
            assert_eq!(
                f.line_count(),
                tc.lines.len(),
                "{}, set_lines: line count",
                tc.filename
            );
            assert_eq!(
                f.lines(),
                tc.lines,
                "{}, lines after set_lines",
                tc.filename
            );
            verify_positions(&fset, &f, tc.lines);

            // add lines with set_lines_for_content and verify all positions
            let src = match tc.source {
                Some(s) => s.to_vec(),
                // no test source available - create one from scratch
                None => make_test_source(tc.size, tc.lines),
            };
            f.set_lines_for_content(&src);
            assert_eq!(
                f.line_count(),
                tc.lines.len(),
                "{}, set_lines_for_content: line count",
                tc.filename
            );
            verify_positions(&fset, &f, tc.lines);
        }
    }

    #[test]
    fn line_info() {
        let mut fset = FileSet::new();
        let f = fset.add_file("foo", fset.base(), 500);
        let lines = [0i64, 42, 77, 100, 210, 220, 277, 300, 333, 401];
        // add lines individually and provide alternative line information
        for &offs in &lines {
            f.add_line(offs);
            f.add_line_info(offs, "bar", 42);
        }
        // verify positions for all offsets
        for offs in 0..=f.size() {
            let p = f.pos(offs);
            let (_, col) = linecol(&lines, offs);
            let msg = format!("{} (offs = {offs}, p = {p})", f.name());
            let want = Position {
                file_name: "bar".to_string(),
                offset: offs,
                line: 42,
                column: col,
            };
            assert_pos_eq(
                &format!("{msg} f.Position"),
                &f.position(f.pos(offs)),
                &want,
            );
            assert_pos_eq(&format!("{msg} fset.Position"), &fset.position(p), &want);
        }
    }

    #[test]
    fn files() {
        let cases = test_cases();
        let mut fset = FileSet::new();
        for (i, tc) in cases.iter().enumerate() {
            let base = if i % 2 == 1 {
                // setting a negative base is equivalent to fset.base(),
                // so test some of each
                -1
            } else {
                fset.base()
            };
            fset.add_file(tc.filename, base, tc.size);
            let mut j = 0;
            fset.iterate(|f| {
                assert_eq!(
                    f.name(),
                    cases[j].filename,
                    "file {} at index {j}",
                    f.name()
                );
                j += 1;
                true
            });
            assert_eq!(j, i + 1, "got {j} files, want {}", i + 1);
        }
    }

    // FileSet::file should return None if Pos is past the end of the FileSet.
    #[test]
    fn file_set_past_end() {
        let mut fset = FileSet::new();
        for tc in test_cases() {
            fset.add_file(tc.filename, fset.base(), tc.size);
        }
        assert!(fset.file(Pos(fset.base())).is_none());
    }

    #[test]
    fn file_set_cache_unlikely() {
        let mut fset = FileSet::new();
        let mut offsets: Vec<(&str, i64)> = vec![];
        for tc in test_cases() {
            offsets.push((tc.filename, fset.base()));
            fset.add_file(tc.filename, fset.base(), tc.size);
        }
        for &(file, pos) in &offsets {
            let f = fset.file(Pos(pos)).expect("file not found");
            assert_eq!(f.name(), file, "at position {pos}");
        }
    }

    #[test]
    fn position_for() {
        let src = b"\nfoo\nb\nar\n//line :100\nfoobar\n//line bar:3\ndone\n";
        const FILENAME: &str = "foo";
        let mut fset = FileSet::new();
        let f = fset.add_file(FILENAME, fset.base(), src.len() as i64);
        f.set_lines_for_content(src);
        let lines = f.lines();

        // verify position info
        for (i, &offs) in lines.iter().enumerate() {
            let want = Position {
                file_name: FILENAME.to_string(),
                offset: offs,
                line: i as i64 + 1,
                column: 1,
            };
            assert_pos_eq(
                "1. PositionFor unadjusted",
                &f.position_for(f.pos(offs), false),
                &want,
            );
            assert_pos_eq(
                "1. PositionFor adjusted",
                &f.position_for(f.pos(offs), true),
                &want,
            );
            assert_pos_eq("1. Position", &f.position(f.pos(offs)), &want);
        }

        // manually add //line info on lines l1, l2
        const L1: i64 = 5;
        const L2: i64 = 7;
        f.add_line_info(lines[L1 as usize - 1], "", 100);
        f.add_line_info(lines[L2 as usize - 1], "bar", 3);

        // unadjusted position info must remain unchanged
        for (i, &offs) in lines.iter().enumerate() {
            let want = Position {
                file_name: FILENAME.to_string(),
                offset: offs,
                line: i as i64 + 1,
                column: 1,
            };
            assert_pos_eq(
                "2. PositionFor unadjusted",
                &f.position_for(f.pos(offs), false),
                &want,
            );
        }

        // adjusted position info should have changed
        for (i, &offs) in lines.iter().enumerate() {
            let line = i as i64 + 1;
            let mut want = Position {
                file_name: FILENAME.to_string(),
                offset: offs,
                line,
                column: 1,
            };
            if line >= L1 {
                want.file_name = String::new();
                want.line = line - L1 + 100;
            }
            if line >= L2 {
                want.file_name = "bar".to_string();
                want.line = line - L2 + 3;
            }
            assert_pos_eq(
                "3. PositionFor adjusted",
                &f.position_for(f.pos(offs), true),
                &want,
            );
            assert_pos_eq("3. Position", &f.position(f.pos(offs)), &want);
        }
    }

    #[test]
    fn line_start() {
        const SRC: &str = "one\ntwo\nthree\n";
        let mut fset = FileSet::new();
        let f = fset.add_file("input", -1, SRC.len() as i64);
        f.set_lines_for_content(SRC.as_bytes());
        for line in 1..=3 {
            let pos = f.line_start(line);
            let position = fset.position(pos);
            assert_eq!(position.line, line, "LineStart({line}) line");
            assert_eq!(position.column, 1, "LineStart({line}) column");
        }
    }

    #[test]
    fn remove_file() {
        let content_a = b"this\nis\nfileA";
        let content_b = b"this\nis\nfileB";
        let mut fset = FileSet::new();
        let a = fset.add_file("fileA", -1, content_a.len() as i64);
        a.set_lines_for_content(content_a);
        let b = fset.add_file("fileB", -1, content_b.len() as i64);
        b.set_lines_for_content(content_b);

        let check_pos = |fset: &FileSet, pos: Pos, want: &str| {
            let got = fset.position(pos).to_string();
            assert_eq!(got, want, "Position({pos})");
        };
        let check_num_files = |fset: &FileSet, want: usize| {
            let mut got = 0;
            fset.iterate(|_| {
                got += 1;
                true
            });
            assert_eq!(got, want, "iterate count");
        };

        let apos3 = a.pos(3);
        let bpos3 = b.pos(3);
        check_pos(&fset, apos3, "fileA:1:4");
        check_pos(&fset, bpos3, "fileB:1:4");
        check_num_files(&fset, 2);

        // after removal, queries on fileA fail
        fset.remove_file(&a);
        check_pos(&fset, apos3, "-");
        check_pos(&fset, bpos3, "fileB:1:4");
        check_num_files(&fset, 1);

        // idempotent / no effect
        fset.remove_file(&a);
        check_pos(&fset, apos3, "-");
        check_pos(&fset, bpos3, "fileB:1:4");
        check_num_files(&fset, 1);
    }

    #[test]
    fn add_line_column_info() {
        const FILENAME: &str = "test.go";
        const FILE_SIZE: i64 = 100;
        let li = |offset: i64, file_name: &str, line: i64, column: i64| LineInfo {
            offset,
            file_name: file_name.to_string(),
            line,
            column,
        };

        let cases: Vec<(&str, Vec<LineInfo>, Vec<LineInfo>)> = vec![
            (
                "normal",
                vec![
                    li(10, FILENAME, 2, 1),
                    li(50, FILENAME, 3, 1),
                    li(80, FILENAME, 4, 2),
                ],
                vec![
                    li(10, FILENAME, 2, 1),
                    li(50, FILENAME, 3, 1),
                    li(80, FILENAME, 4, 2),
                ],
            ),
            (
                "offset1 == file size",
                vec![li(FILE_SIZE, FILENAME, 2, 1)],
                vec![],
            ),
            (
                "offset1 > file size",
                vec![li(FILE_SIZE + 1, FILENAME, 2, 1)],
                vec![],
            ),
            (
                "offset2 == file size",
                vec![li(10, FILENAME, 2, 1), li(FILE_SIZE, FILENAME, 3, 1)],
                vec![li(10, FILENAME, 2, 1)],
            ),
            (
                "offset2 > file size",
                vec![li(10, FILENAME, 2, 1), li(FILE_SIZE + 1, FILENAME, 3, 1)],
                vec![li(10, FILENAME, 2, 1)],
            ),
            (
                "offset2 == offset1",
                vec![li(10, FILENAME, 2, 1), li(10, FILENAME, 3, 1)],
                vec![li(10, FILENAME, 2, 1)],
            ),
            (
                "offset2 < offset1",
                vec![li(10, FILENAME, 2, 1), li(9, FILENAME, 3, 1)],
                vec![li(10, FILENAME, 2, 1)],
            ),
        ];

        for (name, infos, want) in cases {
            let mut fs = FileSet::new();
            let f = fs.add_file(FILENAME, -1, FILE_SIZE);
            for info in &infos {
                f.add_line_column_info(info.offset, &info.file_name, info.line, info.column);
            }
            assert_eq!(&f.inner.borrow().infos, &want, "{name}");
        }
    }

    #[test]
    fn issue_57490() {
        // If debug is set, this test is expected to panic.
        const FSIZE: i64 = 5;
        let mut fset = FileSet::new();
        let base = fset.base();
        let f = fset.add_file("f", base, FSIZE);

        // out-of-bounds positions must not lead to a panic when calling f.offset
        assert_eq!(f.offset(NoPos), 0, "offset of NoPos");
        assert_eq!(f.offset(Pos(-1)), 0, "offset of -1");
        assert_eq!(f.offset(Pos(base + FSIZE + 1)), FSIZE, "offset past end");

        // out-of-bounds offsets must not lead to a panic when calling f.pos
        assert_eq!(f.pos(-1), Pos(base), "pos of -1");
        assert_eq!(f.pos(FSIZE + 1), Pos(base + FSIZE), "pos past end");

        // out-of-bounds Pos values must not lead to a panic when calling f.position
        assert_eq!(f.position(Pos(-1)).to_string(), "f:1:1");
        assert_eq!(
            f.position(Pos(FSIZE + 1)).to_string(),
            format!("f:1:{}", FSIZE + 1)
        );

        // check invariants
        const XSIZE: i64 = FSIZE + 5;
        for offset in -XSIZE..XSIZE {
            let want1 = f.offset(Pos(f.base + offset));
            assert_eq!(
                f.offset(f.pos(offset)),
                want1,
                "offset invariant at {offset}"
            );

            let want2 = f.pos(offset);
            assert_eq!(f.pos(f.offset(want2)), want2, "pos invariant at {offset}");
        }
    }

    fn fset_string(fset: &FileSet) -> String {
        let mut buf = String::from("{");
        let mut sep = "";
        fset.iterate(|f| {
            buf.push_str(&format!("{sep}{}:{}-{}", f.name(), f.base(), f.end()));
            sep = " ";
            true
        });
        buf.push('}');
        buf
    }

    #[test]
    fn add_existing_files() {
        let mut fset = FileSet::new();

        let check = |fset: &FileSet, descr: &str, want: &str| {
            let got = fset_string(fset);
            assert_eq!(got, want, "{descr}");
        };

        let file_a = fset.add_file("A", -1, 3);
        fset.add_file("B", -1, 5);
        check(&fset, "after AddFile [AB]", "{A:1-4 B:5-10}");

        fset.add_existing_files(&[]); // noop
        check(&fset, "after AddExistingFiles []", "{A:1-4 B:5-10}");

        let mut fs2 = FileSet::new();
        let file_c = fs2.add_file("C", 100, 5);
        let mut fs3 = FileSet::new();
        let file_d = fs3.add_file("D", 200, 5);
        fset.add_existing_files(&[
            file_c.clone(),
            file_a.clone(),
            file_d.clone(),
            file_c.clone(),
        ]);
        check(
            &fset,
            "after AddExistingFiles [CADC]",
            "{A:1-4 B:5-10 C:100-105 D:200-205}",
        );

        fset.add_file("E", -1, 3);
        check(
            &fset,
            "after AddFile [E]",
            "{A:1-4 B:5-10 C:100-105 D:200-205 E:206-209}",
        );
    }

    #[test]
    fn file_end() {
        let f = FileSet::new().add_file("a.go", 100, 42);
        assert_eq!(f.base(), 100);
        assert_eq!(f.end(), Pos(142));
    }

    #[test]
    fn file_string() {
        let f = FileSet::new().add_file("a.go", 100, 42);
        assert_eq!(f.to_string(), "a.go(100-142)");
    }
}
