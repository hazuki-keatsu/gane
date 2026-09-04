//! A scanner for Go source text.
//!
//! The scanner takes a `&[u8]` as source which can then be tokenized
//! through repeated calls to the [`Scanner::scan`] method.
//!
//! This module is ported from Go's standard `go/scanner` package.

use std::rc::Rc;

use unicode_general_category::{GeneralCategory, get_general_category};

use crate::token::{File, NoPos, Pos, Position, Token};

/// An error handler may be provided to [`Scanner::init`]. If a syntax error is
/// encountered and a handler was installed, the handler is called with a
/// position and an error message. The position points to the beginning of
/// the offending token.
///
/// This adapts Go's `type ErrorHandler func(pos token.Position, msg string)`
/// to a boxed closure.
pub type ErrorHandler = Box<dyn FnMut(Position, String)>;

/// A Mode value is a set of flags (or 0).
/// They control scanner behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode(u8);

/// Return comments as COMMENT tokens.
pub const SCAN_COMMENTS: Mode = Mode(1);

/// Do not automatically insert semicolons - for testing only.
const DONT_INSERT_SEMIS: Mode = Mode(2);

impl std::ops::BitAnd for Mode {
    type Output = Mode;

    fn bitand(self, rhs: Mode) -> Mode {
        Mode(self.0 & rhs.0)
    }
}

impl std::ops::BitOr for Mode {
    type Output = Mode;

    fn bitor(self, rhs: Mode) -> Mode {
        Mode(self.0 | rhs.0)
    }
}

/// A Scanner holds the scanner's internal state while processing
/// a given text. It can be allocated as part of another data
/// structure but must be initialized via [`Scanner::new`] or
/// [`Scanner::init`] before use.
pub struct Scanner<'src> {
    // immutable state
    file: Rc<File>,            // source file handle
    dir: String,               // directory portion of file.name()
    src: &'src [u8],           // source
    err: Option<ErrorHandler>, // error reporting; or None
    mode: Mode,                // scanning mode

    // scanning state
    ch: i32,            // current character; < 0 (eof) means end-of-file
    offset: usize,      // character offset
    rd_offset: usize,   // reading offset (position after current character)
    line_offset: usize, // current line offset
    insert_semi: bool,  // insert a semicolon before next newline
    nl_pos: Pos,        // position of newline in preceding comment

    end_pos_valid: bool,
    end_pos: Pos, // overrides the offset as the default end position

    // public state - ok to modify
    pub error_count: usize, // number of errors encountered
}

const bom: i32 = 0xFEFF; // byte order mark, only permitted as very first character
const eof: i32 = -1; // end of file

impl<'src> Scanner<'src> {
    /// Creates and initializes a scanner to tokenize the text `src`, setting
    /// the scanner at the beginning of `src`. The scanner uses the file
    /// `file` for position information and it adds line information for each
    /// line. It is ok to re-use the same file when re-scanning the same file
    /// as line information which is already present is ignored. Creation
    /// causes a panic if the file size does not match the src size.
    ///
    /// Calls to [`Scanner::scan`] will invoke the error handler `err` if they
    /// encounter a syntax error and `err` is not None. Also, for each error
    /// encountered, the [`Scanner::error_count`] field is incremented by one.
    /// The `mode` parameter determines how comments are handled.
    ///
    /// Note that `new` may call `err` if there is an error in the first
    /// character of the file.
    pub fn new(
        file: Rc<File>,
        src: &'src [u8],
        err: Option<ErrorHandler>,
        mode: Mode,
    ) -> Scanner<'src> {
        let mut s = Scanner {
            file: file.clone(),
            dir: String::new(),
            src: &[],
            err: None,
            mode: Mode::default(),
            ch: 0,
            offset: 0,
            rd_offset: 0,
            line_offset: 0,
            insert_semi: false,
            nl_pos: NoPos,
            end_pos_valid: false,
            end_pos: NoPos,
            error_count: 0,
        };
        s.init(file, src, err, mode);
        s
    }

    /// Prepares the scanner s to tokenize the text `src` by setting the
    /// scanner at the beginning of `src`. The scanner uses the file `file`
    /// for position information and it adds line information for each line.
    /// It is ok to re-use the same file when re-scanning the same file as
    /// line information which is already present is ignored. Init causes a
    /// panic if the file size does not match the src size.
    ///
    /// Calls to [`Scanner::scan`] will invoke the error handler `err` if they
    /// encounter a syntax error and `err` is not None. Also, for each error
    /// encountered, the [`Scanner::error_count`] field is incremented by one.
    /// The `mode` parameter determines how comments are handled.
    ///
    /// Note that init may call `err` if there is an error in the first
    /// character of the file.
    pub fn init(&mut self, file: Rc<File>, src: &'src [u8], err: Option<ErrorHandler>, mode: Mode) {
        // Explicitly initialize all fields since a scanner may be reused.
        if file.size() != src.len() as i64 {
            panic!(
                "file size ({}) does not match src len ({})",
                file.size(),
                src.len()
            );
        }

        let dir = filepath_split_dir(file.name());

        *self = Scanner {
            file,
            dir,
            src,
            err,
            mode,

            ch: ' ' as i32,
            end_pos_valid: true,
            end_pos: NoPos,

            offset: 0,
            rd_offset: 0,
            line_offset: 0,
            insert_semi: false,
            nl_pos: NoPos,
            error_count: 0,
        };

        self.next();
        if self.ch == bom {
            self.next() // ignore BOM at file beginning
        }
    }

    /// Read the next Unicode char into s.ch.
    /// s.ch < 0 means end-of-file.
    ///
    /// For optimization, there is some overlap between this method and
    /// s.scan_identifier.
    fn next(&mut self) {
        if self.rd_offset < self.src.len() {
            self.offset = self.rd_offset;
            if self.ch == '\n' as i32 {
                self.line_offset = self.offset;
                self.file.add_line(self.offset as i64);
            }
            let b = self.src[self.rd_offset];
            if b < 0x80 {
                // ASCII
                if b == 0 {
                    self.error(self.offset, "illegal character NUL");
                }
                self.ch = b as i32;
                self.rd_offset += 1;
            } else {
                // not ASCII
                let (r, w) = decode_rune(&self.src[self.rd_offset..]);
                if r == '\u{FFFD}' as i32 && w == 1 {
                    let in_ = &self.src[self.rd_offset..];
                    if self.offset == 0
                        && in_.len() >= 2
                        && ((in_[0] == 0xFF && in_[1] == 0xFE)
                            || (in_[0] == 0xFE && in_[1] == 0xFF))
                    {
                        // U+FEFF BOM at start of file, encoded as big- or little-endian
                        // UCS-2 (i.e. 2-byte UTF-16). Give specific error (go.dev/issue/71950).
                        self.error(self.offset, "illegal UTF-8 encoding (got UTF-16)");
                        self.rd_offset += in_.len(); // consume all input to avoid error cascade
                    } else {
                        self.error(self.offset, "illegal UTF-8 encoding");
                    }
                } else if r == bom && self.offset > 0 {
                    self.error(self.offset, "illegal byte order mark");
                }
                self.ch = r;
                self.rd_offset += w;
            }
        } else {
            self.offset = self.src.len();
            if self.ch == '\n' as i32 {
                self.line_offset = self.offset;
                self.file.add_line(self.offset as i64);
            }
            self.ch = eof;
        }
    }

    /// peek returns the byte following the most recently read character without
    /// advancing the scanner. If the scanner is at EOF, peek returns 0.
    fn peek(&self) -> u8 {
        if self.rd_offset < self.src.len() {
            self.src[self.rd_offset]
        } else {
            0
        }
    }

    fn error(&mut self, offs: usize, msg: impl Into<String>) {
        let pos = self.file.position(self.file.pos(offs as i64));
        if let Some(err) = &mut self.err {
            err(pos, msg.into());
        }
        self.error_count += 1;
    }

    /// End returns the position immediately after the last scanned token.
    /// If [`Scanner::scan`] has not been called yet, End returns [`NoPos`].
    pub fn end(&self) -> Pos {
        // Handles special case:
        // - Makes sure we return [NoPos], even when [Scanner::init] has consumed a BOM.
        // - When the previous token was a synthetic [Semicolon] inside a multi-line
        //   comment, we make sure end returns its ending position (i.e. prev_pos+len("\n")).
        if self.end_pos_valid {
            return self.end_pos;
        }

        // Normal case: file.pos(self.offset) represents the end of the token
        self.file.pos(self.offset as i64)
    }

    /// Scan scans the next token and returns the token position, the token,
    /// and its literal string if applicable. The source end is indicated by
    /// [`Token::EOF`].
    ///
    /// If the returned token is a literal ([`Token::Ident`], [`Token::Int`],
    /// [`Token::Float`], [`Token::Imag`], [`Token::Char`],
    /// [`Token::String`]) or [`Token::Comment`], the literal string has the
    /// corresponding value.
    ///
    /// If the returned token is a keyword, the literal string is the keyword.
    ///
    /// If the returned token is [`Token::Semicolon`], the corresponding
    /// literal string is ";" if the semicolon was present in the source,
    /// and "\n" if the semicolon was inserted because of a newline or
    /// at EOF. If the newline is within a /*...*/ comment, the SEMICOLON token
    /// is synthesized immediately after the COMMENT token; its position is that
    /// of the actual newline within the comment.
    ///
    /// If the returned token is [`Token::Illegal`], the literal string is the
    /// offending character.
    ///
    /// In all other cases, Scan returns an empty literal string.
    ///
    /// For more tolerant parsing, Scan will return a valid token if
    /// possible even if a syntax error was encountered. Thus, even
    /// if the resulting token sequence contains no illegal tokens,
    /// a client may not assume that no error occurred. Instead it
    /// must check the scanner's error_count or the number of calls
    /// of the error handler, if there was one installed.
    ///
    /// Scan adds line information to the file added to the file
    /// set with init. Token positions are relative to that file
    /// and thus relative to the file set.
    pub fn scan(&mut self) -> (Pos, Token, String) {
        'scan_again: loop {
            self.end_pos_valid = false;
            if self.nl_pos.is_valid() {
                // Return artificial ';' token after /*...*/ comment
                // containing newline, at position of first newline.
                let pos = self.nl_pos;
                self.end_pos = self.file.pos(self.file.offset(pos) + 1);
                self.end_pos_valid = true;
                self.nl_pos = NoPos;
                return (pos, Token::Semicolon, "\n".to_string());
            }

            self.skip_whitespace();

            // current token start
            let pos = self.file.pos(self.offset as i64);

            // determine token value
            let mut insert_semi = false;
            let ch = self.ch;
            let (tok, lit): (Token, String) = if is_letter(ch) {
                let lit = self.scan_identifier();
                if lit.len() > 1 {
                    // keywords are longer than one letter - avoid lookup otherwise
                    let tok = Token::lookup(&lit);
                    insert_semi = matches!(
                        tok,
                        Token::Ident
                            | Token::Break
                            | Token::Continue
                            | Token::FallThrough
                            | Token::Return
                    );
                    (tok, lit)
                } else {
                    insert_semi = true;
                    (Token::Ident, lit)
                }
            } else if is_decimal(ch) || (ch == '.' as i32 && is_decimal(self.peek() as i32)) {
                insert_semi = true;
                self.scan_number()
            } else {
                self.next(); // always make progress
                if ch == eof {
                    if self.insert_semi {
                        self.insert_semi = false; // EOF consumed
                        return (pos, Token::Semicolon, "\n".to_string());
                    }
                    (Token::EOF, String::new())
                } else {
                    match char_from_rune(ch) {
                        '\n' => {
                            // we only reach here if s.insert_semi was
                            // set in the first place and exited early
                            // from s.skip_whitespace()
                            self.insert_semi = false; // newline consumed
                            return (pos, Token::Semicolon, "\n".to_string());
                        }
                        '"' => {
                            insert_semi = true;
                            (Token::String, self.scan_string())
                        }
                        '\'' => {
                            insert_semi = true;
                            (Token::Char, self.scan_rune())
                        }
                        '`' => {
                            insert_semi = true;
                            (Token::String, self.scan_raw_string())
                        }
                        ':' => {
                            // switch2(token.COLON, token.DEFINE)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::Define, String::new())
                            } else {
                                (Token::Colon, String::new())
                            }
                        }
                        '.' => {
                            // fractions starting with a '.' are handled by outer switch
                            if self.ch == '.' as i32 && self.peek() == b'.' {
                                self.next();
                                self.next(); // consume last '.'
                                (Token::Ellipsis, String::new())
                            } else {
                                (Token::Period, String::new())
                            }
                        }
                        ',' => (Token::Comma, String::new()),
                        ';' => (Token::Semicolon, ";".to_string()),
                        '(' => (Token::LParen, String::new()),
                        ')' => {
                            insert_semi = true;
                            (Token::RParen, String::new())
                        }
                        '[' => (Token::LBrack, String::new()),
                        ']' => {
                            insert_semi = true;
                            (Token::RBrack, String::new())
                        }
                        '{' => (Token::LBrace, String::new()),
                        '}' => {
                            insert_semi = true;
                            (Token::RBrace, String::new())
                        }
                        '+' => {
                            // switch3(token.ADD, token.ADD_ASSIGN, '+', token.INC)
                            let tok = if self.ch == '=' as i32 {
                                self.next();
                                Token::AddAssign
                            } else if self.ch == '+' as i32 {
                                self.next();
                                Token::Inc
                            } else {
                                Token::Add
                            };
                            if tok == Token::Inc {
                                insert_semi = true;
                            }
                            (tok, String::new())
                        }
                        '-' => {
                            // switch3(token.SUB, token.SUB_ASSIGN, '-', token.DEC)
                            let tok = if self.ch == '=' as i32 {
                                self.next();
                                Token::SubAssign
                            } else if self.ch == '-' as i32 {
                                self.next();
                                Token::Dec
                            } else {
                                Token::Sub
                            };
                            if tok == Token::Dec {
                                insert_semi = true;
                            }
                            (tok, String::new())
                        }
                        '*' => {
                            // switch2(token.MUL, token.MUL_ASSIGN)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::MulAssign, String::new())
                            } else {
                                (Token::Mul, String::new())
                            }
                        }
                        '/' => {
                            if self.ch == '/' as i32 || self.ch == '*' as i32 {
                                // comment
                                let (comment, nl_offset) = self.scan_comment();
                                if self.insert_semi && nl_offset != 0 {
                                    // For /*...*/ containing \n, return
                                    // COMMENT then artificial SEMICOLON.
                                    self.nl_pos = self.file.pos(nl_offset as i64);
                                    self.insert_semi = false;
                                } else {
                                    insert_semi = self.insert_semi; // preserve insertSemi info
                                }
                                if self.mode & SCAN_COMMENTS == Mode(0) {
                                    // skip comment
                                    continue 'scan_again;
                                }
                                (Token::Comment, comment)
                            } else {
                                // division
                                // switch2(token.QUO, token.QUO_ASSIGN)
                                if self.ch == '=' as i32 {
                                    self.next();
                                    (Token::QuoAssign, String::new())
                                } else {
                                    (Token::Quo, String::new())
                                }
                            }
                        }
                        '%' => {
                            // switch2(token.REM, token.REM_ASSIGN)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::RemAssign, String::new())
                            } else {
                                (Token::Rem, String::new())
                            }
                        }
                        '^' => {
                            // switch2(token.XOR, token.XOR_ASSIGN)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::XOrAssign, String::new())
                            } else {
                                (Token::XOr, String::new())
                            }
                        }
                        '<' => {
                            if self.ch == '-' as i32 {
                                self.next();
                                (Token::Arrow, String::new())
                            } else {
                                // switch4(token.LSS, token.LEQ, '<', token.SHL, token.SHL_ASSIGN)
                                if self.ch == '=' as i32 {
                                    self.next();
                                    (Token::Leq, String::new())
                                } else if self.ch == '<' as i32 {
                                    self.next();
                                    if self.ch == '=' as i32 {
                                        self.next();
                                        (Token::ShlAssign, String::new())
                                    } else {
                                        (Token::Shl, String::new())
                                    }
                                } else {
                                    (Token::Less, String::new())
                                }
                            }
                        }
                        '>' => {
                            // switch4(token.GTR, token.GEQ, '>', token.SHR, token.SHR_ASSIGN)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::Geq, String::new())
                            } else if self.ch == '>' as i32 {
                                self.next();
                                if self.ch == '=' as i32 {
                                    self.next();
                                    (Token::ShrAssign, String::new())
                                } else {
                                    (Token::Shr, String::new())
                                }
                            } else {
                                (Token::Greater, String::new())
                            }
                        }
                        '=' => {
                            // switch2(token.ASSIGN, token.EQL)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::Equal, String::new())
                            } else {
                                (Token::Assign, String::new())
                            }
                        }
                        '!' => {
                            // switch2(token.NOT, token.NEQ)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::Neq, String::new())
                            } else {
                                (Token::Not, String::new())
                            }
                        }
                        '&' => {
                            if self.ch == '^' as i32 {
                                self.next();
                                // switch2(token.AND_NOT, token.AND_NOT_ASSIGN)
                                if self.ch == '=' as i32 {
                                    self.next();
                                    (Token::AndNotAssign, String::new())
                                } else {
                                    (Token::AndNot, String::new())
                                }
                            } else {
                                // switch3(token.AND, token.AND_ASSIGN, '&', token.LAND)
                                if self.ch == '=' as i32 {
                                    self.next();
                                    (Token::AndAssign, String::new())
                                } else if self.ch == '&' as i32 {
                                    self.next();
                                    (Token::LAnd, String::new())
                                } else {
                                    (Token::And, String::new())
                                }
                            }
                        }
                        '|' => {
                            // switch3(token.OR, token.OR_ASSIGN, '|', token.LOR)
                            if self.ch == '=' as i32 {
                                self.next();
                                (Token::OrAssign, String::new())
                            } else if self.ch == '|' as i32 {
                                self.next();
                                (Token::LOr, String::new())
                            } else {
                                (Token::Or, String::new())
                            }
                        }
                        '~' => (Token::Tilde, String::new()),
                        c @ ('“' | '”') => {
                            // Report an informative error for U+201[CD] quotation
                            // marks, which are easily introduced via copy and paste.
                            self.error(
                                self.file.offset(pos) as usize,
                                format!(
                                    "curly quotation mark {} (use neutral {})",
                                    fmt_quote(c),
                                    fmt_quote('"')
                                ),
                            );
                            insert_semi = self.insert_semi; // preserve insertSemi info
                            (Token::Illegal, c.to_string())
                        }
                        c => {
                            // next reports unexpected BOMs - don't repeat
                            if ch != bom {
                                self.error(
                                    self.file.offset(pos) as usize,
                                    format!("illegal character {}", fmt_unicode_char(c)),
                                );
                            }
                            insert_semi = self.insert_semi; // preserve insertSemi info
                            (Token::Illegal, c.to_string())
                        }
                    }
                }
            };

            if self.mode & DONT_INSERT_SEMIS == Mode(0) {
                self.insert_semi = insert_semi;
            }

            return (pos, tok, lit);
        }
    }

    fn skip_whitespace(&mut self) {
        while self.ch == ' ' as i32
            || self.ch == '\t' as i32
            || (self.ch == '\n' as i32 && !self.insert_semi)
            || self.ch == '\r' as i32
        {
            self.next();
        }
    }

    /// scan_comment returns the text of the comment and (if nonzero)
    /// the offset of the first newline within it, which implies a
    /// /*...*/ comment.
    fn scan_comment(&mut self) -> (String, usize) {
        // initial '/' already consumed; self.ch == '/' || self.ch == '*'
        let offs = self.offset - 1; // position of initial '/'
        let mut next: Option<usize> = None; // position immediately following the comment; None means invalid comment
        let mut num_cr = 0;
        let mut nl_offset = 0; // offset of first newline within /*...*/ comment

        if self.ch == '/' as i32 {
            //-style comment
            // (the final '\n' is not considered part of the comment)
            self.next();
            while self.ch != '\n' as i32 && self.ch >= 0 {
                if self.ch == '\r' as i32 {
                    num_cr += 1;
                }
                self.next();
            }
            // if we are at '\n', the position following the comment is afterwards
            next = if self.ch == '\n' as i32 {
                Some(self.offset + 1)
            } else {
                Some(self.offset)
            };
        } else {
            /*-style comment */
            self.next();
            while self.ch >= 0 {
                let ch = self.ch;
                if ch == '\r' as i32 {
                    num_cr += 1;
                } else if ch == '\n' as i32 && nl_offset == 0 {
                    nl_offset = self.offset;
                }
                self.next();
                if ch == '*' as i32 && self.ch == '/' as i32 {
                    self.next();
                    next = Some(self.offset);
                    break;
                }
            }

            if next.is_none() {
                self.error(offs, "comment not terminated");
            }
        }

        let mut lit: &[u8] = &self.src[offs..self.offset];

        // On Windows, a (//-comment) line may end in "\r\n".
        // Remove the final '\r' before analyzing the text for
        // line directives (matching the compiler). Remove any
        // other '\r' afterwards (matching the pre-existing be-
        // havior of the scanner).
        if num_cr > 0 && lit.len() >= 2 && lit[1] == b'/' && lit[lit.len() - 1] == b'\r' {
            lit = &lit[..lit.len() - 1];
            num_cr -= 1;
        }

        // interpret line directives
        // (//line directives must start at the beginning of the current line)
        if let Some(next) = next
            && (lit[1] == b'*' || offs == self.line_offset)
            && lit[2..].starts_with(line_prefix)
        {
            self.update_line_info(next, offs, lit);
        }

        let lit = if num_cr > 0 {
            strip_cr(lit, lit[1] == b'*')
        } else {
            lit.to_vec()
        };

        (String::from_utf8_lossy(&lit).into_owned(), nl_offset)
    }

    /// update_line_info parses the incoming comment text at offset offs
    /// as a line directive. If successful, it updates the line info table
    /// for the position next per the line directive.
    fn update_line_info(&mut self, next: usize, offs: usize, text: &[u8]) {
        // extract comment text
        let mut text = text;
        if text[1] == b'*' {
            text = &text[..text.len() - 2]; // lop off trailing "*/"
        }
        text = &text[7..]; // lop off leading "//line " or "/*line "
        let offs = offs + 7;

        let (i, n, ok) = trailing_digits(text);
        if i == 0 {
            return; // ignore (not a line directive)
        }
        // i > 0

        if !ok {
            // text has a suffix :xxx but xxx is not a number
            self.error(
                offs + i,
                format!(
                    "invalid line number: {}",
                    String::from_utf8_lossy(&text[i..])
                ),
            );
            return;
        }

        // Put a cap on the maximum size of line and column numbers.
        // 30 bits allows for some additional space before wrapping an int32.
        // Keep this consistent with cmd/compile/internal/syntax.PosMax.
        const max_line_col: i64 = 1 << 30;

        // By default, the trailing number (at digit start i, value n) is the
        // line number.
        let mut line_digit_start = i;
        let mut line = n;
        let mut col = 0;
        let (i2, n2, ok2) = trailing_digits(&text[..i - 1]);
        if ok2 {
            //line filename:line:col
            // Then i/n are the *column* number, and i2/n2 (within
            // text[..i-1]) are the *line* number.
            line_digit_start = i2;
            line = n2;
            col = n;
            if col == 0 || col > max_line_col {
                self.error(
                    offs + i,
                    format!(
                        "invalid column number: {}",
                        String::from_utf8_lossy(&text[i..])
                    ),
                );
                return;
            }
        }

        if line == 0 || line > max_line_col {
            self.error(
                offs + line_digit_start,
                format!(
                    "invalid line number: {}",
                    String::from_utf8_lossy(&text[line_digit_start..])
                ),
            );
            return;
        }

        // If we have a column (//line filename:line:col form),
        // an empty filename means to use the previous filename.
        let filename_part = String::from_utf8_lossy(&text[..line_digit_start - 1]);
        let filename: String = if filename_part.is_empty() && ok2 {
            self.file.position(self.file.pos(offs as i64)).file_name
        } else if filename_part.is_empty() {
            String::new()
        } else {
            // Put a relative filename in the current directory.
            // This is for compatibility with earlier releases.
            // See issue 26671.
            let cleaned = filepath_clean(&filename_part);
            if filepath_is_abs(&cleaned) {
                cleaned
            } else {
                filepath_join(&self.dir, &cleaned)
            }
        };

        self.file
            .add_line_column_info(next as i64, &filename, line, col);
    }

    /// scan_identifier reads the string of valid identifier characters at
    /// self.offset. It must only be called when self.ch is known to be a
    /// valid letter.
    fn scan_identifier(&mut self) -> String {
        let offs = self.offset;

        // Optimize for the common case of an ASCII identifier.
        //
        // Ranging over s.src[s.rd_offset:] lets us avoid some bounds checks, and
        // avoids conversions to runes.
        //
        // In case we encounter a non-ASCII character, fall back on the slower path
        // of calling into s.next().
        let mut rd = self.rd_offset;
        while rd < self.src.len() {
            let b = self.src[rd];
            if b.is_ascii_alphanumeric() || b == b'_' {
                rd += 1;
                continue;
            }
            break;
        }

        if rd == self.src.len() {
            self.offset = rd;
            self.rd_offset = rd;
            self.ch = eof;
            return String::from_utf8_lossy(&self.src[offs..rd]).into_owned();
        }
        let b = self.src[rd];
        self.rd_offset = rd;
        if 0 < b && b < 0x80 {
            // Optimization: we've encountered an ASCII character that's not a letter
            // or number. Avoid the call into s.next() and corresponding set up.
            //
            // Note that s.next() does some line accounting if s.ch is '\n', so this
            // shortcut is only possible because we know that the preceding character
            // is not '\n'.
            self.ch = b as i32;
            self.offset = self.rd_offset;
            self.rd_offset += 1;
        } else {
            // We know that the preceding character is valid for an identifier because
            // scan_identifier is only called when s.ch is a letter, so calling
            // s.next() at s.rd_offset resets the scanner state.
            self.next();
            while is_letter(self.ch) || is_digit(self.ch) {
                self.next();
            }
        }

        String::from_utf8_lossy(&self.src[offs..self.offset]).into_owned()
    }

    /// digits accepts the sequence { digit | '_' }.
    /// If base <= 10, digits accepts any decimal digit but records
    /// the offset (relative to the source start) of a digit >= base
    /// in invalid, if invalid is None.
    /// digits returns a bitset describing whether the sequence contained
    /// digits (bit 0 is set), or separators '_' (bit 1 is set).
    fn digits(&mut self, base: i32, invalid: &mut Option<usize>) -> i32 {
        let mut digsep = 0;
        if base <= 10 {
            let max = '0' as i32 + base;
            while is_decimal(self.ch) || self.ch == '_' as i32 {
                let ds = if self.ch == '_' as i32 {
                    2
                } else {
                    if self.ch >= max && invalid.is_none() {
                        *invalid = Some(self.offset); // record invalid rune offset
                    }
                    1
                };
                digsep |= ds;
                self.next();
            }
        } else {
            while is_hex(self.ch) || self.ch == '_' as i32 {
                let ds = if self.ch == '_' as i32 { 2 } else { 1 };
                digsep |= ds;
                self.next();
            }
        }
        digsep
    }

    /// scan_number scans an INT, FLOAT, or IMAG literal at self.offset.
    fn scan_number(&mut self) -> (Token, String) {
        let offs = self.offset;
        let mut tok = Token::Illegal;

        let mut base = 10; // number base
        let mut prefix: i32 = 0; // one of 0 (decimal), '0' (0-octal), 'x', 'o', or 'b'
        let mut digsep = 0; // bit 0: digit present, bit 1: '_' present
        let mut invalid: Option<usize> = None; // index of invalid digit in literal, or None

        // integer part
        if self.ch != '.' as i32 {
            tok = Token::Int;
            if self.ch == '0' as i32 {
                self.next();
                match lower(self.ch) {
                    p if p == 'x' as i32 => {
                        self.next();
                        base = 16;
                        prefix = 'x' as i32;
                    }
                    p if p == 'o' as i32 => {
                        self.next();
                        base = 8;
                        prefix = 'o' as i32;
                    }
                    p if p == 'b' as i32 => {
                        self.next();
                        base = 2;
                        prefix = 'b' as i32;
                    }
                    _ => {
                        base = 8;
                        prefix = '0' as i32;
                        digsep = 1; // leading 0
                    }
                }
            }
            digsep |= self.digits(base, &mut invalid);
        }

        // fractional part
        if self.ch == '.' as i32 {
            tok = Token::Float;
            if prefix == 'o' as i32 || prefix == 'b' as i32 {
                self.error(
                    self.offset,
                    format!("invalid radix point in {}", litname(prefix)),
                );
            }
            self.next();
            digsep |= self.digits(base, &mut invalid);
        }

        if digsep & 1 == 0 {
            self.error(self.offset, format!("{} has no digits", litname(prefix)));
        }

        // exponent
        let e = lower(self.ch);
        if e == 'e' as i32 || e == 'p' as i32 {
            if e == 'e' as i32 && prefix != 0 && prefix != '0' as i32 {
                self.error(
                    self.offset,
                    format!(
                        "{} exponent requires decimal mantissa",
                        fmt_quote(char_from_rune(self.ch))
                    ),
                );
            } else if e == 'p' as i32 && prefix != 'x' as i32 {
                self.error(
                    self.offset,
                    format!(
                        "{} exponent requires hexadecimal mantissa",
                        fmt_quote(char_from_rune(self.ch))
                    ),
                );
            }
            self.next();
            tok = Token::Float;
            if self.ch == '+' as i32 || self.ch == '-' as i32 {
                self.next();
            }
            let mut invalid_exp = None;
            let ds = self.digits(10, &mut invalid_exp);
            digsep |= ds;
            if ds & 1 == 0 {
                self.error(self.offset, "exponent has no digits");
            }
        } else if prefix == 'x' as i32 && tok == Token::Float {
            self.error(self.offset, "hexadecimal mantissa requires a 'p' exponent");
        }

        // suffix 'i'
        if self.ch == 'i' as i32 {
            tok = Token::Imag;
            self.next();
        }

        let lit = String::from_utf8_lossy(&self.src[offs..self.offset]).into_owned();
        if tok == Token::Int
            && let Some(invalid) = invalid
        {
            self.error(
                invalid,
                format!(
                    "invalid digit {} in {}",
                    fmt_quote(lit.as_bytes()[invalid - offs] as char),
                    litname(prefix)
                ),
            );
        }
        if digsep & 2 != 0
            && let Some(i) = invalid_sep(&lit)
        {
            self.error(offs + i, "'_' must separate successive digits");
        }

        (tok, lit)
    }

    /// scan_escape parses an escape sequence where quote is the accepted
    /// escaped quote. In case of a syntax error, it stops at the offending
    /// character (without consuming it) and returns false. Otherwise
    /// it returns true.
    fn scan_escape(&mut self, quote: i32) -> bool {
        let offs = self.offset;

        if self.ch < 0 {
            self.error(offs, "escape sequence not terminated");
            return false;
        }
        let (mut n, base, max): (i32, u32, u32) = match char_from_rune(self.ch) {
            c if c == char_from_rune(quote) => {
                self.next();
                return true;
            }
            'a' | 'b' | 'f' | 'n' | 'r' | 't' | 'v' | '\\' => {
                self.next();
                return true;
            }
            '0'..='7' => (3, 8, 255),
            'x' => {
                self.next();
                (2, 16, 255)
            }
            'u' => {
                self.next();
                (4, 16, 0x10FFFF)
            }
            'U' => {
                self.next();
                (8, 16, 0x10FFFF)
            }
            _ => {
                self.error(offs, "unknown escape sequence");
                return false;
            }
        };

        let mut x: u32 = 0;
        while n > 0 {
            let d = digit_val(self.ch);
            if d as u32 >= base {
                let msg = if self.ch < 0 {
                    "escape sequence not terminated".to_string()
                } else {
                    format!(
                        "illegal character {} in escape sequence",
                        fmt_unicode_char(char_from_rune(self.ch))
                    )
                };
                self.error(self.offset, msg);
                return false;
            }
            x = x * base + d as u32;
            self.next();
            n -= 1;
        }

        if x > max || (0xD800..0xE000).contains(&x) {
            self.error(offs, "escape sequence is invalid Unicode code point");
            return false;
        }

        true
    }

    fn scan_rune(&mut self) -> String {
        // '\'' opening already consumed
        let offs = self.offset - 1;

        let mut valid = true;
        let mut n = 0;
        loop {
            let ch = self.ch;
            if ch == '\n' as i32 || ch < 0 {
                // only report error if we don't have one already
                if valid {
                    self.error(offs, "rune literal not terminated");
                    valid = false;
                }
                break;
            }
            self.next();
            if ch == '\'' as i32 {
                break;
            }
            n += 1;
            // continue to read to closing quote
            if ch == '\\' as i32 && !self.scan_escape('\'' as i32) {
                valid = false;
            }
        }

        if valid && n != 1 {
            self.error(offs, "illegal rune literal");
        }

        String::from_utf8_lossy(&self.src[offs..self.offset]).into_owned()
    }

    fn scan_string(&mut self) -> String {
        // '"' opening already consumed
        let offs = self.offset - 1;

        loop {
            let ch = self.ch;
            if ch == '\n' as i32 || ch < 0 {
                self.error(offs, "string literal not terminated");
                break;
            }
            self.next();
            if ch == '"' as i32 {
                break;
            }
            if ch == '\\' as i32 {
                self.scan_escape('"' as i32);
            }
        }

        String::from_utf8_lossy(&self.src[offs..self.offset]).into_owned()
    }

    fn scan_raw_string(&mut self) -> String {
        // '`' opening already consumed
        let offs = self.offset - 1;

        let mut has_cr = false;
        loop {
            let ch = self.ch;
            if ch < 0 {
                self.error(offs, "raw string literal not terminated");
                break;
            }
            self.next();
            if ch == '`' as i32 {
                break;
            }
            if ch == '\r' as i32 {
                has_cr = true;
            }
        }

        let lit = &self.src[offs..self.offset];
        let lit = if has_cr {
            strip_cr(lit, false)
        } else {
            lit.to_vec()
        };

        String::from_utf8_lossy(&lit).into_owned()
    }
}

const line_prefix: &[u8] = b"line ";

/// digit_val returns the value of a digit for the given rune,
/// or 16 (larger than any legal digit val) if it is not a digit.
fn digit_val(ch: i32) -> i32 {
    if ('0' as i32..='9' as i32).contains(&ch) {
        return ch - '0' as i32;
    }
    let l = lower(ch);
    if ('a' as i32..='f' as i32).contains(&l) {
        return l - 'a' as i32 + 10;
    }
    16 // larger than any legal digit val
}

/// lower returns lower-case ch iff ch is an ASCII letter.
fn lower(ch: i32) -> i32 {
    ('a' as i32 - 'A' as i32) | ch
}

fn is_decimal(ch: i32) -> bool {
    ('0' as i32..='9' as i32).contains(&ch)
}

fn is_hex(ch: i32) -> bool {
    is_decimal(ch) || ('a' as i32..='f' as i32).contains(&lower(ch))
}

fn is_letter(ch: i32) -> bool {
    ('a' as i32..='z' as i32).contains(&lower(ch))
        || ch == '_' as i32
        || (ch >= 0x80 && unicode_is_letter(char_from_rune(ch)))
}

fn is_digit(ch: i32) -> bool {
    is_decimal(ch) || (ch >= 0x80 && unicode_is_digit(char_from_rune(ch)))
}

/// Mirrors Go's `unicode.IsLetter`: general category L* (Lu, Ll, Lt, Lm, Lo).
fn unicode_is_letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

/// Mirrors Go's `unicode.IsDigit`: general category Nd.
fn unicode_is_digit(c: char) -> bool {
    matches!(get_general_category(c), GeneralCategory::DecimalNumber)
}

/// Mirrors Go's `unicode.IsPrint`: the L, M, N, P, S categories plus the
/// ASCII space character (but no other space separators).
fn unicode_is_print(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

/// Formats a rune like Go's `%#U` verb: "U+XXXX", followed by a quoted
/// glyph if the rune is printable, e.g. `U+0023 '#'`.
fn fmt_unicode_char(c: char) -> String {
    let mut s = format!("U+{:04X}", c as u32);
    if unicode_is_print(c) {
        s.push_str(&format!(" '{}'", c));
    }
    s
}

/// Formats a rune like Go's `%q` verb: a single-quoted rune literal.
fn fmt_quote(c: char) -> String {
    match c {
        '\'' => "'\\''".to_string(),
        '\\' => "'\\\\'".to_string(),
        '\n' => "'\\n'".to_string(),
        '\t' => "'\\t'".to_string(),
        '\r' => "'\\r'".to_string(),
        _ if unicode_is_print(c) => format!("'{}'", c),
        _ => format!("'\\u{:04x}'", c as u32),
    }
}

/// Converts an internal rune (i32) to a char. Never called with `eof`
/// (all call sites guard for `ch < 0` first, or the value came from a
/// decoded rune).
fn char_from_rune(ch: i32) -> char {
    char::from_u32(ch as u32).expect("rune value is a valid char")
}

/// Decodes a rune from the start of `src`, mirroring Go's `utf8.DecodeRune`
/// for the scanner's purposes: a valid UTF-8 encoding yields its char and
/// byte width; any invalid encoding (bad continuation, truncated sequence,
/// overlong form, surrogate, or out-of-range value) yields `('\u{FFFD}', 1)`.
fn decode_rune(src: &[u8]) -> (i32, usize) {
    debug_assert!(!src.is_empty());
    if src[0] < 0x80 {
        return (src[0] as i32, 1);
    }
    let max = src.len().min(4);
    for w in (1..=max).rev() {
        if let Ok(s) = std::str::from_utf8(&src[..w]) {
            let c = s.chars().next().expect("non-empty window");
            return (c as i32, c.len_utf8());
        }
    }
    ('\u{FFFD}' as i32, 1)
}

/// litname returns the literal kind name for a number prefix.
fn litname(prefix: i32) -> &'static str {
    match prefix {
        p if p == 'x' as i32 => "hexadecimal literal",
        p if p == 'o' as i32 || p == '0' as i32 => "octal literal",
        p if p == 'b' as i32 => "binary literal",
        _ => "decimal literal",
    }
}

/// invalid_sep returns the index of the first invalid separator in x, or None.
fn invalid_sep(x: &str) -> Option<usize> {
    let x = x.as_bytes();
    let mut x1 = ' ' as i32; // prefix char, we only care if it's 'x'
    let mut d = '.' as i32; // digit, one of '_', '0' (a digit), or '.' (anything else)
    let mut i = 0;

    // a prefix counts as a digit
    if x.len() >= 2 && x[0] == b'0' {
        x1 = lower(x[1] as i32);
        if x1 == 'x' as i32 || x1 == 'o' as i32 || x1 == 'b' as i32 {
            d = '0' as i32;
            i = 2;
        }
    }

    // mantissa and exponent
    while i < x.len() {
        let p = d; // previous digit
        d = x[i] as i32;
        if d == '_' as i32 {
            if p != '0' as i32 {
                return Some(i);
            }
        } else if is_decimal(d) || (x1 == 'x' as i32 && is_hex(d)) {
            d = '0' as i32;
        } else {
            if p == '_' as i32 {
                return Some(i - 1);
            }
            d = '.' as i32;
        }
        i += 1;
    }
    if d == '_' as i32 {
        return Some(x.len() - 1);
    }

    None
}

/// strip_cr removes carriage return characters from `b`, except, in a
/// `/*`-style comment, a `\r` from `*\r/` (incl. sequences of `\r` from
/// `*\r\r...\r/`) since the resulting `*/` would terminate the comment too
/// early unless the `\r` is immediately following the opening `/*` in which
/// case it's ok because `/*/` is not closed yet (issue #11151).
fn strip_cr(b: &[u8], comment: bool) -> Vec<u8> {
    let mut c = Vec::with_capacity(b.len());
    for (j, &ch) in b.iter().enumerate() {
        if ch != b'\r'
            || (comment
                && c.len() > 2
                && c[c.len() - 1] == b'*'
                && j + 1 < b.len()
                && b[j + 1] == b'/')
        {
            c.push(ch);
        }
    }
    c
}

/// trailing_digits parses the trailing ":digits" of text (looking from the
/// right, since Windows filenames may contain ':').
/// Returns the index after the last ':', the parsed value, and whether the
/// suffix was a valid (non-empty) decimal number.
fn trailing_digits(text: &[u8]) -> (usize, i64, bool) {
    let Some(i) = text.iter().rposition(|&c| c == b':') else {
        return (0, 0, false); // no ":"
    };
    match parse_digits(&text[i + 1..]) {
        Some(n) => (i + 1, n, true),
        None => (i + 1, 0, false),
    }
}

/// Parses a non-empty ASCII decimal number, mirroring the acceptance of
/// `strconv.ParseUint(s, 10, 0)` (no sign, no underscores, no whitespace).
fn parse_digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// filepath_split_dir returns the directory portion of a '/'-separated path,
/// mirroring Go's `path/filepath.Split` on Unix (the dir keeps its trailing
/// '/' and is empty when the path has no '/'; an empty path yields "").
fn filepath_split_dir(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => p[..i + 1].to_string(),
        None => String::new(),
    }
}

/// filepath_clean mirrors Go's `path/filepath.Clean` (Unix flavor), a purely
/// lexical cleanup of a '/'-separated path. Backslashes are ordinary
/// characters (no Windows semantics).
fn filepath_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let rooted = path.starts_with('/');
    let n = path.len();
    let b = path.as_bytes();
    let mut out = Vec::with_capacity(n);
    let mut r = 0;
    let mut dotdot = 0;
    if rooted {
        out.push(b'/');
        r = 1;
        dotdot = 1;
    }
    while r < n {
        match b[r] {
            b'/' => {
                // empty path element
                r += 1;
            }
            _ if b[r] == b'.' && (r + 1 == n || b[r + 1] == b'/') => {
                // . element
                r += 1;
            }
            _ if b[r] == b'.' && b[r + 1] == b'.' && (r + 2 == n || b[r + 2] == b'/') => {
                // .. element: remove to last /
                r += 2;
                if out.len() > dotdot {
                    // can backtrack
                    out.pop();
                    while out.len() > 1 && b[out.len() - 1] != b'/' {
                        out.pop();
                    }
                } else if !rooted {
                    // cannot backtrack, but not rooted, so append .. element.
                    if !out.is_empty() {
                        out.push(b'/');
                    }
                    out.push(b'.');
                    out.push(b'.');
                    dotdot = out.len();
                }
            }
            _ => {
                // real path element - add slash separator
                if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                    out.push(b'/');
                }
                // copy element
                while r < n && b[r] != b'/' {
                    out.push(b[r]);
                    r += 1;
                }
            }
        }
    }
    if out.is_empty() {
        ".".to_string()
    } else {
        String::from_utf8(out).expect("clean keeps UTF-8")
    }
}

/// filepath_join mirrors Go's `path/filepath.Join` for two elements.
fn filepath_join(dir: &str, file: &str) -> String {
    let mut buf = String::with_capacity(dir.len() + file.len() + 1);
    buf.push_str(dir);
    if !buf.is_empty() && !file.is_empty() {
        buf.push('/');
    }
    buf.push_str(file);
    filepath_clean(&buf)
}

/// filepath_is_abs reports whether the path is absolute (Unix semantics).
fn filepath_is_abs(p: &str) -> bool {
    p.starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::scanner::error::ErrorList;
    use crate::token::FileSet;

    // ----- helpers shared by the ported tests (scanner_test.go) -----

    const /* class */ SPECIAL: i32 = 0;
    const LITERAL: i32 = 1;
    const OPERATOR: i32 = 2;
    const KEYWORD: i32 = 3;

    /// token_class mirrors Go's tokenclass: special / literal / operator / keyword.
    fn token_class(tok: Token) -> i32 {
        if tok.is_literal() {
            LITERAL
        } else if tok.is_operator() {
            OPERATOR
        } else if tok.is_keyword() {
            KEYWORD
        } else {
            SPECIAL
        }
    }

    /// go_string renders a token the way Go's token.Token.String() does,
    /// which is the vocabulary used by the ported expected strings.
    fn go_string(tok: Token) -> String {
        match tok {
            Token::Illegal => "ILLEGAL",
            Token::EOF => "EOF",
            Token::Comment => "COMMENT",
            Token::Ident => "IDENT",
            Token::Int => "INT",
            Token::Float => "FLOAT",
            Token::Imag => "IMAG",
            Token::Char => "CHAR",
            Token::String => "STRING",
            Token::LBrace => "{",
            Token::RBrace => "}",
            other => return format!("{other}"),
        }
        .to_string()
    }

    fn newline_count(s: &[u8]) -> i64 {
        s.iter().filter(|&&b| b == b'\n').count() as i64
    }

    /// check_pos mirrors Go's checkPos helper.
    fn check_pos(lit: &str, got: &Position, want: &Position) {
        assert_eq!(got, want, "bad position for {lit:?}");
    }

    /// fatal_error_handler mirrors Go's `func(...) { t.Fatal(msg) }` handlers.
    fn fatal_error_handler() -> ErrorHandler {
        Box::new(|_, msg| panic!("unexpected scanner error: {msg}"))
    }

    fn err_bytes(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    // ----- TestScan (tokens + whitespace tables) -----

    #[derive(Clone, Copy)]
    struct Elt {
        tok: Token,
        lit: &'static str,
        class: i32,
    }

    const TOKENS: &[Elt] = &[
        // Special tokens
        Elt {
            tok: Token::Comment,
            lit: "/* a comment */",
            class: SPECIAL,
        },
        Elt {
            tok: Token::Comment,
            lit: "// a comment \n",
            class: SPECIAL,
        },
        Elt {
            tok: Token::Comment,
            lit: "/*\r*/",
            class: SPECIAL,
        },
        Elt {
            tok: Token::Comment,
            lit: "/**\r/*/",
            class: SPECIAL,
        }, // issue 11151
        Elt {
            tok: Token::Comment,
            lit: "/**\r\r/*/",
            class: SPECIAL,
        },
        Elt {
            tok: Token::Comment,
            lit: "//\r\n",
            class: SPECIAL,
        },
        // Identifiers and basic type literals
        Elt {
            tok: Token::Ident,
            lit: "foobar",
            class: LITERAL,
        },
        Elt {
            tok: Token::Ident,
            lit: "a\u{06F0}\u{06F1}\u{06F8}",
            class: LITERAL,
        },
        Elt {
            tok: Token::Ident,
            lit: "foo\u{096C}\u{096D}",
            class: LITERAL,
        },
        Elt {
            tok: Token::Ident,
            lit: "bar\u{FF19}\u{FF18}\u{FF17}\u{FF16}",
            class: LITERAL,
        },
        Elt {
            tok: Token::Ident,
            lit: "\u{015D}",
            class: LITERAL,
        }, // was bug (issue 4000)
        Elt {
            tok: Token::Ident,
            lit: "\u{015D}foo",
            class: LITERAL,
        }, // was bug (issue 4000)
        Elt {
            tok: Token::Int,
            lit: "0",
            class: LITERAL,
        },
        Elt {
            tok: Token::Int,
            lit: "1",
            class: LITERAL,
        },
        Elt {
            tok: Token::Int,
            lit: "123456789012345678890",
            class: LITERAL,
        },
        Elt {
            tok: Token::Int,
            lit: "01234567",
            class: LITERAL,
        },
        Elt {
            tok: Token::Int,
            lit: "0xcafebabe",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "0.",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: ".0",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "3.14159265",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "1e0",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "1e+100",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "1e-100",
            class: LITERAL,
        },
        Elt {
            tok: Token::Float,
            lit: "2.71828e-1000",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "0i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "1i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "012345678901234567889i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "123456789012345678890i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "0.i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: ".0i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "3.14159265i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "1e0i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "1e+100i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "1e-100i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Imag,
            lit: "2.71828e-1000i",
            class: LITERAL,
        },
        Elt {
            tok: Token::Char,
            lit: "'a'",
            class: LITERAL,
        },
        Elt {
            tok: Token::Char,
            lit: "'\\000'",
            class: LITERAL,
        },
        Elt {
            tok: Token::Char,
            lit: "'\\xFF'",
            class: LITERAL,
        },
        Elt {
            tok: Token::Char,
            lit: "'\\uff16'",
            class: LITERAL,
        },
        Elt {
            tok: Token::Char,
            lit: "'\\U0000ff16'",
            class: LITERAL,
        },
        Elt {
            tok: Token::String,
            lit: "`foobar`",
            class: LITERAL,
        },
        Elt {
            tok: Token::String,
            lit: "`foo\nbar`",
            class: LITERAL,
        },
        Elt {
            tok: Token::String,
            lit: "`\r`",
            class: LITERAL,
        },
        Elt {
            tok: Token::String,
            lit: "`foo\r\nbar`",
            class: LITERAL,
        },
        // Operators and delimiters
        Elt {
            tok: Token::Add,
            lit: "+",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Sub,
            lit: "-",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Mul,
            lit: "*",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Quo,
            lit: "/",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Rem,
            lit: "%",
            class: OPERATOR,
        },
        Elt {
            tok: Token::And,
            lit: "&",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Or,
            lit: "|",
            class: OPERATOR,
        },
        Elt {
            tok: Token::XOr,
            lit: "^",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Shl,
            lit: "<<",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Shr,
            lit: ">>",
            class: OPERATOR,
        },
        Elt {
            tok: Token::AndNot,
            lit: "&^",
            class: OPERATOR,
        },
        Elt {
            tok: Token::AddAssign,
            lit: "+=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::SubAssign,
            lit: "-=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::MulAssign,
            lit: "*=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::QuoAssign,
            lit: "/=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::RemAssign,
            lit: "%=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::AndAssign,
            lit: "&=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::OrAssign,
            lit: "|=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::XOrAssign,
            lit: "^=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::ShlAssign,
            lit: "<<=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::ShrAssign,
            lit: ">>=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::AndNotAssign,
            lit: "&^=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::LAnd,
            lit: "&&",
            class: OPERATOR,
        },
        Elt {
            tok: Token::LOr,
            lit: "||",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Arrow,
            lit: "<-",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Inc,
            lit: "++",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Dec,
            lit: "--",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Equal,
            lit: "==",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Less,
            lit: "<",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Greater,
            lit: ">",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Assign,
            lit: "=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Not,
            lit: "!",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Neq,
            lit: "!=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Leq,
            lit: "<=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Geq,
            lit: ">=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Define,
            lit: ":=",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Ellipsis,
            lit: "...",
            class: OPERATOR,
        },
        Elt {
            tok: Token::LParen,
            lit: "(",
            class: OPERATOR,
        },
        Elt {
            tok: Token::LBrack,
            lit: "[",
            class: OPERATOR,
        },
        Elt {
            tok: Token::LBrace,
            lit: "{",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Comma,
            lit: ",",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Period,
            lit: ".",
            class: OPERATOR,
        },
        Elt {
            tok: Token::RParen,
            lit: ")",
            class: OPERATOR,
        },
        Elt {
            tok: Token::RBrack,
            lit: "]",
            class: OPERATOR,
        },
        Elt {
            tok: Token::RBrace,
            lit: "}",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Semicolon,
            lit: ";",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Colon,
            lit: ":",
            class: OPERATOR,
        },
        Elt {
            tok: Token::Tilde,
            lit: "~",
            class: OPERATOR,
        },
        // Keywords
        Elt {
            tok: Token::Break,
            lit: "break",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Case,
            lit: "case",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Chan,
            lit: "chan",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Const,
            lit: "const",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Continue,
            lit: "continue",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Default,
            lit: "default",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Defer,
            lit: "defer",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Else,
            lit: "else",
            class: KEYWORD,
        },
        Elt {
            tok: Token::FallThrough,
            lit: "fallthrough",
            class: KEYWORD,
        },
        Elt {
            tok: Token::For,
            lit: "for",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Func,
            lit: "func",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Go,
            lit: "go",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Goto,
            lit: "goto",
            class: KEYWORD,
        },
        Elt {
            tok: Token::If,
            lit: "if",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Import,
            lit: "import",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Interface,
            lit: "interface",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Map,
            lit: "map",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Package,
            lit: "package",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Range,
            lit: "range",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Return,
            lit: "return",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Select,
            lit: "select",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Struct,
            lit: "struct",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Switch,
            lit: "switch",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Type,
            lit: "type",
            class: KEYWORD,
        },
        Elt {
            tok: Token::Var,
            lit: "var",
            class: KEYWORD,
        },
    ];

    const WHITESPACE: &str = "  \t  \n\n\n"; // to separate tokens

    // Verify that calling Scan() provides the correct results.
    #[test]
    fn test_scan() {
        let whitespace_linecount = newline_count(WHITESPACE.as_bytes());

        let mut source = String::new();
        for t in TOKENS {
            source.push_str(t.lit);
            source.push_str(WHITESPACE);
        }
        let source_bytes = source.into_bytes();
        let src_len = source_bytes.len() as i64;

        let mut fset = FileSet::new();
        let file = fset.add_file("", -1, src_len);

        let handler_errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let handler = {
            let handler_errors = handler_errors.clone();
            Box::new(move |_: Position, msg: String| {
                handler_errors.borrow_mut().push(msg);
            }) as ErrorHandler
        };
        let mut s = Scanner::new(
            file.clone(),
            &source_bytes,
            Some(handler),
            SCAN_COMMENTS | DONT_INSERT_SEMIS,
        );

        // set up expected position
        let mut epos = Position {
            file_name: String::new(),
            offset: 0,
            line: 1,
            column: 1,
        };

        let mut index = 0;
        loop {
            let (pos, tok, lit) = s.scan();

            // check position
            if tok == Token::EOF {
                // correction for EOF
                epos.line = newline_count(&source_bytes);
                epos.column = 2;
            }
            check_pos(&lit, &file.position(pos), &epos);

            // check token
            let mut e = Elt {
                tok: Token::EOF,
                lit: "",
                class: SPECIAL,
            };
            if index < TOKENS.len() {
                e = TOKENS[index];
                index += 1;
            }
            assert_eq!(
                tok, e.tok,
                "bad token for {lit:?}: got {tok:?}, expected {:?}",
                e.tok
            );

            // check token class
            assert_eq!(token_class(tok), e.class, "bad class for {lit:?}");

            // check literal
            let elit: String;
            match e.tok {
                Token::Comment => {
                    // no CRs in comments
                    let mut bytes = strip_cr(e.lit.as_bytes(), e.lit.as_bytes()[1] == b'*');
                    //-style comment literal doesn't contain newline
                    if bytes[1] == b'/' {
                        bytes.pop();
                    }
                    elit = err_bytes(&bytes);
                }
                Token::Semicolon => elit = ";".to_string(),
                _ => {
                    if e.tok.is_literal() {
                        // no CRs in raw string literals
                        elit = if e.lit.starts_with('`') {
                            err_bytes(&strip_cr(e.lit.as_bytes(), false))
                        } else {
                            e.lit.to_string()
                        };
                    } else if e.tok.is_keyword() || e.tok == Token::Ident {
                        elit = e.lit.to_string();
                    } else {
                        elit = String::new();
                    }
                }
            }
            assert_eq!(
                lit, elit,
                "bad literal for {lit:?}: got {lit:?}, expected {elit:?}"
            );

            if tok == Token::EOF {
                break;
            }

            // update position
            epos.offset += e.lit.len() as i64 + WHITESPACE.len() as i64;
            epos.line += newline_count(e.lit.as_bytes()) + whitespace_linecount;
        }

        assert_eq!(s.error_count, 0, "found {} errors", s.error_count);
        let handler_errors = handler_errors.borrow();
        assert!(
            handler_errors.is_empty(),
            "error handler called: {handler_errors:?}"
        );
    }

    // ----- TestStripCR -----

    #[test]
    fn test_strip_cr() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"//\n", b"//\n"),
            (b"//\r\n", b"//\n"),
            (b"//\r\r\r\n", b"//\n"),
            (b"//\r*\r/\r\n", b"//*/\n"),
            (b"/**/", b"/**/"),
            (b"/*\r/*/", b"/*/*/"),
            (b"/*\r*/", b"/**/"),
            (b"/**\r/*/", b"/**\r/*/"),
            (b"/*\r/\r*\r/*/", b"/*/*\r/*/"),
            (b"/*\r\r\r\r*/", b"/**/"),
        ];
        for (have, want) in cases {
            let got = strip_cr(have, have.len() >= 2 && have[1] == b'*');
            assert_eq!(&got[..], *want, "strip_cr({have:?})");
        }
    }

    // ----- TestSemicolons -----

    fn check_semi(input: &[u8], want_in: &str, mode: Mode) {
        let mut want = want_in.to_string();
        if mode & SCAN_COMMENTS == Mode::default() {
            want = want.replace("COMMENT ", "");
            want = want.replace(" COMMENT", ""); // if at end
            want = want.replace("COMMENT", ""); // if sole token
        }

        let mut fset = FileSet::new();
        let file = fset.add_file("TestSemis", -1, input.len() as i64);
        let mut s = Scanner::new(file.clone(), input, None, mode);
        let mut tokens: Vec<String> = Vec::new();
        loop {
            let (pos, tok, lit) = s.scan();
            if tok == Token::EOF {
                break;
            }
            if tok == Token::Semicolon && lit != ";" {
                // Artificial semicolon:
                // assert that position is EOF or that of a newline.
                let off = file.offset(pos);
                if off != input.len() as i64 && input[off as usize] != b'\n' {
                    panic!(
                        "scanning <<{}>>, got SEMICOLON at offset {off}, want newline or EOF",
                        err_bytes(input)
                    );
                }
            }
            tokens.push(go_string(tok)); // "\n" => ";"
        }
        let got = tokens.join(" ");
        assert_eq!(
            got,
            want,
            "scanning <<{}>>, got [{got}], want [{want}]",
            err_bytes(input)
        );
    }

    #[test]
    fn test_semicolons() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("\u{feff};", ";"), // first BOM is ignored
            (";", ";"),
            ("foo\n", "IDENT ;"),
            ("123\n", "INT ;"),
            ("1.2\n", "FLOAT ;"),
            ("'x'\n", "CHAR ;"),
            ("\"x\"\n", "STRING ;"),
            ("`x`\n", "STRING ;"),
            ("+\n", "+"),
            ("-\n", "-"),
            ("*\n", "*"),
            ("/\n", "/"),
            ("%\n", "%"),
            ("&\n", "&"),
            ("|\n", "|"),
            ("^\n", "^"),
            ("<<\n", "<<"),
            (">>\n", ">>"),
            ("&^\n", "&^"),
            ("+=\n", "+="),
            ("-=\n", "-="),
            ("*=\n", "*="),
            ("/=\n", "/="),
            ("%=\n", "%="),
            ("&=\n", "&="),
            ("|=\n", "|="),
            ("^=\n", "^="),
            ("<<=\n", "<<="),
            (">>=\n", ">>="),
            ("&^=\n", "&^="),
            ("&&\n", "&&"),
            ("||\n", "||"),
            ("<-\n", "<-"),
            ("++\n", "++ ;"),
            ("--\n", "-- ;"),
            ("==\n", "=="),
            ("<\n", "<"),
            (">\n", ">"),
            ("=\n", "="),
            ("!\n", "!"),
            ("!=\n", "!="),
            ("<=\n", "<="),
            (">=\n", ">="),
            (":=\n", ":="),
            ("...\n", "..."),
            ("(\n", "("),
            ("[\n", "["),
            ("{\n", "{"),
            (",\n", ","),
            (".\n", "."),
            (")\n", ") ;"),
            ("]\n", "] ;"),
            ("}\n", "} ;"),
            (";\n", ";"),
            (":\n", ":"),
            ("break\n", "break ;"),
            ("case\n", "case"),
            ("chan\n", "chan"),
            ("const\n", "const"),
            ("continue\n", "continue ;"),
            ("default\n", "default"),
            ("defer\n", "defer"),
            ("else\n", "else"),
            ("fallthrough\n", "fallthrough ;"),
            ("for\n", "for"),
            ("func\n", "func"),
            ("go\n", "go"),
            ("goto\n", "goto"),
            ("if\n", "if"),
            ("import\n", "import"),
            ("interface\n", "interface"),
            ("map\n", "map"),
            ("package\n", "package"),
            ("range\n", "range"),
            ("return\n", "return ;"),
            ("select\n", "select"),
            ("struct\n", "struct"),
            ("switch\n", "switch"),
            ("type\n", "type"),
            ("var\n", "var"),
            ("foo//comment\n", "IDENT COMMENT ;"),
            ("foo//comment", "IDENT COMMENT ;"),
            ("foo/*comment*/\n", "IDENT COMMENT ;"),
            ("foo/*\n*/", "IDENT COMMENT ;"),
            ("foo/*comment*/    \n", "IDENT COMMENT ;"),
            ("foo/*\n*/    ", "IDENT COMMENT ;"),
            ("foo    // comment\n", "IDENT COMMENT ;"),
            ("foo    // comment", "IDENT COMMENT ;"),
            ("foo    /*comment*/\n", "IDENT COMMENT ;"),
            ("foo    /*\n*/", "IDENT COMMENT ;"),
            (
                "foo    /*  */ /* \n */ bar/**/\n",
                "IDENT COMMENT COMMENT ; IDENT COMMENT ;",
            ),
            (
                "foo    /*0*/ /*1*/ /*2*/\n",
                "IDENT COMMENT COMMENT COMMENT ;",
            ),
            ("foo    /*comment*/    \n", "IDENT COMMENT ;"),
            (
                "foo    /*0*/ /*1*/ /*2*/    \n",
                "IDENT COMMENT COMMENT COMMENT ;",
            ),
            (
                "foo\t/**/ /*-------------*/       /*----\n*/bar       /*  \n*/baa\n",
                "IDENT COMMENT COMMENT COMMENT ; IDENT COMMENT ; IDENT ;",
            ),
            ("foo    /* an EOF terminates a line */", "IDENT COMMENT ;"),
            (
                "foo    /* an EOF terminates a line */ /*",
                "IDENT COMMENT COMMENT ;",
            ),
            (
                "foo    /* an EOF terminates a line */ //",
                "IDENT COMMENT COMMENT ;",
            ),
            (
                "package main\n\nfunc main() {\n\tif {\n\t\treturn /* */ }\n}\n",
                "package IDENT ; func IDENT ( ) { if { return COMMENT } ; } ;",
            ),
            ("package main", "package IDENT ;"),
        ];

        for (input, want) in cases {
            let input = input.as_bytes();
            check_semi(input, want, Mode::default());
            check_semi(input, want, SCAN_COMMENTS);

            // if the input ended in newlines, the input must tokenize the
            // same with or without those newlines
            let mut i = input.len();
            while i > 0 && input[i - 1] == b'\n' {
                i -= 1;
                check_semi(&input[..i], want, Mode::default());
                check_semi(&input[..i], want, SCAN_COMMENTS);
            }
        }
    }

    // ----- TestLineDirectives -----

    struct Segment {
        srcline: &'static str,  // a line of source text
        filename: &'static str, // filename for current token; error message for invalid line directives
        line: i64, // line and column for current token; error position for invalid line directives
        column: i64,
    }

    fn test_segments(segments: &[Segment], filename: &str) {
        let mut src = String::new();
        for e in segments {
            src.push_str(e.srcline);
        }
        let src_bytes = src.into_bytes();

        let mut fset = FileSet::new();
        let file = fset.add_file(filename, -1, src_bytes.len() as i64);
        let mut s = Scanner::new(
            file.clone(),
            &src_bytes,
            Some(fatal_error_handler()),
            DONT_INSERT_SEMIS,
        );
        for e in segments {
            let (p, _, lit) = s.scan();
            let pos = file.position(p);
            let want = Position {
                file_name: e.filename.to_string(),
                offset: pos.offset,
                line: e.line,
                column: e.column,
            };
            check_pos(&lit, &pos, &want);
        }

        assert_eq!(s.error_count, 0, "got {} errors", s.error_count);
    }

    #[test]
    fn test_line_directives() {
        let segments: &[Segment] = &[
            // exactly one token per line since the test consumes one token per segment
            Segment {
                srcline: "  line1",
                filename: "TestLineDirectives",
                line: 1,
                column: 3,
            },
            Segment {
                srcline: "\nline2",
                filename: "TestLineDirectives",
                line: 2,
                column: 1,
            },
            Segment {
                srcline: "\nline3  //line File1.go:100",
                filename: "TestLineDirectives",
                line: 3,
                column: 1,
            }, // bad line comment, ignored
            Segment {
                srcline: "\nline4",
                filename: "TestLineDirectives",
                line: 4,
                column: 1,
            },
            Segment {
                srcline: "\n//line File1.go:100\n  line100",
                filename: "File1.go",
                line: 100,
                column: 0,
            },
            Segment {
                srcline: "\n//line  \t :42\n  line1",
                filename: " \t ",
                line: 42,
                column: 0,
            },
            Segment {
                srcline: "\n//line File2.go:200\n  line200",
                filename: "File2.go",
                line: 200,
                column: 0,
            },
            Segment {
                srcline: "\n//line foo\t:42\n  line42",
                filename: "foo\t",
                line: 42,
                column: 0,
            },
            Segment {
                srcline: "\n //line foo:42\n  line43",
                filename: "foo\t",
                line: 44,
                column: 0,
            }, // bad line comment, ignored (use existing, prior filename)
            Segment {
                srcline: "\n//line foo 42\n  line44",
                filename: "foo\t",
                line: 46,
                column: 0,
            }, // bad line comment, ignored (use existing, prior filename)
            Segment {
                srcline: "\n//line /bar:42\n  line45",
                filename: "/bar",
                line: 42,
                column: 0,
            },
            Segment {
                srcline: "\n//line ./foo:42\n  line46",
                filename: "foo",
                line: 42,
                column: 0,
            },
            Segment {
                srcline: "\n//line a/b/c/File1.go:100\n  line100",
                filename: "a/b/c/File1.go",
                line: 100,
                column: 0,
            },
            Segment {
                srcline: "\n//line c:\\bar:42\n  line200",
                filename: "c:\\bar",
                line: 42,
                column: 0,
            },
            Segment {
                srcline: "\n//line c:\\dir\\File1.go:100\n  line201",
                filename: "c:\\dir\\File1.go",
                line: 100,
                column: 0,
            },
            // tests for new line directive syntax
            Segment {
                srcline: "\n//line :100\na1",
                filename: "",
                line: 100,
                column: 0,
            }, // missing filename means empty filename
            Segment {
                srcline: "\n//line bar:100\nb1",
                filename: "bar",
                line: 100,
                column: 0,
            },
            Segment {
                srcline: "\n//line :100:10\nc1",
                filename: "bar",
                line: 100,
                column: 10,
            }, // missing filename means current filename
            Segment {
                srcline: "\n//line foo:100:10\nd1",
                filename: "foo",
                line: 100,
                column: 10,
            },
            Segment {
                srcline: "\n/*line :100*/a2",
                filename: "",
                line: 100,
                column: 0,
            }, // missing filename means empty filename
            Segment {
                srcline: "\n/*line bar:100*/b2",
                filename: "bar",
                line: 100,
                column: 0,
            },
            Segment {
                srcline: "\n/*line :100:10*/c2",
                filename: "bar",
                line: 100,
                column: 10,
            }, // missing filename means current filename
            Segment {
                srcline: "\n/*line foo:100:10*/d2",
                filename: "foo",
                line: 100,
                column: 10,
            },
            Segment {
                srcline: "\n/*line foo:100:10*/    e2",
                filename: "foo",
                line: 100,
                column: 14,
            }, // line-directive relative column
            Segment {
                srcline: "\n/*line foo:100:10*/\n\nf2",
                filename: "foo",
                line: 102,
                column: 1,
            }, // absolute column since on new line
        ];
        test_segments(segments, "TestLineDirectives");

        let dirsegments: &[Segment] = &[
            // exactly one token per line since the test consumes one token per segment
            Segment {
                srcline: "  line1",
                filename: "TestLineDir/TestLineDirectives",
                line: 1,
                column: 3,
            },
            Segment {
                srcline: "\n//line File1.go:100\n  line100",
                filename: "TestLineDir/File1.go",
                line: 100,
                column: 0,
            },
        ];
        test_segments(dirsegments, "TestLineDir/TestLineDirectives");

        // Go runs the Unix variant on non-Windows platforms; this port
        // implements Go's Unix filepath semantics deterministically.
        let dir_unix_segments: &[Segment] = &[Segment {
            srcline: "\n//line /bar:42\n  line42",
            filename: "/bar",
            line: 42,
            column: 0,
        }];
        test_segments(dir_unix_segments, "TestLineDir/TestLineDirectives");
    }

    // The filename is used for the error message in these test cases.
    // The first line directive is valid and used to control the expected error line.
    #[test]
    fn test_invalid_line_directives() {
        let invalid_segments: &[Segment] = &[
            Segment {
                srcline: "\n//line :1:1\n//line foo:42 extra text\ndummy",
                filename: "invalid line number: 42 extra text",
                line: 1,
                column: 12,
            },
            Segment {
                srcline: "\n//line :2:1\n//line foobar:\ndummy",
                filename: "invalid line number: ",
                line: 2,
                column: 15,
            },
            Segment {
                srcline: "\n//line :5:1\n//line :0\ndummy",
                filename: "invalid line number: 0",
                line: 5,
                column: 9,
            },
            Segment {
                srcline: "\n//line :10:1\n//line :1:0\ndummy",
                filename: "invalid column number: 0",
                line: 10,
                column: 11,
            },
            Segment {
                srcline: "\n//line :1:1\n//line :foo:0\ndummy",
                filename: "invalid line number: 0",
                line: 1,
                column: 13,
            }, // foo is considered part of the filename
        ];

        // make source
        let mut src = String::new();
        for e in invalid_segments {
            src.push_str(e.srcline);
        }
        let src_bytes = src.into_bytes();

        // verify scan
        let errors = Rc::new(RefCell::new(Vec::<(Position, String)>::new()));
        let handler = {
            let errors = errors.clone();
            Box::new(move |pos: Position, msg: String| {
                errors.borrow_mut().push((pos, msg));
            }) as ErrorHandler
        };
        let mut fset = FileSet::new();
        let file = fset.add_file("dir/TestInvalidLineDirectives", -1, src_bytes.len() as i64);
        let mut s = Scanner::new(file.clone(), &src_bytes, Some(handler), DONT_INSERT_SEMIS);
        for _ in invalid_segments {
            s.scan();
        }

        assert_eq!(
            s.error_count,
            invalid_segments.len(),
            "got {} errors; want {}",
            s.error_count,
            invalid_segments.len()
        );
        let errors = errors.borrow();
        assert_eq!(
            errors.len(),
            invalid_segments.len(),
            "got {} error handler calls",
            errors.len()
        );
        for (e, (pos, msg)) in invalid_segments.iter().zip(errors.iter()) {
            assert_eq!(msg, e.filename, "got error {msg:?}; want {:?}", e.filename);
            assert_eq!(
                (pos.line, pos.column),
                (e.line, e.column),
                "got position {}:{}",
                pos.line,
                pos.column
            );
        }
    }

    // Verify that initializing the same scanner more than once works correctly.
    #[test]
    fn test_init() {
        let mut fset = FileSet::new();

        // 1st init
        let src1: &[u8] = b"if true { }";
        let f1 = fset.add_file("src1", -1, src1.len() as i64);
        let mut s = Scanner::new(f1.clone(), src1, None, DONT_INSERT_SEMIS);
        assert_eq!(f1.size(), src1.len() as i64, "bad file size");
        s.scan(); // if
        s.scan(); // true
        let (_, tok, _) = s.scan(); // {
        assert_eq!(
            tok,
            Token::LBrace,
            "bad token: got {tok:?}, expected LBrace"
        );

        // 2nd init
        let src2: &[u8] = b"go true { ]";
        let f2 = fset.add_file("src2", -1, src2.len() as i64);
        s.init(f2.clone(), src2, None, DONT_INSERT_SEMIS);
        assert_eq!(f2.size(), src2.len() as i64, "bad file size");
        let (_, tok, _) = s.scan(); // go
        assert_eq!(tok, Token::Go, "bad token: got {tok:?}, expected Go");

        assert_eq!(s.error_count, 0, "found {} errors", s.error_count);
    }

    #[test]
    fn test_std_error_handler() {
        let src: &[u8] = b"@\n@ @\n//line File2:20\n@\n//line File2:1\n@ @\n//line File1:1\n@ @ @";

        let list = Rc::new(RefCell::new(ErrorList::default()));
        let handler = {
            let list = list.clone();
            Box::new(move |pos: Position, msg: String| {
                list.borrow_mut().add(pos, msg);
            }) as ErrorHandler
        };

        let mut fset = FileSet::new();
        let file = fset.add_file("File1", -1, src.len() as i64);
        let mut s = Scanner::new(file.clone(), src, Some(handler), DONT_INSERT_SEMIS);
        loop {
            let (_, tok, _) = s.scan();
            if tok == Token::EOF {
                break;
            }
        }

        let mut list = list.borrow_mut();
        assert_eq!(
            list.len(),
            s.error_count,
            "found {} errors, expected {}",
            list.len(),
            s.error_count
        );
        assert_eq!(list.len(), 9, "found {} raw errors, expected 9", list.len());

        list.sort();
        assert_eq!(
            list.len(),
            9,
            "found {} sorted errors, expected 9",
            list.len()
        );

        list.remove_multiples();
        assert_eq!(
            list.len(),
            4,
            "found {} one-per-line errors, expected 4",
            list.len()
        );
    }

    // ----- TestScanErrors -----

    struct ErrorCollector {
        cnt: usize,    // number of errors encountered
        msg: String,   // last error message encountered
        pos: Position, // last error position encountered
    }

    fn check_error(src: &[u8], tok: Token, pos: i64, lit: &[u8], err: &str) {
        let collector = Rc::new(RefCell::new(ErrorCollector {
            cnt: 0,
            msg: String::new(),
            pos: Position::default(),
        }));
        let handler = {
            let collector = collector.clone();
            Box::new(move |pos: Position, msg: String| {
                let mut h = collector.borrow_mut();
                h.cnt += 1;
                h.msg = msg;
                h.pos = pos;
            }) as ErrorHandler
        };
        let mut fset = FileSet::new();
        let file = fset.add_file("", -1, src.len() as i64);
        let mut s = Scanner::new(
            file.clone(),
            src,
            Some(handler),
            SCAN_COMMENTS | DONT_INSERT_SEMIS,
        );
        let (_, tok0, lit0) = s.scan();
        assert_eq!(tok0, tok, "{src:?}: got {tok0:?}, expected {tok:?}");
        if tok0 != Token::Illegal {
            assert_eq!(
                lit0.as_bytes(),
                lit,
                "{src:?}: got literal {:?}, expected {:?}",
                lit0.as_bytes(),
                lit
            );
        }
        let cnt = if err.is_empty() { 0 } else { 1 };
        let h = collector.borrow();
        assert_eq!(h.cnt, cnt, "{src:?}: got cnt {}, expected {}", h.cnt, cnt);
        assert_eq!(
            h.msg, err,
            "{src:?}: got msg {:?}, expected {:?}",
            h.msg, err
        );
        assert_eq!(
            h.pos.offset, pos,
            "{src:?}: got offset {}, expected {}",
            h.pos.offset, pos
        );
    }

    // one (src, token, error offset, literal, error message) case
    type ErrorCase = (&'static [u8], Token, i64, &'static [u8], &'static str);

    #[test]
    fn test_scan_errors() {
        let cases: &[ErrorCase] = &[
            (b"\x07", Token::Illegal, 0, b"", "illegal character U+0007"),
            (b"#", Token::Illegal, 0, b"", "illegal character U+0023 '#'"),
            (
                b"\xE2\x80\xA6",
                Token::Illegal,
                0,
                b"",
                "illegal character U+2026 '\u{2026}'",
            ),
            (b"..", Token::Period, 0, b"", ""), // two periods, not invalid token (issue #28112)
            (b"' '", Token::Char, 0, b"' '", ""),
            (b"''", Token::Char, 0, b"''", "illegal rune literal"),
            (b"'12'", Token::Char, 0, b"'12'", "illegal rune literal"),
            (b"'123'", Token::Char, 0, b"'123'", "illegal rune literal"),
            (
                b"'\\0'",
                Token::Char,
                3,
                b"'\\0'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\07'",
                Token::Char,
                4,
                b"'\\07'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\8'",
                Token::Char,
                2,
                b"'\\8'",
                "unknown escape sequence",
            ),
            (
                b"'\\08'",
                Token::Char,
                3,
                b"'\\08'",
                "illegal character U+0038 '8' in escape sequence",
            ),
            (
                b"'\\x'",
                Token::Char,
                3,
                b"'\\x'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\x0'",
                Token::Char,
                4,
                b"'\\x0'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\x0g'",
                Token::Char,
                4,
                b"'\\x0g'",
                "illegal character U+0067 'g' in escape sequence",
            ),
            (
                b"'\\u'",
                Token::Char,
                3,
                b"'\\u'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\u0'",
                Token::Char,
                4,
                b"'\\u0'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\u00'",
                Token::Char,
                5,
                b"'\\u00'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\u000'",
                Token::Char,
                6,
                b"'\\u000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\u000",
                Token::Char,
                6,
                b"'\\u000",
                "escape sequence not terminated",
            ),
            (b"'\\u0000'", Token::Char, 0, b"'\\u0000'", ""),
            (
                b"'\\U'",
                Token::Char,
                3,
                b"'\\U'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U0'",
                Token::Char,
                4,
                b"'\\U0'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U00'",
                Token::Char,
                5,
                b"'\\U00'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U000'",
                Token::Char,
                6,
                b"'\\U000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U0000'",
                Token::Char,
                7,
                b"'\\U0000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U00000'",
                Token::Char,
                8,
                b"'\\U00000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U000000'",
                Token::Char,
                9,
                b"'\\U000000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U0000000'",
                Token::Char,
                10,
                b"'\\U0000000'",
                "illegal character U+0027 ''' in escape sequence",
            ),
            (
                b"'\\U0000000",
                Token::Char,
                10,
                b"'\\U0000000",
                "escape sequence not terminated",
            ),
            (b"'\\U00000000'", Token::Char, 0, b"'\\U00000000'", ""),
            (
                b"'\\Uffffffff'",
                Token::Char,
                2,
                b"'\\Uffffffff'",
                "escape sequence is invalid Unicode code point",
            ),
            (b"'", Token::Char, 0, b"'", "rune literal not terminated"),
            (
                b"'\\",
                Token::Char,
                2,
                b"'\\",
                "escape sequence not terminated",
            ),
            (b"'\n", Token::Char, 0, b"'", "rune literal not terminated"),
            (
                b"'\n   ",
                Token::Char,
                0,
                b"'",
                "rune literal not terminated",
            ),
            (b"\"\"", Token::String, 0, b"\"\"", ""),
            (
                b"\"abc",
                Token::String,
                0,
                b"\"abc",
                "string literal not terminated",
            ),
            (
                b"\"abc\n",
                Token::String,
                0,
                b"\"abc",
                "string literal not terminated",
            ),
            (
                b"\"abc\n   ",
                Token::String,
                0,
                b"\"abc",
                "string literal not terminated",
            ),
            (b"``", Token::String, 0, b"``", ""),
            (
                b"`",
                Token::String,
                0,
                b"`",
                "raw string literal not terminated",
            ),
            (b"/**/", Token::Comment, 0, b"/**/", ""),
            (b"/*", Token::Comment, 0, b"/*", "comment not terminated"),
            (b"077", Token::Int, 0, b"077", ""),
            (b"078.", Token::Float, 0, b"078.", ""),
            (b"07801234567.", Token::Float, 0, b"07801234567.", ""),
            (b"078e0", Token::Float, 0, b"078e0", ""),
            (b"0E", Token::Float, 2, b"0E", "exponent has no digits"), // issue 17621
            (
                b"078",
                Token::Int,
                2,
                b"078",
                "invalid digit '8' in octal literal",
            ),
            (
                b"07090000008",
                Token::Int,
                3,
                b"07090000008",
                "invalid digit '9' in octal literal",
            ),
            (
                b"0x",
                Token::Int,
                2,
                b"0x",
                "hexadecimal literal has no digits",
            ),
            (
                b"\"abc\x00def\"",
                Token::String,
                4,
                b"\"abc\x00def\"",
                "illegal character NUL",
            ),
            // adapted: Go keeps the invalid byte in the literal, Rust replaces
            // it with U+FFFD (String::from_utf8_lossy)
            (
                b"\"abc\x80def\"",
                Token::String,
                4,
                b"\"abc\xEF\xBF\xBDdef\"",
                "illegal UTF-8 encoding",
            ),
            (
                b"\xEF\xBB\xBF\xEF\xBB\xBF",
                Token::Illegal,
                3,
                b"",
                "illegal byte order mark",
            ), // only first BOM is ignored
            (
                b"//\xEF\xBB\xBF",
                Token::Comment,
                2,
                b"//\xEF\xBB\xBF",
                "illegal byte order mark",
            ), // only first BOM is ignored
            (
                b"'\xEF\xBB\xBF'",
                Token::Char,
                1,
                b"'\xEF\xBB\xBF'",
                "illegal byte order mark",
            ), // only first BOM is ignored
            (
                b"\"abc\xEF\xBB\xBFdef\"",
                Token::String,
                4,
                b"\"abc\xEF\xBB\xBFdef\"",
                "illegal byte order mark",
            ), // only first BOM is ignored
            (
                b"abc\x00def",
                Token::Ident,
                3,
                b"abc",
                "illegal character NUL",
            ),
            (b"abc\x00", Token::Ident, 3, b"abc", "illegal character NUL"),
            (
                b"\xE2\x80\x9Cabc\xE2\x80\x9D",
                Token::Illegal,
                0,
                b"",
                "curly quotation mark '\u{201C}' (use neutral '\"')",
            ),
        ];
        for e in cases {
            check_error(e.0, e.1, e.2, e.3, e.4);
        }
    }

    #[test]
    fn test_utf16() {
        // This test doesn't fit within test_scan_errors because
        // the latter assumes that there was only one error.
        for src in [
            b"\xfe\xff\x00p\x00a\x00c\x00k\x00a\x00g\x00e\x00 \x00p".as_slice(), // BOM + "package p" encoded as UTF-16 BE
            b"\xff\xfep\x00a\x00c\x00k\x00a\x00g\x00e\x00 \x00p\x00".as_slice(), // BOM + "package p" encoded as UTF-16 LE
        ] {
            let got = Rc::new(RefCell::new(Vec::<String>::new()));
            let handler = {
                let got = got.clone();
                Box::new(move |posn: Position, msg: String| {
                    got.borrow_mut().push(format!("#{}: {}", posn.offset, msg));
                }) as ErrorHandler
            };
            let mut fset = FileSet::new();
            let file = fset.add_file("", -1, src.len() as i64);
            let mut s = Scanner::new(file.clone(), src, Some(handler), Mode::default());
            s.scan();

            // We expect two errors:
            // one from the decoder, one from the scanner.
            let want = [
                "#0: illegal UTF-8 encoding (got UTF-16)",
                "#0: illegal character U+FFFD '\u{FFFD}'",
            ];
            let got = got.borrow();
            assert_eq!(&got[..], &want[..], "Scan({src:?}) returned errors");
        }
    }

    // Verify that no comments show up as literal values when skipping comments.
    #[test]
    fn test_issue10213() {
        let src: &[u8] = b"
			var (
				A = 1 // foo
			)

			var (
				B = 2
				// foo
			)

			var C = 3 // foo

			var D = 4
			// foo

			func anycode() {
			// foo
			}
		";
        let mut fset = FileSet::new();
        let file = fset.add_file("", -1, src.len() as i64);
        let mut s = Scanner::new(file.clone(), src, None, Mode::default());
        loop {
            let (pos, tok, lit) = s.scan();
            let class = token_class(tok);
            if !lit.is_empty() && class != KEYWORD && class != LITERAL && tok != Token::Semicolon {
                panic!("{}: tok = {tok}, lit = {lit:?}", file.position(pos));
            }
            if tok == Token::EOF {
                break;
            }
        }
    }

    #[test]
    fn test_issue28112() {
        let src: &[u8] = b"... .. 0.. .."; // make sure to have stand-alone ".." immediately before EOF to test EOF behavior
        let tokens: [Token; 8] = [
            Token::Ellipsis,
            Token::Period,
            Token::Period,
            Token::Float,
            Token::Period,
            Token::Period,
            Token::Period,
            Token::EOF,
        ];
        let mut fset = FileSet::new();
        let file = fset.add_file("", -1, src.len() as i64);
        let mut s = Scanner::new(file.clone(), src, None, Mode::default());
        for want in tokens {
            let (pos, got, lit) = s.scan();
            assert_eq!(
                got,
                want,
                "{}: got {got:?}, want {want:?}",
                file.position(pos)
            );
            // literals expect to have a (non-empty) literal string and we don't care about other tokens for this test
            if token_class(got) == LITERAL && lit.is_empty() {
                panic!(
                    "{}: for {got:?} got empty literal string",
                    file.position(pos)
                );
            }
        }
    }

    // ----- TestNumbers -----

    // (first-token names below use the Go test's token vocabulary)
    #[test]
    fn test_numbers() {
        let cases: &[(&str, &str, &str, &str)] = &[
            // binaries
            ("INT", "0b0", "0b0", ""),
            ("INT", "0b1010", "0b1010", ""),
            ("INT", "0B1110", "0B1110", ""),
            ("INT", "0b", "0b", "binary literal has no digits"),
            (
                "INT",
                "0b0190",
                "0b0190",
                "invalid digit '9' in binary literal",
            ),
            ("INT", "0b01a0", "0b01 a0", ""), // only accept 0-9
            (
                "FLOAT",
                "0b.",
                "0b.",
                "invalid radix point in binary literal",
            ),
            (
                "FLOAT",
                "0b.1",
                "0b.1",
                "invalid radix point in binary literal",
            ),
            (
                "FLOAT",
                "0b1.0",
                "0b1.0",
                "invalid radix point in binary literal",
            ),
            (
                "FLOAT",
                "0b1e10",
                "0b1e10",
                "'e' exponent requires decimal mantissa",
            ),
            (
                "FLOAT",
                "0b1P-1",
                "0b1P-1",
                "'P' exponent requires hexadecimal mantissa",
            ),
            ("IMAG", "0b10i", "0b10i", ""),
            (
                "IMAG",
                "0b10.0i",
                "0b10.0i",
                "invalid radix point in binary literal",
            ),
            // octals
            ("INT", "0o0", "0o0", ""),
            ("INT", "0o1234", "0o1234", ""),
            ("INT", "0O1234", "0O1234", ""),
            ("INT", "0o", "0o", "octal literal has no digits"),
            (
                "INT",
                "0o8123",
                "0o8123",
                "invalid digit '8' in octal literal",
            ),
            (
                "INT",
                "0o1293",
                "0o1293",
                "invalid digit '9' in octal literal",
            ),
            ("INT", "0o12a3", "0o12 a3", ""), // only accept 0-9
            (
                "FLOAT",
                "0o.",
                "0o.",
                "invalid radix point in octal literal",
            ),
            (
                "FLOAT",
                "0o.2",
                "0o.2",
                "invalid radix point in octal literal",
            ),
            (
                "FLOAT",
                "0o1.2",
                "0o1.2",
                "invalid radix point in octal literal",
            ),
            (
                "FLOAT",
                "0o1E+2",
                "0o1E+2",
                "'E' exponent requires decimal mantissa",
            ),
            (
                "FLOAT",
                "0o1p10",
                "0o1p10",
                "'p' exponent requires hexadecimal mantissa",
            ),
            ("IMAG", "0o10i", "0o10i", ""),
            (
                "IMAG",
                "0o10e0i",
                "0o10e0i",
                "'e' exponent requires decimal mantissa",
            ),
            // 0-octals
            ("INT", "0", "0", ""),
            ("INT", "0123", "0123", ""),
            (
                "INT",
                "08123",
                "08123",
                "invalid digit '8' in octal literal",
            ),
            (
                "INT",
                "01293",
                "01293",
                "invalid digit '9' in octal literal",
            ),
            ("INT", "0F.", "0 F .", ""), // only accept 0-9
            ("INT", "0123F.", "0123 F .", ""),
            ("INT", "0123456x", "0123456 x", ""),
            // decimals
            ("INT", "1", "1", ""),
            ("INT", "1234", "1234", ""),
            ("INT", "1f", "1 f", ""), // only accept 0-9
            ("IMAG", "0i", "0i", ""),
            ("IMAG", "0678i", "0678i", ""),
            // decimal floats
            ("FLOAT", "0.", "0.", ""),
            ("FLOAT", "123.", "123.", ""),
            ("FLOAT", "0123.", "0123.", ""),
            ("FLOAT", ".0", ".0", ""),
            ("FLOAT", ".123", ".123", ""),
            ("FLOAT", ".0123", ".0123", ""),
            ("FLOAT", "0.0", "0.0", ""),
            ("FLOAT", "123.123", "123.123", ""),
            ("FLOAT", "0123.0123", "0123.0123", ""),
            ("FLOAT", "0e0", "0e0", ""),
            ("FLOAT", "123e+0", "123e+0", ""),
            ("FLOAT", "0123E-1", "0123E-1", ""),
            ("FLOAT", "0.e+1", "0.e+1", ""),
            ("FLOAT", "123.E-10", "123.E-10", ""),
            ("FLOAT", "0123.e123", "0123.e123", ""),
            ("FLOAT", ".0e-1", ".0e-1", ""),
            ("FLOAT", ".123E+10", ".123E+10", ""),
            ("FLOAT", ".0123E123", ".0123E123", ""),
            ("FLOAT", "0.0e1", "0.0e1", ""),
            ("FLOAT", "123.123E-10", "123.123E-10", ""),
            ("FLOAT", "0123.0123e+456", "0123.0123e+456", ""),
            ("FLOAT", "0e", "0e", "exponent has no digits"),
            ("FLOAT", "0E+", "0E+", "exponent has no digits"),
            ("FLOAT", "1e+f", "1e+ f", "exponent has no digits"),
            (
                "FLOAT",
                "0p0",
                "0p0",
                "'p' exponent requires hexadecimal mantissa",
            ),
            (
                "FLOAT",
                "1.0P-1",
                "1.0P-1",
                "'P' exponent requires hexadecimal mantissa",
            ),
            ("IMAG", "0.i", "0.i", ""),
            ("IMAG", ".123i", ".123i", ""),
            ("IMAG", "123.123i", "123.123i", ""),
            ("IMAG", "123e+0i", "123e+0i", ""),
            ("IMAG", "123.E-10i", "123.E-10i", ""),
            ("IMAG", ".123E+10i", ".123E+10i", ""),
            // hexadecimals
            ("INT", "0x0", "0x0", ""),
            ("INT", "0x1234", "0x1234", ""),
            ("INT", "0xcafef00d", "0xcafef00d", ""),
            ("INT", "0XCAFEF00D", "0XCAFEF00D", ""),
            ("INT", "0x", "0x", "hexadecimal literal has no digits"),
            ("INT", "0x1g", "0x1 g", ""),
            ("IMAG", "0xf00i", "0xf00i", ""),
            // hexadecimal floats
            ("FLOAT", "0x0p0", "0x0p0", ""),
            ("FLOAT", "0x12efp-123", "0x12efp-123", ""),
            ("FLOAT", "0xABCD.p+0", "0xABCD.p+0", ""),
            ("FLOAT", "0x.0189P-0", "0x.0189P-0", ""),
            ("FLOAT", "0x1.ffffp+1023", "0x1.ffffp+1023", ""),
            ("FLOAT", "0x.", "0x.", "hexadecimal literal has no digits"),
            (
                "FLOAT",
                "0x0.",
                "0x0.",
                "hexadecimal mantissa requires a 'p' exponent",
            ),
            (
                "FLOAT",
                "0x.0",
                "0x.0",
                "hexadecimal mantissa requires a 'p' exponent",
            ),
            (
                "FLOAT",
                "0x1.1",
                "0x1.1",
                "hexadecimal mantissa requires a 'p' exponent",
            ),
            (
                "FLOAT",
                "0x1.1e0",
                "0x1.1e0",
                "hexadecimal mantissa requires a 'p' exponent",
            ),
            (
                "FLOAT",
                "0x1.2gp1a",
                "0x1.2 gp1a",
                "hexadecimal mantissa requires a 'p' exponent",
            ),
            ("FLOAT", "0x0p", "0x0p", "exponent has no digits"),
            ("FLOAT", "0xeP-", "0xeP-", "exponent has no digits"),
            ("FLOAT", "0x1234PAB", "0x1234P AB", "exponent has no digits"),
            ("FLOAT", "0x1.2p1a", "0x1.2p1 a", ""),
            ("IMAG", "0xf00.bap+12i", "0xf00.bap+12i", ""),
            // separators
            ("INT", "0b_1000_0001", "0b_1000_0001", ""),
            ("INT", "0o_600", "0o_600", ""),
            ("INT", "0_466", "0_466", ""),
            ("INT", "1_000", "1_000", ""),
            ("FLOAT", "1_000.000_1", "1_000.000_1", ""),
            ("IMAG", "10e+1_2_3i", "10e+1_2_3i", ""),
            ("INT", "0x_f00d", "0x_f00d", ""),
            ("FLOAT", "0x_f00d.0p1_2", "0x_f00d.0p1_2", ""),
            (
                "INT",
                "0b__1000",
                "0b__1000",
                "'_' must separate successive digits",
            ),
            (
                "INT",
                "0o60___0",
                "0o60___0",
                "'_' must separate successive digits",
            ),
            (
                "INT",
                "0466_",
                "0466_",
                "'_' must separate successive digits",
            ),
            ("FLOAT", "1_.", "1_.", "'_' must separate successive digits"),
            (
                "FLOAT",
                "0._1",
                "0._1",
                "'_' must separate successive digits",
            ),
            (
                "FLOAT",
                "2.7_e0",
                "2.7_e0",
                "'_' must separate successive digits",
            ),
            (
                "IMAG",
                "10e+12_i",
                "10e+12_i",
                "'_' must separate successive digits",
            ),
            (
                "INT",
                "0x___0",
                "0x___0",
                "'_' must separate successive digits",
            ),
            (
                "FLOAT",
                "0x1.0_p0",
                "0x1.0_p0",
                "'_' must separate successive digits",
            ),
        ];
        let expected_tok = |name: &str| -> Token {
            match name {
                "INT" => Token::Int,
                "FLOAT" => Token::Float,
                "IMAG" => Token::Imag,
                _ => unreachable!(),
            }
        };

        for (tok_name, src, tokens, err) in cases {
            let src = src.as_bytes();
            let last_err = Rc::new(RefCell::new(String::new()));
            let handler = {
                let last_err = last_err.clone();
                Box::new(move |_: Position, msg: String| {
                    let mut e = last_err.borrow_mut();
                    if e.is_empty() {
                        *e = msg;
                    }
                }) as ErrorHandler
            };
            let mut fset = FileSet::new();
            let file = fset.add_file("", -1, src.len() as i64);
            let mut s = Scanner::new(file.clone(), src, Some(handler), Mode::default());
            for (i, want) in tokens.split(' ').enumerate() {
                *last_err.borrow_mut() = String::new();
                let (_, tok, mut lit) = s.scan();

                // compute lit where for tokens where lit is not defined
                match tok {
                    Token::Period => lit = ".".to_string(),
                    Token::Add => lit = "+".to_string(),
                    Token::Sub => lit = "-".to_string(),
                    _ => {}
                }

                if i == 0 {
                    assert_eq!(
                        tok,
                        expected_tok(tok_name),
                        "{src:?}: got token {tok:?}; want {tok_name}"
                    );
                    let e = last_err.borrow();
                    assert_eq!(e.as_str(), *err, "{src:?}: got error {e:?}; want {err:?}");
                }

                assert_eq!(
                    lit, *want,
                    "{src:?}: got literal {lit:?} ({tok:?}); want {want:?}"
                );
            }

            // make sure we read all
            let (_, mut tok, _) = s.scan();
            if tok == Token::Semicolon {
                (_, tok, _) = s.scan();
            }
            assert_eq!(tok, Token::EOF, "{src:?}: got {tok:?}; want EOF");
        }
    }

    // ----- reuse tests and TestScannerEnd -----

    #[test]
    fn test_scan_reuse_semi_in_newline_comment() {
        let src: &[u8] = b"identifier /*a\nb*/ + other";
        let mut fset = FileSet::new();
        let file = fset.add_file("test.go", -1, src.len() as i64);
        let mut s = Scanner::new(
            file.clone(),
            src,
            Some(fatal_error_handler()),
            SCAN_COMMENTS,
        );

        s.scan(); // IDENT(identifier)

        let (_, tok, _) = s.scan(); // COMMENT(/*a\nb*/)
        assert_eq!(tok, Token::Comment, "tok = {tok:?}; want = COMMENT");

        let file = fset.add_file("test.go", -1, src.len() as i64);
        s.init(file, src, Some(fatal_error_handler()), SCAN_COMMENTS);

        let (_, tok, _) = s.scan();
        assert_eq!(tok, Token::Ident, "tok = {tok:?}; want = IDENT");
    }

    #[test]
    fn test_scanner_end() {
        struct Case {
            name: &'static str,
            src: &'static [u8],
            // (token, start offset, end offset)
            end: &'static [(Token, i64, i64)],
        }

        // offsets below equal the Go test's absolute positions minus the
        // base of 1 used for the test file.
        let cases: &[Case] = &[
            Case {
                name: "operators",
                src: b"+ - / >> == =",
                end: &[
                    (Token::Add, 0, 1),
                    (Token::Sub, 2, 3),
                    (Token::Quo, 4, 5),
                    (Token::Shr, 6, 8),
                    (Token::Equal, 9, 11),
                    (Token::Assign, 12, 13),
                    (Token::EOF, 13, 13),
                ],
            },
            Case {
                name: "braces",
                src: b"{([])}",
                end: &[
                    (Token::LBrace, 0, 1),
                    (Token::LParen, 1, 2),
                    (Token::LBrack, 2, 3),
                    (Token::RBrack, 3, 4),
                    (Token::RParen, 4, 5),
                    (Token::RBrace, 5, 6),
                    (Token::Semicolon, 6, 6),
                    (Token::EOF, 6, 6),
                ],
            },
            Case {
                name: "literals",
                src: b"\"foo\" 123 1.23 0b11",
                end: &[
                    (Token::String, 0, 5),
                    (Token::Int, 6, 9),
                    (Token::Float, 10, 14),
                    (Token::Int, 15, 19),
                    (Token::Semicolon, 19, 19),
                    (Token::EOF, 19, 19),
                ],
            },
            Case {
                name: "missing newline at the end of file",
                src: b"foo",
                end: &[
                    (Token::Ident, 0, 3),
                    (Token::Semicolon, 3, 3),
                    (Token::EOF, 3, 3),
                ],
            },
            Case {
                name: "newline at the end of file",
                src: b"foo\n",
                end: &[
                    (Token::Ident, 0, 3),
                    (Token::Semicolon, 3, 4),
                    (Token::EOF, 4, 4),
                ],
            },
            Case {
                name: "semicolon at the end of file",
                src: b"foo;",
                end: &[
                    (Token::Ident, 0, 3),
                    (Token::Semicolon, 3, 4),
                    (Token::EOF, 4, 4),
                ],
            },
            Case {
                name: "semicolon and newline at the end of file",
                src: b"foo;\n",
                end: &[
                    (Token::Ident, 0, 3),
                    (Token::Semicolon, 3, 4),
                    (Token::EOF, 5, 5),
                ],
            },
            Case {
                name: "newline in comment acting as semicolon",
                src: b"foo /*\n*/ bar",
                end: &[
                    (Token::Ident, 0, 3),
                    (Token::Comment, 4, 9),
                    (Token::Semicolon, 6, 7),
                    (Token::Ident, 10, 13),
                    (Token::Semicolon, 13, 13),
                    (Token::EOF, 13, 13),
                ],
            },
            Case {
                name: "BOM",
                src: "\u{FEFF}foo".as_bytes(),
                end: &[
                    (Token::Ident, 3, 6),
                    (Token::Semicolon, 6, 6),
                    (Token::EOF, 6, 6),
                ],
            },
        ];

        for tt in cases {
            let mut fset = FileSet::new();
            let file = fset.add_file("test.go", -1, tt.src.len() as i64);

            let mut s = Scanner::new(
                file.clone(),
                tt.src,
                Some(fatal_error_handler()),
                SCAN_COMMENTS,
            );

            assert_eq!(
                s.end(),
                NoPos,
                "after init in {}: s.end() = {:?}; want NoPos",
                tt.name,
                s.end()
            );

            let mut got: Vec<(Token, Pos, Pos)> = Vec::new();
            loop {
                let (pos, tok, _) = s.scan();
                got.push((tok, pos, s.end()));
                if tok == Token::EOF {
                    break;
                }
            }

            let want: Vec<(Token, Pos, Pos)> = tt
                .end
                .iter()
                .map(|(tok, start, end)| (*tok, file.pos(*start), file.pos(*end)))
                .collect();
            assert_eq!(
                got, want,
                "input {:?} in {}: got = {:?}; want = {:?}",
                tt.src, tt.name, got, want
            );
        }
    }

    #[test]
    fn test_scanner_end_reuse() {
        let src: &[u8] = b"identifier /*a\nb*/ + other";
        let mut fset = FileSet::new();
        let file = fset.add_file("test.go", -1, src.len() as i64);
        let mut s = Scanner::new(
            file.clone(),
            src,
            Some(fatal_error_handler()),
            SCAN_COMMENTS,
        );

        s.scan(); // IDENT(identifier)
        s.scan(); // COMMENT(/*a\nb*/)

        let (_, tok, _) = s.scan(); // SEMICOLON
        assert_eq!(tok, Token::Semicolon, "tok = {tok:?}; want = SEMICOLON");

        let file = fset.add_file("test.go", -1, src.len() as i64);
        s.init(file, src, Some(fatal_error_handler()), SCAN_COMMENTS);

        assert_eq!(s.end(), NoPos, "s.end() = {:?}; want NoPos", s.end());
    }

    // ----- ExampleScanner_Scan (example_test.go) -----

    #[test]
    fn example_scanner_scan() {
        // src is the input that we want to tokenize.
        let src: &[u8] = b"cos(x) + 1i*sin(x) // Euler";

        // Initialize the scanner.
        let mut fset = FileSet::new(); // positions are relative to fset
        let file = fset.add_file("", -1, src.len() as i64); // register input "file"
        let mut s = Scanner::new(
            file.clone(),
            src,
            None, /* no error handler */
            SCAN_COMMENTS,
        );

        // Repeated calls to scan yield the token sequence found in the input.
        let mut got = String::new();
        loop {
            let (pos, tok, lit) = s.scan();
            if tok == Token::EOF {
                break;
            }
            got.push_str(&format!(
                "{}\t{}\t{:?}\n",
                file.position(pos),
                go_string(tok),
                lit
            ));
        }

        let want = "\
1:1\tIDENT\t\"cos\"
1:4\t(\t\"\"
1:5\tIDENT\t\"x\"
1:6\t)\t\"\"
1:8\t+\t\"\"
1:10\tIMAG\t\"1i\"
1:12\t*\t\"\"
1:13\tIDENT\t\"sin\"
1:16\t(\t\"\"
1:17\tIDENT\t\"x\"
1:18\t)\t\"\"
1:20\tCOMMENT\t\"// Euler\"
1:28\t;\t\"\\n\"
";
        assert_eq!(got, want);
    }
}
