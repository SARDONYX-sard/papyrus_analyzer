//! Decompilation of function bodies.
//!
//! Compilers keep every intermediate value in a temporary (`::tempN`) that is
//! written once and read once, right after. Expressions are rebuilt by
//! keeping the value of each temporary pending until an instruction reads it;
//! instructions writing real variables, and calls whose result is not used,
//! become statements.
//!
//! Control flow follows the shapes compilers generate:
//!
//! - `If`: `JMPF c, next; then; JMP end; next: else...`, `ElseIf` being an
//!   `If` alone in the `Else` part;
//! - `While`: `top: cond; JMPF c, end; body; JMP top; end:`;
//! - `&&` / `||`: `CAST r, a; JMPF/JMPT r, end; b...; CAST r, b; end:`.
//!
//! Names are compared by [`ScriptCx::key`], which ignores case.

use std::borrow::Cow;

use papyrus_pex::pex::{FunctionInfo, Instruction, OpCode, StringId, VariableValue};
use rustc_hash::FxHashMap;

use crate::{
    ScriptCx,
    ast::{BinOp, Expr, Stmt, Text},
    sub_text,
};

/// Decompiles the code of a function.
pub(crate) fn decompile<'p>(cx: &ScriptCx<'p>, info: &'p FunctionInfo) -> Vec<Stmt<'p>> {
    let locals = info.params.iter().chain(&info.locals).map(|v| (cx.key(v.name), v.ty)).collect();
    let mut b = Body { cx, insts: &info.instructions, locals, pending: Vec::new() };
    let mut out = Vec::new();
    b.block(0, info.instructions.len(), &mut out, true);
    // A void `Return` at the very end is implied.
    if matches!(out.last(), Some(Stmt::Return(None))) {
        out.pop();
    }
    declare_locals(cx, info, &mut out);
    out
}

struct Body<'c, 'p> {
    cx: &'c ScriptCx<'p>,
    insts: &'p [Instruction],
    /// Parameters and locals to their type, by key.
    locals: FxHashMap<StringId, StringId>,
    /// Values of temporaries not read yet, by key, in definition order.
    pending: Vec<(StringId, Expr<'p>)>,
}

/// `::tempN`; `::temporarilyCleared_var` is a property variable.
fn is_temp(name: &str) -> bool {
    name.len() > 6
        && name[..6].eq_ignore_ascii_case("::temp")
        && name[6..].bytes().all(|b| b.is_ascii_digit())
}

/// Names and values.
impl<'p> Body<'_, 'p> {
    /// The name of an identifier operand.
    fn ident(&self, v: &VariableValue) -> Option<StringId> {
        match *v {
            VariableValue::Identifier(id) => Some(id),
            _ => None,
        }
    }

    /// The name of an identifier operand, as text (`""` for other values).
    fn ident_text(&self, v: &VariableValue) -> Text<'p> {
        self.ident(v).map_or(Cow::Borrowed(""), |id| self.cx.str(id))
    }

    /// Whether `v` is the identifier `id`, ignoring case.
    fn is_ident(&self, v: &VariableValue, id: StringId) -> bool {
        self.ident(v).is_some_and(|i| self.cx.key(i) == self.cx.key(id))
    }

    /// The declared type of a parameter, local, variable or property.
    fn declared_type(&self, id: StringId) -> Option<Text<'p>> {
        let key = self.cx.key(id);
        let ty = self.locals.get(&key).or_else(|| self.cx.member_types.get(&key))?;
        Some(self.cx.str(*ty))
    }

    /// The type of a value.
    fn type_of(&self, v: &VariableValue) -> Option<Text<'p>> {
        Some(Cow::Borrowed(match *v {
            VariableValue::Null => "None",
            VariableValue::Identifier(id) => {
                if self.cx.str(id).eq_ignore_ascii_case("self") {
                    return Some(self.cx.script.clone());
                }
                return self.declared_type(id);
            }
            VariableValue::String(_) => "String",
            VariableValue::Integer(_) => "Int",
            VariableValue::Float(_) => "Float",
            VariableValue::Boolean(_) => "Bool",
        }))
    }

    /// The expression of a value; reading a temporary consumes it.
    fn take(&mut self, v: &VariableValue) -> Expr<'p> {
        match *v {
            VariableValue::Null => Expr::None,
            VariableValue::Integer(i) => Expr::Int(i),
            VariableValue::Float(f) => Expr::Float(f),
            VariableValue::Boolean(b) => Expr::Bool(b),
            VariableValue::String(id) => Expr::Str(self.cx.str(id)),
            VariableValue::Identifier(id) => {
                let key = self.cx.key(id);
                if is_temp(&self.cx.str(id))
                    && let Some(i) = self.pending.iter().position(|(k, _)| *k == key)
                {
                    return self.pending.remove(i).1;
                }
                self.name(id)
            }
        }
    }

    /// A name as written in the source.
    fn name(&self, id: StringId) -> Expr<'p> {
        let name = self.cx.str(id);
        if name.eq_ignore_ascii_case("self") {
            return Expr::SelfRef;
        }
        if name.eq_ignore_ascii_case("::NoneVar") {
            return Expr::None;
        }
        if let Some(&prop) = self.cx.auto_vars.get(&self.cx.key(id)) {
            return Expr::Name(self.cx.str(prop));
        }
        // The hidden variable of an auto property of a parent script.
        if name.starts_with("::") && name.ends_with("_var") && name.len() >= 6 {
            return Expr::Name(sub_text(name, |n| &n[2..n.len() - 4]));
        }
        // A temporary read without definition (code this decompiler does not
        // understand): keep a valid name.
        Expr::Name(sub_text(name, |n| n.trim_start_matches(':')))
    }
}

/// Statements.
impl<'p> Body<'_, 'p> {
    /// Calls whose result is pending become statements, in order, so that
    /// they run before what follows.
    fn flush_calls(&mut self, out: &mut Vec<Stmt<'p>>) {
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].1.has_call() {
                out.push(Stmt::Expr(self.pending.remove(i).1));
            } else {
                i += 1;
            }
        }
    }

    fn emit(&mut self, stmt: Stmt<'p>, out: &mut Vec<Stmt<'p>>) {
        self.flush_calls(out);
        out.push(stmt);
    }

    /// Writes `value` to `dest`: a temporary keeps it pending, `::NoneVar`
    /// drops it, a variable gets an assignment.
    fn define(&mut self, dest: &VariableValue, value: Expr<'p>, out: &mut Vec<Stmt<'p>>) {
        let Some(id) = self.ident(dest) else { return };
        let name = self.cx.str(id);
        if is_temp(&name) {
            let key = self.cx.key(id);
            if let Some(i) = self.pending.iter().position(|(k, _)| *k == key) {
                // Overwritten before being read: only calls matter.
                let (_, old) = self.pending.remove(i);
                if old.has_call() {
                    self.emit(Stmt::Expr(old), out);
                }
            }
            self.pending.push((key, value));
        } else if name.eq_ignore_ascii_case("::NoneVar") {
            self.emit(Stmt::Expr(value), out);
        } else {
            let target = self.name(id);
            self.emit(Stmt::Assign(target, value), out);
        }
    }
}

/// Control flow.
impl<'p> Body<'_, 'p> {
    /// The instruction a jump at `index` goes to.
    fn target(&self, index: usize, inst: &Instruction) -> usize {
        let offset = match inst.args.last() {
            Some(VariableValue::Integer(o)) => *o as i64,
            _ => 1,
        };
        (index as i64 + offset).max(0) as usize
    }

    fn block(&mut self, start: usize, end: usize, out: &mut Vec<Stmt<'p>>, flush: bool) {
        let mut i = start;
        while i < end {
            i = match self.loop_end(i, end) {
                Some(j) => self.while_loop(i, j, out),
                None => self.step(i, end, out),
            };
        }
        if flush {
            // Values nobody reads come from expression statements, calls or
            // not (`x == 1` alone compiles).
            out.extend(self.pending.drain(..).map(|(_, e)| Stmt::Expr(e)));
        }
    }

    /// The jump closing a loop starting at `i`: the last `JMP` back to it.
    fn loop_end(&self, i: usize, end: usize) -> Option<usize> {
        (i + 1..end).rev().find(|&j| {
            let inst = &self.insts[j];
            inst.opcode == OpCode::Jmp && self.target(j, inst) == i
        })
    }

    fn while_loop(&mut self, start: usize, back: usize, out: &mut Vec<Stmt<'p>>) -> usize {
        self.flush_calls(out);
        let mut k = start;
        // The code computing the condition, up to the jump out of the loop.
        while k < back {
            let inst = &self.insts[k];
            if matches!(inst.opcode, OpCode::JmpF | OpCode::JmpT) && self.target(k, inst) > back {
                break;
            }
            k = self.step(k, back, out);
        }
        if k >= back {
            // An endless loop: `While True`.
            let mut body = Vec::new();
            self.block(start, back, &mut body, true);
            out.push(Stmt::While(Expr::Bool(true), body));
            return back + 1;
        }
        let inst = &self.insts[k];
        let cond = self.take(&inst.args[0]);
        let cond = if inst.opcode == OpCode::JmpT { not(cond) } else { cond };
        let mut body = Vec::new();
        self.block(k + 1, back, &mut body, true);
        out.push(Stmt::While(cond, body));
        back + 1
    }

    /// Decompiles the instruction at `i`, or the construct starting there,
    /// and returns the index of the next one.
    fn step(&mut self, i: usize, end: usize, out: &mut Vec<Stmt<'p>>) -> usize {
        let inst = &self.insts[i];
        match inst.opcode {
            OpCode::Cast => match self.logical(i, end, out) {
                Some(next) => next,
                None => {
                    self.simple(inst, out);
                    i + 1
                }
            },
            OpCode::JmpF | OpCode::JmpT => self.if_stmt(i, end, out),
            // Jumps to the next instruction, or that no construct explains.
            OpCode::Jmp => i + 1,
            _ => {
                self.simple(inst, out);
                i + 1
            }
        }
    }

    /// `CAST r, a; JMPF r, end; ...; CAST r, b; end:` is `a && b` (`||` with
    /// `JMPT`).
    fn logical(&mut self, i: usize, end: usize, out: &mut Vec<Stmt<'p>>) -> Option<usize> {
        let cast = &self.insts[i];
        let jump = self.insts.get(i + 1)?;
        let r = self.ident(&cast.args[0])?;
        if !matches!(jump.opcode, OpCode::JmpF | OpCode::JmpT) || !self.is_ident(&jump.args[0], r) {
            return None;
        }
        let after = self.target(i + 1, jump);
        if after <= i + 2 || after > end {
            return None;
        }
        let last = &self.insts[after - 1];
        if last.opcode != OpCode::Cast || !self.is_ident(&last.args[0], r) {
            return None;
        }
        let a = self.take(&cast.args[1]);
        self.block(i + 2, after - 1, out, false);
        let b = self.take(&last.args[1]);
        let op = if jump.opcode == OpCode::JmpF { BinOp::And } else { BinOp::Or };
        self.define(&cast.args[0], Expr::Binary(op, Box::new(a), Box::new(b)), out);
        Some(after)
    }

    fn if_stmt(&mut self, i: usize, end: usize, out: &mut Vec<Stmt<'p>>) -> usize {
        let inst = &self.insts[i];
        let cond = self.take(&inst.args[0]);
        let cond = if inst.opcode == OpCode::JmpT { not(cond) } else { cond };
        self.flush_calls(out);
        let next = self.target(i, inst).clamp(i + 1, end);
        // `JMP end` closing the `then` part when there is an `Else`.
        let else_end = (next >= i + 2)
            .then(|| (next - 1, &self.insts[next - 1]))
            .filter(|(j, last)| last.opcode == OpCode::Jmp && self.target(*j, last) > next)
            .map(|(j, last)| self.target(j, last).min(end));
        let mut then = Vec::new();
        self.block(i + 1, if else_end.is_some() { next - 1 } else { next }, &mut then, true);
        let mut branches = vec![(cond, then)];
        let mut else_branch = None;
        if let Some(e) = else_end {
            let mut els = Vec::new();
            self.block(next, e, &mut els, true);
            match els.as_mut_slice() {
                [Stmt::If(inner, inner_else)] => {
                    branches.append(inner);
                    else_branch = inner_else.take();
                }
                [] => {}
                _ => else_branch = Some(els),
            }
        }
        out.push(Stmt::If(branches, else_branch));
        else_end.unwrap_or(next)
    }
}

/// Instructions without control flow: each reads its operands with
/// [`Body::take`], then [`Body::define`]s its destination or emits a
/// statement.
impl<'p> Body<'_, 'p> {
    fn simple(&mut self, inst: &Instruction, out: &mut Vec<Stmt<'p>>) {
        use OpCode::*;
        let a = &inst.args;
        match inst.opcode {
            Nop | Jmp | JmpT | JmpF => {}
            IAdd | FAdd | ISub | FSub | IMul | FMul | IDiv | FDiv | IMod | StrCat | CmpEq
            | CmpLt | CmpLe | CmpGt | CmpGe => {
                let op = match inst.opcode {
                    IAdd | FAdd | StrCat => BinOp::Add,
                    ISub | FSub => BinOp::Sub,
                    IMul | FMul => BinOp::Mul,
                    IDiv | FDiv => BinOp::Div,
                    IMod => BinOp::Rem,
                    CmpEq => BinOp::Eq,
                    CmpLt => BinOp::Lt,
                    CmpLe => BinOp::Le,
                    CmpGt => BinOp::Gt,
                    _ => BinOp::Ge,
                };
                let l = self.take(&a[1]);
                let r = self.take(&a[2]);
                // When both operands were converted, the conversion decides
                // the operation (`(i as Float) / j`): keep one explicit.
                let l = match (l, &r) {
                    (Expr::Implicit(e, ty), Expr::Implicit(..)) => Expr::Cast(e, ty),
                    (l, _) => l,
                };
                self.define(&a[0], Expr::Binary(op, Box::new(l), Box::new(r)), out);
            }
            Not => {
                let e = match self.take(&a[1]) {
                    Expr::Binary(BinOp::Eq, l, r) => Expr::Binary(BinOp::Ne, l, r),
                    e => not(e),
                };
                self.define(&a[0], e, out);
            }
            INeg | FNeg => {
                let e = match self.take(&a[1]) {
                    Expr::Int(i) => Expr::Int(i.wrapping_neg()),
                    Expr::Float(f) => Expr::Float(-f),
                    e => Expr::Neg(Box::new(e)),
                };
                self.define(&a[0], e, out);
            }
            Assign => {
                let v = self.take(&a[1]);
                self.define(&a[0], v, out);
            }
            Cast => {
                let to = self.ident(&a[0]).and_then(|d| self.declared_type(d));
                let from = self.type_of(&a[1]);
                let v = self.take(&a[1]);
                let e = match (to, from) {
                    (_, Some(f)) if f.eq_ignore_ascii_case("None") => v,
                    // `x as T` of an object already of type `T` is explicit.
                    (Some(t), Some(f)) if t.eq_ignore_ascii_case(&f) && is_primitive(&f) => v,
                    (Some(t), Some(f))
                        if t.eq_ignore_ascii_case("bool")
                            || t.eq_ignore_ascii_case("string")
                            || (t.eq_ignore_ascii_case("float")
                                && f.eq_ignore_ascii_case("int")) =>
                    {
                        Expr::Implicit(Box::new(v), t)
                    }
                    (Some(t), _) => Expr::Cast(Box::new(v), t),
                    (None, _) => v,
                };
                self.define(&a[0], e, out);
            }
            CallMethod => {
                let recv = match self.take(&a[1]) {
                    Expr::SelfRef => None,
                    r => Some(Box::new(r)),
                };
                let args = self.call_args(a, 4);
                let name = self.ident_text(&a[0]);
                let name = if name.eq_ignore_ascii_case("GotoState") {
                    Cow::Borrowed("GoToState")
                } else {
                    name
                };
                self.define(&a[2], Expr::Call(recv, name, args), out);
            }
            CallParent => {
                let args = self.call_args(a, 3);
                let name = self.ident_text(&a[0]);
                self.define(&a[1], Expr::Call(Some(Box::new(Expr::ParentRef)), name, args), out);
            }
            CallStatic => {
                let script = self.ident_text(&a[0]);
                let args = self.call_args(a, 4);
                let name = self.ident_text(&a[1]);
                // Functions of the script itself are called without it.
                let recv = (!script.eq_ignore_ascii_case(&self.cx.script))
                    .then(|| Box::new(Expr::Name(script)));
                self.define(&a[2], Expr::Call(recv, name, args), out);
            }
            Ret => {
                let v = match &a[0] {
                    VariableValue::Null => None,
                    v => Some(self.take(v)),
                };
                self.emit(Stmt::Return(v), out);
            }
            PropGet => {
                let recv = self.take(&a[1]);
                let name = self.ident_text(&a[0]);
                self.define(&a[2], field(recv, name), out);
            }
            PropSet => {
                let recv = self.take(&a[1]);
                let v = self.take(&a[2]);
                let name = self.ident_text(&a[0]);
                self.emit(Stmt::Assign(field(recv, name), v), out);
            }
            ArrayCreate => {
                let ty = self.ident(&a[0]).and_then(|d| self.declared_type(d));
                let elem =
                    sub_text(ty.unwrap_or(Cow::Borrowed("Int")), |t| t.trim_end_matches("[]"));
                let size = self.take(&a[1]);
                self.define(&a[0], Expr::New(elem, Box::new(size)), out);
            }
            ArrayLength => {
                let arr = self.take(&a[1]);
                self.define(&a[0], Expr::Field(Box::new(arr), Cow::Borrowed("Length")), out);
            }
            ArrayGetElement => {
                let arr = self.take(&a[1]);
                let i = self.take(&a[2]);
                self.define(&a[0], Expr::Index(Box::new(arr), Box::new(i)), out);
            }
            ArraySetElement => {
                let arr = self.take(&a[0]);
                let i = self.take(&a[1]);
                let v = self.take(&a[2]);
                self.emit(Stmt::Assign(Expr::Index(Box::new(arr), Box::new(i)), v), out);
            }
            ArrayFindElement | ArrayRFindElement => {
                let arr = self.take(&a[0]);
                let value = self.take(&a[2]);
                let start = self.take(&a[3]);
                let (name, default) =
                    if inst.opcode == ArrayFindElement { ("Find", 0) } else { ("RFind", -1) };
                // The start index is left out when it is the default one.
                let mut args = vec![value];
                if start != Expr::Int(default) {
                    args.push(start);
                }
                let call = Expr::Call(Some(Box::new(arr)), Cow::Borrowed(name), args);
                self.define(&a[1], call, out);
            }
        }
    }

    /// The variadic arguments of a call, after the count at `count`.
    fn call_args(&mut self, args: &[VariableValue], count: usize) -> Vec<Expr<'p>> {
        args.iter().skip(count).map(|v| self.take(v)).collect()
    }
}

fn is_primitive(ty: &str) -> bool {
    ["bool", "int", "float", "string"].iter().any(|p| ty.eq_ignore_ascii_case(p))
}

fn not(e: Expr<'_>) -> Expr<'_> {
    match e {
        Expr::Not(inner) => *inner,
        Expr::Binary(BinOp::Eq, l, r) => Expr::Binary(BinOp::Ne, l, r),
        Expr::Binary(BinOp::Ne, l, r) => Expr::Binary(BinOp::Eq, l, r),
        e => Expr::Not(Box::new(e)),
    }
}

/// `recv.name`, or `name` on `Self`.
fn field<'p>(recv: Expr<'p>, name: Text<'p>) -> Expr<'p> {
    match recv {
        Expr::SelfRef => Expr::Name(name),
        r => Expr::Field(Box::new(r), name),
    }
}

/// Declares the locals of the function: at their first assignment when it
/// starts the innermost block using them, else before their first use.
fn declare_locals<'p>(cx: &ScriptCx<'p>, info: &FunctionInfo, stmts: &mut Vec<Stmt<'p>>) {
    let mut unused = Vec::new();
    for local in &info.locals {
        let name = cx.str(local.name);
        // Temporaries and `::NoneVar`.
        if name.starts_with("::") {
            continue;
        }
        let ty = cx.str(local.ty);
        if !place(stmts, &name, &ty) {
            unused.push(Stmt::Local(ty, name, None));
        }
    }
    // Locals no code uses still keep their slot.
    stmts.splice(0..0, unused);
}

/// Declares `name` in `stmts`; `false` when no statement uses it.
fn place<'p>(stmts: &mut Vec<Stmt<'p>>, name: &Text<'p>, ty: &Text<'p>) -> bool {
    let mut users = (0..stmts.len()).filter(|&i| stmts[i].uses(name));
    let Some(first) = users.next() else { return false };
    let only_user = users.next().is_none();
    if only_user {
        // Used in one nested block only: declare it there.
        let placed = match &mut stmts[first] {
            Stmt::If(branches, else_branch) => {
                let cond_use = branches.iter().any(|(c, _)| c.uses(name));
                !cond_use && {
                    let mut using = branches
                        .iter_mut()
                        .map(|(_, b)| b)
                        .chain(else_branch.iter_mut())
                        .filter(|b| b.iter().any(|s| s.uses(name)));
                    match (using.next(), using.next()) {
                        (Some(block), None) => place(block, name, ty),
                        _ => false,
                    }
                }
            }
            Stmt::While(cond, body) => !cond.uses(name) && place(body, name, ty),
            _ => false,
        };
        if placed {
            return true;
        }
    }
    // At the first use: `Type name = value` when it assigns the variable.
    let init = match &stmts[first] {
        Stmt::Assign(Expr::Name(n), value) if n.eq_ignore_ascii_case(name) && !value.uses(name) => {
            Some(value.clone())
        }
        _ => None,
    };
    match init {
        Some(value) => stmts[first] = Stmt::Local(ty.clone(), name.clone(), Some(value)),
        None => stmts.insert(first, Stmt::Local(ty.clone(), name.clone(), None)),
    }
    true
}
