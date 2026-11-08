//! Code generation of function bodies: HIR to [`FunctionIr`].
//!
//! The generated code follows the conventions of the official compiler:
//!
//! - every intermediate value lives in a temporary `::tempN`, numbered per
//!   script in source order; temporaries are released once their value is
//!   used and recycled for later values of the same type;
//! - calls without result write to `::NoneVar`;
//! - implicit conversions (`Int` to `Float`, anything to `String` or `Bool`)
//!   are explicit `CAST`s;
//! - `&&` and `||` short-circuit, casting both sides to `Bool`;
//! - auto properties of the compiled script are accessed through their
//!   hidden variable, other properties with `PROPGET` / `PROPSET`;
//! - values stored into properties and array elements go through a
//!   temporary;
//! - every `If` branch ends with a jump to the end, even after `Return`.
//!
//! The local table lists, per block, the temporaries first created in it,
//! then the variables it declares, then the nested blocks.
//!
//! Every expression compiles to a [`Val`]: an operand, its type, and whether
//! it is a temporary the consumer must release. The functions below all have
//! the same shape: compute the operands, pick a destination with
//! [`FnGen::dest`], emit the instruction, release the operands.

use std::{collections::VecDeque, io::Write as _, sync::LazyLock};

use rustc_hash::{FxHashMap, FxHashSet};

use papyrus_ide::{
    FileId, LineIndex, TextRange,
    hir::{Arg, BinaryOp, Body, BodyKind, Expr, ExprId, Literal, LocalId, Stmt, StmtId, UnaryOp},
    index::{Flags, MemberKind, TypeRef},
    sema::{Builtin, Def, Sema},
    typeck::{Ty, TypeckResult},
};
use papyrus_intern::Name;
use papyrus_pex::pex::OpCode;

use crate::codegen::{
    constant,
    ir::{FunctionIr, Inst, Label, Operand, Slot},
};

/// `self`: the object a function runs on.
static SELF: LazyLock<Name> = LazyLock::new(|| Name::new("self"));
/// `::NoneVar`: where calls without result write.
static NONE_VAR: LazyLock<Name> = LazyLock::new(|| Name::new("::NoneVar"));

/// The state shared by the functions of one script.
pub(crate) struct ScriptCx<'a> {
    pub(crate) sema: Sema<'a>,
    pub(crate) lines: LineIndex,
    /// The next temporary number.
    pub(crate) temps: u32,
    /// Auto property to its hidden variable (`Count` to `::Count_var`).
    auto_vars: FxHashMap<Name, Name>,
}

impl<'a> ScriptCx<'a> {
    pub(crate) fn new(sema: Sema<'a>) -> ScriptCx<'a> {
        ScriptCx {
            sema,
            lines: LineIndex::new(sema.text),
            temps: 0,
            auto_vars: FxHashMap::default(),
        }
    }

    /// The one-based line of an offset.
    pub(crate) fn line(&self, range: TextRange) -> u32 {
        self.lines.line_col(self.sema.text, range.start()).line + 1
    }

    /// The hidden variable of the auto property `property`.
    fn auto_var(&mut self, property: Name) -> Name {
        *self.auto_vars.entry(property).or_insert_with(|| {
            Name::new(&papyrus_pex::pex::property_auto_var_name(property.as_str()))
        })
    }

    /// A new temporary: `::temp0`, `::temp1`...
    fn next_temp(&mut self) -> Name {
        let n = self.temps;
        self.temps += 1;
        // "::temp" and at most 10 digits, formatted on the stack.
        let mut buf = [0u8; 16];
        let mut rest = &mut buf[..];
        let _ = write!(rest, "::temp{n}");
        let len = 16 - rest.len();
        Name::new(std::str::from_utf8(&buf[..len]).expect("ASCII"))
    }
}

/// The type of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VTy {
    /// Not known; only in code with errors.
    Unknown,
    /// `None`, and results of calls without return type.
    None,
    /// A value.
    Value(TypeRef),
}

impl VTy {
    const BOOL: VTy = VTy::Value(TypeRef::BOOL);
    const INT: VTy = VTy::Value(TypeRef::INT);
    const FLOAT: VTy = VTy::Value(TypeRef::FLOAT);
    const STRING: VTy = VTy::Value(TypeRef::STRING);

    fn of(ty: Ty) -> VTy {
        match ty {
            Ty::Value(t) => VTy::Value(t),
            Ty::None => VTy::None,
            Ty::Unknown | Ty::Script(_) => VTy::Unknown,
        }
    }

    fn of_const(op: &Operand) -> VTy {
        match op {
            Operand::Int(_) => VTy::INT,
            Operand::Float(_) => VTy::FLOAT,
            Operand::Bool(_) => VTy::BOOL,
            Operand::Str(_) => VTy::STRING,
            Operand::None | Operand::Var(_) => VTy::None,
        }
    }

    /// The type of a slot holding the value; `None` (the `None` type) when
    /// it has no type.
    fn slot_type(self) -> Option<TypeRef> {
        match self {
            VTy::Value(t) => Some(t),
            VTy::None | VTy::Unknown => None,
        }
    }

    /// Whether both have the same type in the `.pex` file (names ignore
    /// case, and both `None` and unknown are `None`).
    fn is(self, other: VTy) -> bool {
        self.slot_type() == other.slot_type()
    }
}

/// A computed value.
#[derive(Clone, Debug)]
struct Val {
    op: Operand,
    ty: VTy,
    /// The value is in a temporary owned by the consumer.
    temp: bool,
}

impl Val {
    /// No value: `None`.
    const NONE: Val = Val { op: Operand::None, ty: VTy::None, temp: false };

    fn constant(op: Operand, ty: VTy) -> Val {
        Val { op, ty, temp: false }
    }

    fn var(name: Name, ty: VTy) -> Val {
        Val { op: Operand::Var(name), ty, temp: false }
    }

    fn temp(name: Name, ty: VTy) -> Val {
        Val { op: Operand::Var(name), ty, temp: true }
    }

    /// `self`, the receiver of calls without one.
    fn self_ref() -> Val {
        Val::var(*SELF, VTy::Unknown)
    }
}

#[derive(Default)]
struct Scope {
    temps: Vec<Slot>,
    locals: Vec<Slot>,
    children: Vec<usize>,
}

/// A parameter as seen by a call.
struct ParamSig {
    name: Name,
    ty: Option<TypeRef>,
    default: Option<Operand>,
}

/// Generates the code of one body.
pub(crate) fn function(cx: &mut ScriptCx<'_>, body: &Body, types: &TypeckResult) -> FunctionIr {
    let ret = match body.kind {
        BodyKind::Function { ret, .. } => ret,
        BodyKind::Event => None,
    };
    let mut g = FnGen {
        cx,
        body,
        types,
        ret,
        code: Vec::new(),
        line: 0,
        labels: 0,
        free: FxHashMap::default(),
        scopes: vec![Scope::default()],
        current: 0,
        none_var: false,
        declared: FxHashSet::default(),
    };
    for &stmt in &body.root {
        g.stmt(stmt);
    }
    let params = body
        .locals
        .iter()
        .filter(|l| l.is_param)
        .map(|l| Slot { name: l.name, ty: l.ty })
        .collect();
    let mut locals = Vec::new();
    g.flatten(0, &mut locals);
    FunctionIr { params, locals, code: g.code }
}

struct FnGen<'c, 'a> {
    cx: &'c mut ScriptCx<'a>,
    body: &'c Body,
    types: &'c TypeckResult,
    ret: Option<TypeRef>,
    code: Vec<(Inst, u32)>,
    line: u32,
    labels: u32,
    /// Released temporaries by type, oldest first.
    free: FxHashMap<Option<TypeRef>, VecDeque<Name>>,
    scopes: Vec<Scope>,
    current: usize,
    none_var: bool,
    /// The declared locals.
    declared: FxHashSet<Name>,
}

impl<'c, 'a> FnGen<'c, 'a> {
    fn sema(&self) -> Sema<'a> {
        self.cx.sema
    }

    // === Emission ===

    fn op<const N: usize>(&mut self, opcode: OpCode, args: [Operand; N]) {
        self.code.push((Inst::Op { opcode, args: args.into() }, self.line));
    }

    /// A call: the fixed operands of `opcode`, then the arguments.
    fn call_op<const N: usize>(&mut self, opcode: OpCode, fixed: [Operand; N], args: &[Val]) {
        let mut ops = Vec::with_capacity(N + args.len());
        ops.extend(fixed);
        ops.extend(args.iter().map(|a| a.op.clone()));
        self.code.push((Inst::Op { opcode, args: ops }, self.line));
    }

    fn label(&mut self) -> Label {
        self.labels += 1;
        Label(self.labels - 1)
    }

    fn jump(&mut self, opcode: OpCode, cond: Option<Operand>, target: Label) {
        self.code.push((Inst::Jump { opcode, cond, target }, self.line));
    }

    fn place(&mut self, label: Label) {
        self.code.push((Inst::Label(label), self.line));
    }

    fn set_line(&mut self, range: TextRange) {
        self.line = self.cx.line(range);
    }

    // === Slots ===

    fn new_temp(&mut self, ty: VTy) -> Name {
        let name = self.cx.next_temp();
        self.scopes[self.current].temps.push(Slot { name, ty: ty.slot_type() });
        name
    }

    /// A temporary for the result of an operation on `operands`: a released
    /// one, else an operand temporary of the same type, else a new one.
    fn dest(&mut self, ty: VTy, operands: &[&Val]) -> Name {
        self.dest_among(ty, operands.iter().copied())
    }

    /// [`FnGen::dest`], with the operands as an iterator.
    fn dest_among<'v>(
        &mut self,
        ty: VTy,
        operands: impl DoubleEndedIterator<Item = &'v Val>,
    ) -> Name {
        if let Some(t) = self.free.get_mut(&ty.slot_type()).and_then(VecDeque::pop_front) {
            return t;
        }
        for v in operands.rev() {
            if v.temp
                && v.ty.is(ty)
                && let Operand::Var(n) = v.op
            {
                return n;
            }
        }
        self.new_temp(ty)
    }

    fn release(&mut self, v: &Val) {
        if v.temp
            && let Operand::Var(n) = v.op
        {
            self.free.entry(v.ty.slot_type()).or_default().push_back(n);
        }
    }

    /// Releases `v` unless it is `dest`, which lives on.
    fn release_except(&mut self, v: &Val, dest: Name) {
        if v.op.as_var() != Some(dest) {
            self.release(v);
        }
    }

    fn none_var(&mut self) -> Operand {
        if !self.none_var {
            self.none_var = true;
            self.scopes[self.current].temps.push(Slot { name: *NONE_VAR, ty: None });
        }
        Operand::Var(*NONE_VAR)
    }

    fn local_name(&self, local: LocalId) -> Name {
        self.body.local(local).name
    }

    fn declare(&mut self, local: LocalId) {
        let data = self.body.local(local);
        if self.declared.insert(data.name) {
            self.scopes[self.current].locals.push(Slot { name: data.name, ty: data.ty });
        }
    }

    fn flatten(&mut self, scope: usize, out: &mut Vec<Slot>) {
        let s = std::mem::take(&mut self.scopes[scope]);
        out.extend(s.temps);
        out.extend(s.locals);
        for child in s.children {
            self.flatten(child, out);
        }
    }

    fn block(&mut self, stmts: &[StmtId]) {
        let parent = self.current;
        self.scopes.push(Scope::default());
        let id = self.scopes.len() - 1;
        self.scopes[parent].children.push(id);
        self.current = id;
        for &s in stmts {
            self.stmt(s);
        }
        self.current = parent;
    }

    // === Statements ===

    fn stmt(&mut self, id: StmtId) {
        // The body outlives `self`: matching on it borrows nothing of `self`.
        let body = self.body;
        self.set_line(body.stmt_range(id));
        match body.stmt(id) {
            &Stmt::Local { local, init } => {
                self.declare(local);
                if let Some(init) = init {
                    let v = self.expr(init);
                    let v = self.coerce_opt(v, body.local(local).ty);
                    self.op(OpCode::Assign, [Operand::Var(self.local_name(local)), v.op.clone()]);
                    self.release(&v);
                }
            }
            &Stmt::Assign { op, target, value } => self.assign(op, target, value),
            &Stmt::Expr(e) => {
                let v = self.expr(e);
                self.release(&v);
            }
            &Stmt::Return(Some(e)) => {
                let v = self.expr(e);
                let v = self.coerce_opt(v, self.ret);
                self.op(OpCode::Ret, [v.op.clone()]);
                self.release(&v);
            }
            &Stmt::Return(None) => self.op(OpCode::Ret, [Operand::None]),
            Stmt::If { branches, else_branch } => {
                let end = self.label();
                let count = branches.len();
                let mut conds = Vec::with_capacity(count);
                for (i, (cond, block)) in branches.iter().enumerate() {
                    self.set_line(body.expr_range(*cond));
                    let c = self.expr(*cond);
                    let next =
                        if i + 1 < count || else_branch.is_some() { self.label() } else { end };
                    self.jump(OpCode::JmpF, Some(c.op.clone()), next);
                    // Like the official compiler, condition temporaries are
                    // only recycled after the whole statement.
                    conds.push(c);
                    self.block(block);
                    self.jump(OpCode::Jmp, None, end);
                    if next != end {
                        self.place(next);
                    }
                }
                if let Some(block) = else_branch {
                    self.block(block);
                }
                self.place(end);
                for c in &conds {
                    self.release(c);
                }
            }
            Stmt::While { condition, body: block } => {
                let top = self.label();
                let end = self.label();
                self.place(top);
                self.set_line(body.expr_range(*condition));
                let c = self.expr(*condition);
                self.jump(OpCode::JmpF, Some(c.op.clone()), end);
                self.block(block);
                self.jump(OpCode::Jmp, None, top);
                self.place(end);
                self.release(&c);
            }
        }
    }

    fn assign(&mut self, op: Option<BinaryOp>, target: ExprId, value: ExprId) {
        let ty = self.ty(target);
        match *self.body.expr(target) {
            Expr::Local(local) => self.assign_var(self.local_name(local), ty, op, value),
            Expr::Def(Def::Member { file, index }) => match self.member_place(file, index) {
                Some(var) => self.assign_var(var, ty, op, value),
                None => {
                    let name = self.sema().member(file, index).name;
                    self.assign_prop(None, name, ty, op, value);
                }
            },
            Expr::Field { receiver, def: Some(Def::Member { file, index }), .. } => {
                let m = self.sema().member(file, index);
                if m.kind == MemberKind::Variable {
                    self.assign_var(m.name, ty, op, value);
                } else {
                    self.assign_prop(Some(receiver), m.name, ty, op, value);
                }
            }
            Expr::Index { base, index } => self.assign_element(base, index, ty, op, value),
            _ => {
                let v = self.expr(value);
                self.release(&v);
            }
        }
    }

    /// The variable holding a script member, for variables and auto
    /// properties of the compiled script; `None` for other properties.
    fn member_place(&mut self, file: FileId, index: u32) -> Option<Name> {
        let m = self.sema().member(file, index);
        match m.kind {
            MemberKind::Variable => Some(m.name),
            MemberKind::Property
                if m.flags.contains(Flags::AUTO) && !m.flags.contains(Flags::AUTO_READONLY) =>
            {
                Some(self.cx.auto_var(m.name))
            }
            _ => None,
        }
    }

    /// `var = value`, or `var op= value`.
    fn assign_var(&mut self, var: Name, ty: VTy, op: Option<BinaryOp>, value: ExprId) {
        let v = self.expr(value);
        let v = match op {
            None => v,
            Some(op) => self.binary_vals(op, Val::var(var, ty), v),
        };
        let v = self.coerce_vty(v, ty);
        self.op(OpCode::Assign, [Operand::Var(var), v.op.clone()]);
        self.release(&v);
    }

    /// `base[index] = value`, or `base[index] op= value`.
    fn assign_element(
        &mut self,
        base: ExprId,
        index: ExprId,
        ty: VTy,
        op: Option<BinaryOp>,
        value: ExprId,
    ) {
        let b = self.expr(base);
        let i = self.expr(index);
        let i = self.coerce(i, TypeRef::INT);
        let t = self.dest(ty, &[]);
        let v = self.expr(value);
        let v = match op {
            None => v,
            Some(op) => {
                let cur = self.dest(ty, &[]);
                self.op(OpCode::ArrayGetElement, [Operand::Var(cur), b.op.clone(), i.op.clone()]);
                self.binary_vals(op, Val::temp(cur, ty), v)
            }
        };
        let v = self.coerce_vty(v, ty);
        self.op(OpCode::Assign, [Operand::Var(t), v.op.clone()]);
        self.release(&v);
        self.op(OpCode::ArraySetElement, [b.op.clone(), i.op.clone(), Operand::Var(t)]);
        self.release(&Val::temp(t, ty));
        self.release(&b);
        self.release(&i);
    }

    /// Assigns property `name` of `recv` (`None`: `Self`). The value is
    /// computed before the receiver, unless the operator reads the property.
    fn assign_prop(
        &mut self,
        recv: Option<ExprId>,
        name: Name,
        ty: VTy,
        op: Option<BinaryOp>,
        value: ExprId,
    ) {
        let receiver = |g: &mut Self| match recv {
            Some(r) => g.expr(r),
            None => Val::self_ref(),
        };
        let t = self.dest(ty, &[]);
        let (recv, v) = match op {
            None => {
                let v = self.expr(value);
                (None, v)
            }
            Some(op) => {
                let recv = receiver(self);
                let v = self.expr(value);
                let cur = self.dest(ty, &[]);
                self.op(OpCode::PropGet, [Operand::Var(name), recv.op.clone(), Operand::Var(cur)]);
                let v = self.binary_vals(op, Val::temp(cur, ty), v);
                (Some(recv), v)
            }
        };
        let v = self.coerce_vty(v, ty);
        self.op(OpCode::Assign, [Operand::Var(t), v.op.clone()]);
        self.release(&v);
        let recv = match recv {
            Some(r) => r,
            None => receiver(self),
        };
        self.op(OpCode::PropSet, [Operand::Var(name), recv.op.clone(), Operand::Var(t)]);
        self.release(&Val::temp(t, ty));
        self.release(&recv);
    }

    // === Conversions ===

    fn coerce_opt(&mut self, v: Val, to: Option<TypeRef>) -> Val {
        match to {
            Some(t) => self.coerce(v, t),
            None => v,
        }
    }

    fn coerce_vty(&mut self, v: Val, to: VTy) -> Val {
        match to {
            VTy::Value(t) => self.coerce(v, t),
            _ => v,
        }
    }

    /// Applies the implicit conversion of `v` to `to`.
    fn coerce(&mut self, v: Val, to: TypeRef) -> Val {
        let target = VTy::Value(to);
        match v.ty {
            VTy::Unknown => v,
            _ if v.ty.is(target) => v,
            // Arrays take `None` and arrays as they are; everything else,
            // child objects and `None` included, is cast.
            _ if to.array => v,
            _ => self.cast(v, target),
        }
    }

    /// Converts `v` to `to`, nothing when it already has that type.
    fn cast(&mut self, v: Val, to: VTy) -> Val {
        if v.ty.is(to) {
            return Val { ty: to, ..v };
        }
        self.cast_always(v, to)
    }

    /// `CAST`, even to the same type: what `x as T` compiles to.
    fn cast_always(&mut self, v: Val, to: VTy) -> Val {
        let d = self.dest(to, &[&v]);
        self.op(OpCode::Cast, [Operand::Var(d), v.op.clone()]);
        self.release_except(&v, d);
        Val::temp(d, to)
    }

    // === Expressions ===

    fn ty(&self, id: ExprId) -> VTy {
        self.types.types.get(id.0 as usize).map_or(VTy::Unknown, |&t| VTy::of(t))
    }

    fn expr(&mut self, id: ExprId) -> Val {
        let ty = self.ty(id);
        // The body outlives `self`: matching on it borrows nothing of `self`.
        let body = self.body;
        match body.expr(id) {
            Expr::Missing | Expr::Unresolved(_) => Val::NONE,
            &Expr::Literal(lit) => Val::constant(self.literal(id, lit), ty),
            &Expr::Local(local) => Val::var(self.local_name(local), ty),
            &Expr::Def(def) => self.def_value(def, ty),
            Expr::SelfRef | Expr::ParentRef => Val::var(*SELF, ty),
            &Expr::Field { receiver, def, .. } => self.field(receiver, def, ty),
            Expr::Call { receiver, def, args, .. } => self.call(*receiver, *def, args),
            &Expr::Index { base, index } => {
                let b = self.expr(base);
                let i = self.expr(index);
                let i = self.coerce(i, TypeRef::INT);
                let d = self.dest(ty, &[&i]);
                self.op(OpCode::ArrayGetElement, [Operand::Var(d), b.op.clone(), i.op.clone()]);
                self.release(&b);
                self.release_except(&i, d);
                Val::temp(d, ty)
            }
            &Expr::Cast { expr, ty: target } => {
                let v = self.expr(expr);
                match target {
                    Some(t) => self.cast_always(v, VTy::Value(t)),
                    None => v,
                }
            }
            &Expr::New { size, .. } => {
                let s = self.expr(size);
                let s = self.coerce(s, TypeRef::INT);
                let d = self.dest(ty, &[]);
                self.op(OpCode::ArrayCreate, [Operand::Var(d), s.op.clone()]);
                self.release(&s);
                Val::temp(d, ty)
            }
            &Expr::Unary { op, expr } => self.unary(op, expr, ty),
            &Expr::Binary { op: op @ (BinaryOp::And | BinaryOp::Or), lhs, rhs } => {
                self.logical(op, lhs, rhs)
            }
            &Expr::Binary { op, lhs, rhs } => {
                let l = self.expr(lhs);
                let r = self.expr(rhs);
                self.binary_vals(op, l, r)
            }
        }
    }

    fn literal(&self, id: ExprId, lit: Literal) -> Operand {
        let range = self.body.expr_range(id);
        let text = self.sema().text.get(usize::from(range.start())..usize::from(range.end()));
        match lit {
            Literal::None => Operand::None,
            _ => text.and_then(constant::parse).unwrap_or(Operand::None),
        }
    }

    fn def_value(&mut self, def: Def, ty: VTy) -> Val {
        match def {
            Def::Member { file, index } => {
                if let Some(var) = self.member_place(file, index) {
                    return Val::var(var, ty);
                }
                let m = self.sema().member(file, index);
                if m.kind != MemberKind::Property {
                    return Val::NONE;
                }
                let d = self.dest(ty, &[]);
                self.op(
                    OpCode::PropGet,
                    [Operand::Var(m.name), Operand::Var(*SELF), Operand::Var(d)],
                );
                Val::temp(d, ty)
            }
            Def::Script { file } => Val::var(self.script_name(file), ty),
            _ => Val::NONE,
        }
    }

    /// The name of the script of `file`.
    fn script_name(&self, file: FileId) -> Name {
        self.sema().ws.file(file).index.name.unwrap_or_else(|| Name::new(""))
    }

    fn field(&mut self, receiver: ExprId, def: Option<Def>, ty: VTy) -> Val {
        match def {
            Some(Def::Builtin(Builtin::Length, _)) => {
                let b = self.expr(receiver);
                let d = self.dest(VTy::INT, &[]);
                self.op(OpCode::ArrayLength, [Operand::Var(d), b.op.clone()]);
                self.release(&b);
                Val::temp(d, VTy::INT)
            }
            Some(Def::Member { file, index }) => {
                let m = self.sema().member(file, index);
                match m.kind {
                    MemberKind::Variable => Val::var(m.name, ty),
                    MemberKind::Property => {
                        let r = self.expr(receiver);
                        let d = self.dest(ty, &[]);
                        self.op(
                            OpCode::PropGet,
                            [Operand::Var(m.name), r.op.clone(), Operand::Var(d)],
                        );
                        self.release(&r);
                        Val::temp(d, ty)
                    }
                    _ => Val::NONE,
                }
            }
            _ => Val::NONE,
        }
    }

    fn unary(&mut self, op: UnaryOp, expr: ExprId, ty: VTy) -> Val {
        // Negative literals are constants.
        if op == UnaryOp::Neg
            && let Expr::Literal(lit) = *self.body.expr(expr)
        {
            match self.literal(expr, lit) {
                Operand::Int(i) => return Val::constant(Operand::Int(i.wrapping_neg()), ty),
                Operand::Float(f) => return Val::constant(Operand::Float(-f), ty),
                _ => {}
            }
        }
        let v = self.expr(expr);
        let (opcode, ty) = match op {
            UnaryOp::Not => (OpCode::Not, VTy::BOOL),
            UnaryOp::Neg if v.ty.is(VTy::FLOAT) => (OpCode::FNeg, VTy::FLOAT),
            UnaryOp::Neg => (OpCode::INeg, VTy::INT),
        };
        let d = self.dest(ty, &[&v]);
        self.op(opcode, [Operand::Var(d), v.op.clone()]);
        self.release_except(&v, d);
        Val::temp(d, ty)
    }

    fn binary_vals(&mut self, op: BinaryOp, l: Val, r: Val) -> Val {
        use BinaryOp::*;
        match op {
            Eq | Ne | Lt | Le | Gt | Ge => {
                let (l, r) = self.harmonize(l, r);
                let opcode = match op {
                    Eq | Ne => OpCode::CmpEq,
                    Lt => OpCode::CmpLt,
                    Le => OpCode::CmpLe,
                    Gt => OpCode::CmpGt,
                    _ => OpCode::CmpGe,
                };
                let d = self.arith(opcode, VTy::BOOL, l, r);
                // `a != b` is `!(a == b)`.
                if op == Ne
                    && let Operand::Var(n) = d.op
                {
                    self.op(OpCode::Not, [Operand::Var(n), Operand::Var(n)]);
                }
                d
            }
            Add if l.ty.is(VTy::STRING) || r.ty.is(VTy::STRING) => {
                let l = self.coerce(l, TypeRef::STRING);
                let r = self.coerce(r, TypeRef::STRING);
                self.arith(OpCode::StrCat, VTy::STRING, l, r)
            }
            _ => {
                let float = l.ty.is(VTy::FLOAT) || r.ty.is(VTy::FLOAT);
                let (ty, target) =
                    if float { (VTy::FLOAT, TypeRef::FLOAT) } else { (VTy::INT, TypeRef::INT) };
                let l = self.coerce(l, target);
                let r = self.coerce(r, target);
                let opcode = match (op, float) {
                    (Add, false) => OpCode::IAdd,
                    (Add, true) => OpCode::FAdd,
                    (Sub, false) => OpCode::ISub,
                    (Sub, true) => OpCode::FSub,
                    (Mul, false) => OpCode::IMul,
                    (Mul, true) => OpCode::FMul,
                    (Div, false) => OpCode::IDiv,
                    (Div, true) => OpCode::FDiv,
                    _ => OpCode::IMod,
                };
                self.arith(opcode, ty, l, r)
            }
        }
    }

    /// `opcode dest, l, r`, the result of type `ty`.
    fn arith(&mut self, opcode: OpCode, ty: VTy, l: Val, r: Val) -> Val {
        let d = self.dest(ty, &[&l, &r]);
        self.op(opcode, [Operand::Var(d), l.op.clone(), r.op.clone()]);
        self.release_except(&l, d);
        self.release_except(&r, d);
        Val::temp(d, ty)
    }

    /// Brings the operands of a comparison to the same type.
    fn harmonize(&mut self, l: Val, r: Val) -> (Val, Val) {
        if l.ty == VTy::Unknown || r.ty == VTy::Unknown || l.ty.is(r.ty) {
            return (l, r);
        }
        let rank = |t: VTy| match t {
            VTy::None => 0,
            t if t.is(VTy::STRING) => 4,
            t if t.is(VTy::FLOAT) => 3,
            t if t.is(VTy::BOOL) => 5,
            t if t.is(VTy::INT) => 2,
            _ => 1,
        };
        let (rl, rr) = (rank(l.ty), rank(r.ty));
        if rl == 1 && rr == 1 {
            // Two object types: the child is cast to its parent.
            let (VTy::Value(lt), VTy::Value(rt)) = (l.ty, r.ty) else { return (l, r) };
            return match (lt.object(), rt.object()) {
                (Some(a), Some(b)) if self.extends(b, a) => {
                    let r = self.cast(r, l.ty);
                    (l, r)
                }
                (Some(a), Some(b)) if self.extends(a, b) => {
                    let ty = r.ty;
                    (self.cast(l, ty), r)
                }
                _ => (l, r),
            };
        }
        if rl > rr {
            let ty = l.ty;
            let r = self.cast(r, ty);
            (l, r)
        } else {
            let ty = r.ty;
            let l = self.cast(l, ty);
            (l, r)
        }
    }

    /// Whether script `child` is `ancestor` or extends it.
    fn extends(&self, child: Name, ancestor: Name) -> bool {
        self.sema().chain(child).any(|(_, index)| index.name == Some(ancestor))
    }

    /// `l && r` / `l || r`: `CAST d, l; JMPF/JMPT d, end; CAST d, r; end:`.
    fn logical(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId) -> Val {
        let l = self.expr(lhs);
        let d = self.dest(VTy::BOOL, &[&l]);
        self.op(OpCode::Cast, [Operand::Var(d), l.op.clone()]);
        self.release_except(&l, d);
        let end = self.label();
        let jump = if op == BinaryOp::And { OpCode::JmpF } else { OpCode::JmpT };
        self.jump(jump, Some(Operand::Var(d)), end);
        let r = self.expr(rhs);
        self.op(OpCode::Cast, [Operand::Var(d), r.op.clone()]);
        self.release_except(&r, d);
        self.place(end);
        Val::temp(d, VTy::BOOL)
    }

    // === Calls ===

    fn call(&mut self, receiver: Option<ExprId>, def: Option<Def>, args: &[Arg]) -> Val {
        match def {
            Some(Def::Builtin(b @ (Builtin::GoToState | Builtin::GetState), _)) => {
                self.state_call(b, receiver, args)
            }
            Some(Def::Builtin(b @ (Builtin::Find | Builtin::RFind), elem)) => {
                let Some(receiver) = receiver else { return Val::NONE };
                self.find_call(b, elem, receiver, args)
            }
            Some(Def::Member { file, index }) => self.member_call(file, index, receiver, args),
            _ => {
                for a in args {
                    let v = self.expr(a.value);
                    self.release(&v);
                }
                Val::NONE
            }
        }
    }

    /// `GoToState(name)` and `GetState()`: methods of every script.
    fn state_call(&mut self, b: Builtin, receiver: Option<ExprId>, args: &[Arg]) -> Val {
        let recv = match receiver {
            Some(r) => self.expr(r),
            None => Val::self_ref(),
        };
        let go_to =
            [ParamSig { name: Name::new("asNewState"), ty: Some(TypeRef::STRING), default: None }];
        let (name, params, ret): (&str, &[ParamSig], VTy) = if b == Builtin::GoToState {
            ("GotoState", &go_to, VTy::None)
        } else {
            ("GetState", &[], VTy::STRING)
        };
        let args = self.args(params, args);
        let dest = self.call_dest(ret, &args);
        self.call_op(
            OpCode::CallMethod,
            [Operand::var(name), recv.op.clone(), dest.op.clone()],
            &args,
        );
        self.release_all(&args, &dest);
        self.release(&recv);
        dest
    }

    /// `array.Find(value, start)` and `array.RFind(value, start)`.
    fn find_call(
        &mut self,
        b: Builtin,
        elem: Option<TypeRef>,
        receiver: ExprId,
        args: &[Arg],
    ) -> Val {
        let arr = self.expr(receiver);
        let (opcode, start) = if b == Builtin::Find {
            (OpCode::ArrayFindElement, 0)
        } else {
            (OpCode::ArrayRFindElement, -1)
        };
        let params = [
            ParamSig { name: Name::new("akElement"), ty: elem, default: None },
            ParamSig {
                name: Name::new("aiStartIndex"),
                ty: Some(TypeRef::INT),
                default: Some(Operand::Int(start)),
            },
        ];
        let args = self.args(&params, args);
        let d = self.dest(VTy::INT, &[]);
        self.call_op(opcode, [arr.op.clone(), Operand::Var(d)], &args);
        let dest = Val::temp(d, VTy::INT);
        self.release_all(&args, &dest);
        self.release(&arr);
        dest
    }

    /// A call of a function of a script: `CALLSTATIC` for `Global`
    /// functions, `CALLPARENT` for `Parent.F()`, else `CALLMETHOD`.
    fn member_call(
        &mut self,
        file: FileId,
        index: u32,
        receiver: Option<ExprId>,
        args: &[Arg],
    ) -> Val {
        let m = self.sema().member(file, index);
        if !m.is_callable() {
            return Val::NONE;
        }
        let params: Vec<ParamSig> = m
            .params
            .iter()
            .map(|p| ParamSig {
                name: p.name,
                ty: p.ty,
                default: p
                    .default
                    .as_deref()
                    .and_then(constant::parse)
                    .map(|d| p.ty.and_then(|t| constant::convert(&d, t)).unwrap_or(d)),
            })
            .collect();
        let ret = match (m.kind, m.ty) {
            (MemberKind::Function, Some(t)) => VTy::Value(t),
            _ => VTy::None,
        };
        let name = Operand::Var(m.name);
        let parent_call = receiver.is_some_and(|r| matches!(self.body.expr(r), Expr::ParentRef));
        if m.is_global() {
            let script = Operand::Var(self.script_name(file));
            let args = self.args(&params, args);
            let dest = self.call_dest(ret, &args);
            self.call_op(OpCode::CallStatic, [script, name, dest.op.clone()], &args);
            self.release_all(&args, &dest);
            dest
        } else if parent_call {
            let args = self.args(&params, args);
            let dest = self.call_dest(ret, &args);
            self.call_op(OpCode::CallParent, [name, dest.op.clone()], &args);
            self.release_all(&args, &dest);
            dest
        } else {
            let recv = match receiver {
                Some(r) => self.expr(r),
                None => Val::self_ref(),
            };
            let args = self.args(&params, args);
            let dest = self.call_dest(ret, &args);
            self.call_op(OpCode::CallMethod, [name, recv.op.clone(), dest.op.clone()], &args);
            self.release_all(&args, &dest);
            self.release(&recv);
            dest
        }
    }

    /// Where a call writes its result: a temporary, or `::NoneVar` for
    /// functions without result.
    fn call_dest(&mut self, ret: VTy, args: &[Val]) -> Val {
        match ret {
            VTy::Value(_) => {
                let d = self.dest_among(ret, args.iter());
                Val::temp(d, ret)
            }
            _ => Val::constant(self.none_var(), VTy::None),
        }
    }

    fn release_all(&mut self, args: &[Val], dest: &Val) {
        let keep = dest.op.as_var();
        for a in args {
            match keep {
                Some(k) if a.temp => self.release_except(a, k),
                _ => self.release(a),
            }
        }
    }

    /// Evaluates the arguments in source order and returns them in parameter
    /// order, defaults filled in and converted to the parameter types.
    fn args(&mut self, params: &[ParamSig], args: &[Arg]) -> Vec<Val> {
        let mut slots: Vec<Option<Val>> = vec![None; params.len()];
        let mut positional = 0;
        for a in args {
            let v = self.expr(a.value);
            let slot = match a.name {
                Some(n) => params.iter().position(|p| p.name == n),
                None => {
                    positional += 1;
                    Some(positional - 1)
                }
            };
            match slot.filter(|&i| i < params.len()) {
                Some(i) => slots[i] = Some(v),
                None => self.release(&v),
            }
        }
        let mut out = Vec::with_capacity(params.len());
        for (p, v) in params.iter().zip(slots) {
            let v = v.unwrap_or_else(|| {
                // Defaults are passed as they are, already of the parameter
                // type.
                let d = p.default.clone().unwrap_or(Operand::None);
                let ty = p.ty.map_or_else(|| VTy::of_const(&d), VTy::Value);
                Val::constant(d, ty)
            });
            // Converted after all arguments are evaluated, like the official
            // compiler does.
            out.push(self.coerce_opt(v, p.ty));
        }
        out
    }
}
