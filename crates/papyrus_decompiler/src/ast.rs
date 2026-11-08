//! The reconstructed source: expressions and statements, and their printing.
//!
//! Names and strings are [`Text`]s: borrowed from the string table of the
//! `.pex` file (`'p`), owned only for the rare strings that are not UTF-8.

use std::{borrow::Cow, fmt::Write as _};

/// A name or string of the reconstructed source.
pub(crate) type Text<'p> = Cow<'p, str>;

/// A binary operator, with its precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinOp {
    fn as_str(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
        }
    }

    fn precedence(self) -> u8 {
        match self {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::Add | BinOp::Sub => 4,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 5,
        }
    }

    /// Operators that can be written as `x op= y`.
    fn compound(self) -> bool {
        matches!(self, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem)
    }
}

const UNARY: u8 = 6;
const CAST: u8 = 7;
const POSTFIX: u8 = 8;

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr<'p> {
    None,
    Int(i32),
    Float(f32),
    Bool(bool),
    Str(Text<'p>),
    /// A local, parameter, variable, property or script name.
    Name(Text<'p>),
    SelfRef,
    ParentRef,
    Field(Box<Expr<'p>>, Text<'p>),
    /// A call; without receiver for functions of `Self`.
    Call(Option<Box<Expr<'p>>>, Text<'p>, Vec<Expr<'p>>),
    Index(Box<Expr<'p>>, Box<Expr<'p>>),
    Cast(Box<Expr<'p>>, Text<'p>),
    /// A conversion the compiler inserts by itself (to `Bool`, to `String`,
    /// `Int` to `Float`): printed as the inner expression.
    Implicit(Box<Expr<'p>>, Text<'p>),
    New(Text<'p>, Box<Expr<'p>>),
    Not(Box<Expr<'p>>),
    Neg(Box<Expr<'p>>),
    Binary(BinOp, Box<Expr<'p>>, Box<Expr<'p>>),
}

impl Expr<'_> {
    fn precedence(&self) -> u8 {
        match self {
            Expr::Binary(op, ..) => op.precedence(),
            Expr::Not(_) | Expr::Neg(_) => UNARY,
            Expr::Cast(..) => CAST,
            Expr::Implicit(e, _) => e.precedence(),
            // Negative numbers print with their sign.
            Expr::Int(i) if *i < 0 => UNARY,
            Expr::Float(f) if f.is_sign_negative() => UNARY,
            _ => POSTFIX,
        }
    }

    /// Whether evaluating the expression may have effects (calls).
    pub(crate) fn has_call(&self) -> bool {
        match self {
            Expr::Call(..) => true,
            Expr::Field(e, _)
            | Expr::Cast(e, _)
            | Expr::Implicit(e, _)
            | Expr::New(_, e)
            | Expr::Not(e)
            | Expr::Neg(e) => e.has_call(),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => a.has_call() || b.has_call(),
            _ => false,
        }
    }

    /// Calls `f` on every name the expression uses.
    pub(crate) fn names(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Expr::Name(n) => f(n),
            Expr::Field(e, _)
            | Expr::Cast(e, _)
            | Expr::Implicit(e, _)
            | Expr::New(_, e)
            | Expr::Not(e)
            | Expr::Neg(e) => e.names(f),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                a.names(f);
                b.names(f);
            }
            Expr::Call(recv, _, args) => {
                if let Some(r) = recv {
                    r.names(f);
                }
                for a in args {
                    a.names(f);
                }
            }
            _ => {}
        }
    }

    /// Whether the expression uses `name` (ignoring case).
    pub(crate) fn uses(&self, name: &str) -> bool {
        let mut found = false;
        self.names(&mut |n| found |= n.eq_ignore_ascii_case(name));
        found
    }

    pub(crate) fn write(&self, out: &mut String) {
        match self {
            Expr::None => out.push_str("None"),
            Expr::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Expr::Float(f) => write_float(out, *f),
            Expr::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
            Expr::Str(s) => write_string(out, s),
            Expr::Name(n) => out.push_str(n),
            Expr::SelfRef => out.push_str("Self"),
            Expr::ParentRef => out.push_str("Parent"),
            Expr::Field(e, name) => {
                self.child(out, e, POSTFIX);
                out.push('.');
                out.push_str(name);
            }
            Expr::Call(recv, name, args) => {
                if let Some(r) = recv {
                    self.child(out, r, POSTFIX);
                    out.push('.');
                }
                out.push_str(name);
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    a.write(out);
                }
                out.push(')');
            }
            Expr::Index(a, i) => {
                self.child(out, a, POSTFIX);
                out.push('[');
                i.write(out);
                out.push(']');
            }
            Expr::Implicit(e, _) => e.write(out),
            Expr::Cast(e, ty) => {
                self.child(out, e, CAST);
                out.push_str(" as ");
                out.push_str(ty);
            }
            Expr::New(elem, size) => {
                out.push_str("new ");
                out.push_str(elem);
                out.push('[');
                size.write(out);
                out.push(']');
            }
            Expr::Not(e) => {
                out.push('!');
                self.child(out, e, UNARY);
            }
            Expr::Neg(e) => {
                out.push('-');
                self.child(out, e, UNARY);
            }
            Expr::Binary(op, l, r) => {
                let p = op.precedence();
                self.child(out, l, p);
                let _ = write!(out, " {} ", op.as_str());
                // Left associative: parenthesize a right operand of the same
                // precedence.
                self.child(out, r, p + 1);
            }
        }
    }

    fn child(&self, out: &mut String, e: &Expr<'_>, min: u8) {
        // `(i as Float) / 2` reads better than `i as Float / 2`.
        let cast_operand = matches!(self, Expr::Binary(..)) && matches!(e, Expr::Cast(..));
        if e.precedence() < min || cast_operand {
            out.push('(');
            e.write(out);
            out.push(')');
        } else {
            e.write(out);
        }
    }
}

/// A float literal: always with a `.`, so that it reads back as a float.
fn write_float(out: &mut String, f: f32) {
    let start = out.len();
    let _ = write!(out, "{f}");
    if !out[start..].contains(['.', 'e', 'E', 'i', 'N']) {
        out.push_str(".0");
    }
}

/// A string literal: quoted, with `"`, `\`, newlines and tabs escaped.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stmt<'p> {
    /// `Type name = value`
    Local(Text<'p>, Text<'p>, Option<Expr<'p>>),
    Assign(Expr<'p>, Expr<'p>),
    Expr(Expr<'p>),
    Return(Option<Expr<'p>>),
    If(Vec<(Expr<'p>, Vec<Stmt<'p>>)>, Option<Vec<Stmt<'p>>>),
    While(Expr<'p>, Vec<Stmt<'p>>),
}

impl Stmt<'_> {
    /// Calls `f` on every name the statement uses, nested blocks included.
    pub(crate) fn names(&self, f: &mut dyn FnMut(&str)) {
        match self {
            Stmt::Local(_, name, init) => {
                f(name);
                if let Some(e) = init {
                    e.names(f);
                }
            }
            Stmt::Assign(t, v) => {
                t.names(f);
                v.names(f);
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) => e.names(f),
            Stmt::Return(None) => {}
            Stmt::If(branches, else_branch) => {
                for (c, b) in branches {
                    c.names(f);
                    for s in b {
                        s.names(f);
                    }
                }
                for s in else_branch.iter().flatten() {
                    s.names(f);
                }
            }
            Stmt::While(c, b) => {
                c.names(f);
                for s in b {
                    s.names(f);
                }
            }
        }
    }

    /// Whether the statement uses `name` (ignoring case).
    pub(crate) fn uses(&self, name: &str) -> bool {
        let mut found = false;
        self.names(&mut |n| found |= n.eq_ignore_ascii_case(name));
        found
    }
}

/// Prints statements, indented by `depth` levels of four spaces.
pub(crate) fn write_block(out: &mut String, stmts: &[Stmt<'_>], depth: usize) {
    for s in stmts {
        write_stmt(out, s, depth);
    }
}

pub(crate) fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

fn write_stmt(out: &mut String, stmt: &Stmt<'_>, depth: usize) {
    indent(out, depth);
    match stmt {
        Stmt::Local(ty, name, init) => {
            let _ = write!(out, "{ty} {name}");
            if let Some(e) = init {
                out.push_str(" = ");
                e.write(out);
            }
        }
        Stmt::Assign(target, value) => {
            target.write(out);
            match value {
                Expr::Binary(op, l, r) if op.compound() && **l == *target => {
                    let _ = write!(out, " {}= ", op.as_str());
                    r.write(out);
                }
                _ => {
                    out.push_str(" = ");
                    value.write(out);
                }
            }
        }
        Stmt::Expr(e) => e.write(out),
        Stmt::Return(value) => {
            out.push_str("Return");
            if let Some(e) = value {
                out.push(' ');
                e.write(out);
            }
        }
        Stmt::If(branches, else_branch) => {
            for (i, (cond, body)) in branches.iter().enumerate() {
                if i > 0 {
                    indent(out, depth);
                }
                out.push_str(if i == 0 { "If " } else { "ElseIf " });
                cond.write(out);
                out.push('\n');
                write_block(out, body, depth + 1);
            }
            if let Some(body) = else_branch {
                indent(out, depth);
                out.push_str("Else\n");
                write_block(out, body, depth + 1);
            }
            indent(out, depth);
            out.push_str("EndIf");
        }
        Stmt::While(cond, body) => {
            out.push_str("While ");
            cond.write(out);
            out.push('\n');
            write_block(out, body, depth + 1);
            indent(out, depth);
            out.push_str("EndWhile");
        }
    }
    out.push('\n');
}
