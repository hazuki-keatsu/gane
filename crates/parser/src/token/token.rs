//! Define the token of Go language and some helpers.
//!
//! This module is ported from Go's standard `go/token` package,
//! using Rust enums instead of Go's integer constants,
//! and provides features like string representation, keyword lookup,
//! category checks, and priority calculations.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Token {
    // Special
    Illegal,
    EOF,
    Comment,

    // Literal
    Ident,  // main
    Int,    // 123
    Float,  // 12.3
    Imag,   // 12.3i
    Char,   // 'a'
    String, // "abc"

    // Operator and delimiters
    Add, // +
    Sub, // -
    Mul, // *
    Quo, // /
    Rem, // %

    And,    // &
    Or,     // |
    XOr,    // ^
    Shl,    // <<
    Shr,    // >>
    AndNot, // &^

    AddAssign, // +=
    SubAssign, // -=
    MulAssign, // *=
    QuoAssign, // /=
    RemAssign, // %=

    AndAssign,    // &=
    OrAssign,     // |=
    XOrAssign,    // ^=
    ShlAssign,    // <<=
    ShrAssign,    // >>=
    AndNotAssign, // &^=

    LAnd,  // &&
    LOr,   // ||
    Arrow, // <-
    Inc,   // ++
    Dec,   // --

    Equal,   // ==
    Less,    // <
    Greater, // >
    Assign,  // =
    Not,     // !

    Neq,      // !=
    Leq,      // <=
    Geq,      // >=
    Define,   // :=
    Ellipsis, // ...

    LParen, // (
    LBrack, // [
    LBrace, // {
    Comma,  // ,
    Period, // .

    RParen,    // )
    RBrack,    // ]
    RBrace,    // }
    Semicolon, // ;
    Colon,     // :

    // Keyword
    Break,
    Case,
    Chan,
    Const,
    Continue,

    Default,
    Defer,
    Else,
    FallThrough,
    For,

    Func,
    Go,
    Goto,
    If,
    Import,

    Interface,
    Map,
    Package,
    Range,
    Return,

    Select,
    Struct,
    Switch,
    Type,
    Var,

    // Additional
    Tilde,
}

impl Token {
    fn to_str(&self) -> &str {
        match self {
            Self::Illegal => "Illegal",
            Self::EOF => "EOF",
            Self::Comment => "Comment",
            Self::Ident => "Ident",
            Self::Int => "Int",
            Self::Float => "Float",
            Self::Imag => "Imag",
            Self::Char => "Char",
            Self::String => "String",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Quo => "/",
            Self::Rem => "%",
            Self::And => "&",
            Self::Or => "|",
            Self::XOr => "^",
            Self::Shl => "<<",
            Self::Shr => ">>",
            Self::AndNot => "&^",
            Self::AddAssign => "+=",
            Self::SubAssign => "-=",
            Self::MulAssign => "*=",
            Self::QuoAssign => "/=",
            Self::RemAssign => "%=",
            Self::AndAssign => "&=",
            Self::OrAssign => "|=",
            Self::XOrAssign => "^=",
            Self::ShlAssign => "<<=",
            Self::ShrAssign => ">>=",
            Self::AndNotAssign => "&^=",
            Self::LAnd => "&&",
            Self::LOr => "||",
            Self::Arrow => "<-",
            Self::Inc => "++",
            Self::Dec => "--",
            Self::Equal => "==",
            Self::Less => "<",
            Self::Greater => ">",
            Self::Assign => "=",
            Self::Not => "!",
            Self::Neq => "!=",
            Self::Leq => "<=",
            Self::Geq => ">=",
            Self::Define => ":=",
            Self::Ellipsis => "...",
            Self::LParen => "(",
            Self::LBrack => "[",
            Self::LBrace => "{",
            Self::Comma => ",",
            Self::Period => ".",
            Self::RParen => ")",
            Self::RBrack => "]",
            Self::RBrace => "}",
            Self::Semicolon => ";",
            Self::Colon => ":",
            Self::Break => "break",
            Self::Case => "case",
            Self::Chan => "chan",
            Self::Const => "const",
            Self::Continue => "continue",
            Self::Default => "default",
            Self::Defer => "defer",
            Self::Else => "else",
            Self::FallThrough => "fallthrough",
            Self::For => "for",
            Self::Func => "func",
            Self::Go => "go",
            Self::Goto => "goto",
            Self::If => "if",
            Self::Import => "import",
            Self::Interface => "interface",
            Self::Map => "map",
            Self::Package => "package",
            Self::Range => "range",
            Self::Return => "return",
            Self::Select => "select",
            Self::Struct => "struct",
            Self::Switch => "switch",
            Self::Type => "type",
            Self::Var => "var",
            Self::Tilde => "~",
        }
    }

    pub fn lookup(ident: &str) -> Self {
        match ident {
            "break" => Token::Break,
            "case" => Token::Case,
            "chan" => Token::Chan,
            "const" => Token::Const,
            "continue" => Token::Continue,
            "default" => Token::Default,
            "defer" => Token::Defer,
            "else" => Token::Else,
            "fallthrough" => Token::FallThrough,
            "for" => Token::For,
            "func" => Token::Func,
            "go" => Token::Go,
            "goto" => Token::Goto,
            "if" => Token::If,
            "import" => Token::Import,
            "interface" => Token::Interface,
            "map" => Token::Map,
            "package" => Token::Package,
            "range" => Token::Range,
            "return" => Token::Return,
            "select" => Token::Select,
            "struct" => Token::Struct,
            "switch" => Token::Switch,
            "type" => Token::Type,
            "var" => Token::Var,
            _ => Token::Ident,
        }
    }

    pub fn is_literal(&self) -> bool {
        matches!(
            self,
            Token::Ident | Token::Int | Token::Float | Token::Imag | Token::Char | Token::String
        )
    }

    pub fn is_operator(&self) -> bool {
        matches!(
            self,
            Token::Add
                | Token::Sub
                | Token::Mul
                | Token::Quo
                | Token::Rem
                | Token::And
                | Token::Or
                | Token::XOr
                | Token::Shl
                | Token::Shr
                | Token::AndNot
                | Token::AddAssign
                | Token::SubAssign
                | Token::MulAssign
                | Token::QuoAssign
                | Token::RemAssign
                | Token::AndAssign
                | Token::OrAssign
                | Token::XOrAssign
                | Token::ShlAssign
                | Token::ShrAssign
                | Token::AndNotAssign
                | Token::LAnd
                | Token::LOr
                | Token::Arrow
                | Token::Inc
                | Token::Dec
                | Token::Equal
                | Token::Less
                | Token::Greater
                | Token::Assign
                | Token::Not
                | Token::Neq
                | Token::Leq
                | Token::Geq
                | Token::Define
                | Token::Ellipsis
                | Token::LParen
                | Token::LBrack
                | Token::LBrace
                | Token::Comma
                | Token::Period
                | Token::RParen
                | Token::RBrack
                | Token::RBrace
                | Token::Semicolon
                | Token::Colon
                | Token::Tilde
        )
    }

    pub fn is_keyword(&self) -> bool {
        matches!(
            self,
            Token::Break
                | Token::Case
                | Token::Chan
                | Token::Const
                | Token::Continue
                | Token::Default
                | Token::Defer
                | Token::Else
                | Token::FallThrough
                | Token::For
                | Token::Func
                | Token::Go
                | Token::Goto
                | Token::If
                | Token::Import
                | Token::Interface
                | Token::Map
                | Token::Package
                | Token::Range
                | Token::Return
                | Token::Select
                | Token::Struct
                | Token::Switch
                | Token::Type
                | Token::Var
        )
    }

    /// Get the precedence of token
    ///
    /// 0 means the lowest precedence
    /// 6 means unary token's precedence
    /// 7 means the highest precedence
    pub fn get_precedence(&self) -> i8 {
        match self {
            Self::LOr => 1,
            Self::LAnd => 2,
            Self::Equal | Self::Neq | Self::Less | Self::Leq | Self::Greater | Self::Geq => 3,
            Self::Add | Self::Sub | Self::Or | Self::XOr => 4,
            Self::Mul
            | Self::Quo
            | Self::Rem
            | Self::Shl
            | Self::Shr
            | Self::And
            | Self::AndNot => 5,
            _ => 0,
        }
    }
}

impl core::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

pub fn is_keyword(name: &str) -> bool {
    Token::lookup(name) != Token::Ident
}

pub fn is_exported(name: &str) -> bool {
    name.chars().next().map_or(false, |ch| ch.is_uppercase())
}

pub fn is_identifier(name: &str) -> bool {
    if name.is_empty() || is_keyword(name) {
        return false;
    }
    let mut chars = name.chars();
    if let Some(first) = chars.next() {
        if !(first.is_alphabetic() || first == '_') {
            return false;
        }
        for c in chars {
            if !(c.is_alphanumeric() || c == '_') {
                return false;
            }
        }
        true
    } else {
        false
    }
}
