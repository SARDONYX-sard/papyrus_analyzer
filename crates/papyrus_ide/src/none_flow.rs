//! Whether object values may be `None`, along the statements of a body.
//!
//! Casts give `None` when they fail, and functions returning an object may
//! return `None`. A local holding such a value may be `None` until a
//! condition rules it out:
//!
//! ```papyrus
//! Actor a = akTarget as Actor     ; may be None
//! If a                            ; or `a != None`, `!a` + `Return`, `a && ...`
//!     a.Kill()                    ; not None here
//! EndIf
//! ```
//!
//! A local holding `Self` is followed too: casting it back to the script or to
//! a parent never fails, as in the code the Creation Kit generates:
//!
//! ```papyrus
//! Quest __temp = self as Quest
//! MyQuest kmyQuest = __temp as MyQuest    ; not None
//! ```
//!
//! [`analyze`] gives the state of every use of a local, for hovers, and the
//! receivers that may be `None`, for the lints `unchecked-cast` and
//! `unchecked-call-result`.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    TextRange,
    hir::{BinaryOp, Body, Expr, ExprId, Literal, LocalId, Stmt, StmtId, UnaryOp},
    index::{BaseType, MemberKind, TypeRef},
    sema::{Def, Sema},
    typeck::{Ty, TypeckResult},
};

/// Where a value that may be `None` comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoneSource {
    /// A cast that can fail (a downcast), at this range.
    Cast(TextRange),
    /// A call of a function returning an object, at this range.
    Call(TextRange),
}

/// What is known of a local at one of its uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nullness {
    /// It may be `None`.
    MaybeNone(NoneSource),
    /// A condition rules `None` out.
    NotNone,
    /// It holds `Self`, so it is not `None`.
    IsSelf,
}

/// A value that may be `None` used as a receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncheckedReceiver {
    /// The receiver.
    pub range: TextRange,
    /// The local it is, if any.
    pub local: Option<LocalId>,
    /// The accessed member: `Kill()`, `Name`.
    pub member: String,
    /// Where the value comes from.
    pub source: NoneSource,
}

/// The result of [`analyze`].
#[derive(Clone, Debug, Default)]
pub struct NoneFlow {
    /// The state of locals where they are used, for those with one.
    pub uses: FxHashMap<ExprId, Nullness>,
    /// The receivers that may be `None`; a local only at its first such use
    /// after each assignment.
    pub unchecked: Vec<UncheckedReceiver>,
}

/// Follows the values that may be `None` through `body`.
pub fn analyze(sema: &Sema<'_>, body: &Body, types: &TypeckResult) -> NoneFlow {
    let mut cx =
        Flow { sema, body, types, out: NoneFlow::default(), reported: FxHashSet::default() };
    cx.block(&body.root, State::default());
    cx.out
}

type State = FxHashMap<LocalId, Nullness>;

/// What a cast between scripts does, known from their `extends` chains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastKind {
    /// To the script of the value or one it extends: never fails.
    Upcast,
    /// To a script extending the one of the value: `None` unless the object
    /// is one.
    Downcast,
    /// Between scripts neither of which extends the other: always `None`,
    /// and a compile error (`invalid-cast`).
    Unrelated,
}

/// The kind of a cast of a `from` value to `to`; `None` when they are not
/// both scripts or when a script of their `extends` chains is unknown (the
/// imports of papyrus.toml provide them all, as for compiling).
pub fn cast_kind(sema: &Sema<'_>, from: TypeRef, to: TypeRef) -> Option<CastKind> {
    let (from, to) = (from.object()?, to.object()?);
    if !sema.chain_is_complete(from) || !sema.chain_is_complete(to) {
        return None;
    }
    let extends = |child, parent| sema.chain(child).any(|(_, i)| i.name == Some(parent));
    Some(if extends(from, to) {
        CastKind::Upcast
    } else if extends(to, from) {
        CastKind::Downcast
    } else {
        CastKind::Unrelated
    })
}

struct Flow<'a, 's> {
    sema: &'a Sema<'s>,
    body: &'a Body,
    types: &'a TypeckResult,
    out: NoneFlow,
    /// Locals reported since their last assignment.
    reported: FxHashSet<LocalId>,
}

/// `state` with `locals` known not to be `None`.
fn with_not_none(state: &State, locals: &FxHashSet<LocalId>) -> State {
    let mut out = state.clone();
    for &l in locals {
        if out.get(&l) != Some(&Nullness::IsSelf) {
            out.insert(l, Nullness::NotNone);
        }
    }
    out
}

/// The state after paths joining: may be `None` if it may on any path, not
/// `None` if not on every path.
fn merge(paths: Vec<State>) -> Option<State> {
    let mut paths = paths.into_iter();
    let mut out = paths.next()?;
    for path in paths {
        let keys: Vec<LocalId> = out.keys().chain(path.keys()).copied().collect();
        for l in keys {
            match (out.get(&l).copied(), path.get(&l).copied()) {
                (Some(Nullness::MaybeNone(_)), _) => {}
                (_, Some(n @ Nullness::MaybeNone(_))) => {
                    out.insert(l, n);
                }
                (Some(a), Some(b)) if a == b => {}
                // Not `None` on both paths, but `Self` on one only.
                (Some(_), Some(_)) => {
                    out.insert(l, Nullness::NotNone);
                }
                _ => {
                    out.remove(&l);
                }
            }
        }
    }
    Some(out)
}

impl Flow<'_, '_> {
    /// Follows `stmts` from `state`; returns the state at the end, `None`
    /// when the block does not fall through (it ends with `Return`).
    fn block(&mut self, stmts: &[StmtId], mut state: State) -> Option<State> {
        for &s in stmts {
            match self.body.stmt(s).clone() {
                Stmt::Local { local, init } => {
                    if let Some(init) = init {
                        self.expr(init, &state);
                    }
                    self.assign(local, init, &mut state);
                }
                Stmt::Assign { op, target, value } => {
                    self.expr(value, &state);
                    match (op, self.body.expr(target)) {
                        (None, &Expr::Local(local)) => self.assign(local, Some(value), &mut state),
                        _ => self.expr(target, &state),
                    }
                }
                Stmt::Expr(e) => self.expr(e, &state),
                Stmt::Return(e) => {
                    if let Some(e) = e {
                        self.expr(e, &state);
                    }
                    return None;
                }
                Stmt::If { branches, else_branch } => {
                    // The path taken when every condition so far was false.
                    let mut rest = state.clone();
                    let mut ends = Vec::new();
                    for (cond, stmts) in &branches {
                        self.expr(*cond, &rest);
                        let (when_true, when_false) = self.facts(*cond);
                        ends.push(self.block(stmts, with_not_none(&rest, &when_true)));
                        rest = with_not_none(&rest, &when_false);
                    }
                    ends.push(match &else_branch {
                        Some(stmts) => self.block(stmts, rest),
                        None => Some(rest),
                    });
                    state = merge(ends.into_iter().flatten().collect())?;
                }
                Stmt::While { condition, body } => {
                    // Assigned in the loop: not `Self` anymore from the second
                    // round on, which is not followed.
                    let mut assigned = FxHashSet::default();
                    self.assigned_locals(&body, &mut assigned);
                    state.retain(|l, n| *n != Nullness::IsSelf || !assigned.contains(l));
                    self.expr(condition, &state);
                    let (when_true, when_false) = self.facts(condition);
                    let end = self.block(&body, with_not_none(&state, &when_true));
                    let after = with_not_none(&state, &when_false);
                    state = match end {
                        Some(end) => merge(vec![after, with_not_none(&end, &when_false)])?,
                        None => after,
                    };
                }
            }
        }
        Some(state)
    }

    /// The locals `stmts` assign, in nested blocks too.
    fn assigned_locals(&self, stmts: &[StmtId], out: &mut FxHashSet<LocalId>) {
        for &s in stmts {
            match self.body.stmt(s) {
                Stmt::Assign { target, .. } => {
                    if let Expr::Local(l) = *self.body.expr(*target) {
                        out.insert(l);
                    }
                }
                Stmt::If { branches, else_branch } => {
                    for (_, stmts) in branches {
                        self.assigned_locals(stmts, out);
                    }
                    if let Some(stmts) = else_branch {
                        self.assigned_locals(stmts, out);
                    }
                }
                Stmt::While { body, .. } => self.assigned_locals(body, out),
                Stmt::Local { .. } | Stmt::Expr(_) | Stmt::Return(_) => {}
            }
        }
    }

    fn assign(&mut self, local: LocalId, value: Option<ExprId>, state: &mut State) {
        self.reported.remove(&local);
        match value.and_then(|v| self.nullness(v, state)) {
            Some(n) => state.insert(local, n),
            None => state.remove(&local),
        };
    }

    /// Records the uses of locals in `e` and the receivers that may be `None`.
    fn expr(&mut self, e: ExprId, state: &State) {
        match self.body.expr(e).clone() {
            Expr::Local(l) => {
                if let Some(&n) = state.get(&l) {
                    self.out.uses.insert(e, n);
                }
            }
            Expr::Field { receiver, name, .. } => {
                self.expr(receiver, state);
                self.receiver(receiver, name.to_string(), state);
            }
            Expr::Call { receiver, name, args, .. } => {
                if let Some(r) = receiver {
                    self.expr(r, state);
                }
                for a in &args {
                    self.expr(a.value, state);
                }
                if let Some(r) = receiver {
                    self.receiver(r, format!("{name}()"), state);
                }
            }
            Expr::Binary { op: BinaryOp::And, lhs, rhs } => {
                self.expr(lhs, state);
                self.expr(rhs, &with_not_none(state, &self.facts(lhs).0));
            }
            Expr::Binary { op: BinaryOp::Or, lhs, rhs } => {
                self.expr(lhs, state);
                self.expr(rhs, &with_not_none(state, &self.facts(lhs).1));
            }
            Expr::Binary { lhs, rhs, .. } | Expr::Index { base: lhs, index: rhs } => {
                self.expr(lhs, state);
                self.expr(rhs, state);
            }
            Expr::Unary { expr, .. } | Expr::Cast { expr, .. } | Expr::New { size: expr, .. } => {
                self.expr(expr, state)
            }
            Expr::Missing
            | Expr::Literal(_)
            | Expr::Def(_)
            | Expr::Unresolved(_)
            | Expr::SelfRef
            | Expr::ParentRef => {}
        }
    }

    /// `receiver` is used to access `member`.
    fn receiver(&mut self, receiver: ExprId, member: String, state: &State) {
        let local = match *self.body.expr(receiver) {
            Expr::Local(l) => Some(l),
            _ => None,
        };
        let Some(Nullness::MaybeNone(source)) = self.nullness(receiver, state) else { return };
        if let Some(l) = local
            && !self.reported.insert(l)
        {
            return;
        }
        let range = self.body.expr_range(receiver);
        self.out.unchecked.push(UncheckedReceiver { range, local, member, source });
    }

    /// What is known of the value of `e`.
    fn nullness(&self, e: ExprId, state: &State) -> Option<Nullness> {
        let range = self.body.expr_range(e);
        match *self.body.expr(e) {
            Expr::Local(local) => state.get(&local).copied(),
            Expr::SelfRef => Some(Nullness::IsSelf),
            Expr::Cast { expr, ty: Some(ty) } => {
                if self.nullness(expr, state) == Some(Nullness::IsSelf) && self.is_own_type(ty) {
                    return Some(Nullness::IsSelf);
                }
                let from = self.ty(expr)?;
                match cast_kind(self.sema, from, ty)? {
                    CastKind::Upcast => None,
                    CastKind::Downcast => Some(Nullness::MaybeNone(NoneSource::Cast(range))),
                    // A compile error (invalid-cast), reported as such.
                    CastKind::Unrelated => None,
                }
            }
            Expr::Call { def: Some(Def::Member { file, index }), .. } => {
                let m = self.sema.member(file, index);
                let object =
                    m.kind == MemberKind::Function && m.ty.and_then(TypeRef::object).is_some();
                object.then_some(Nullness::MaybeNone(NoneSource::Call(range)))
            }
            _ => None,
        }
    }

    /// Whether `Self` is always a `ty`: the script or one it extends.
    fn is_own_type(&self, ty: TypeRef) -> bool {
        let script = self.sema.script_name().map(|s| TypeRef::scalar(BaseType::Object(s)));
        script.is_some_and(|s| cast_kind(self.sema, s, ty) == Some(CastKind::Upcast))
    }

    fn ty(&self, e: ExprId) -> Option<TypeRef> {
        match self.types.types.get(e.0 as usize) {
            Some(Ty::Value(t)) => Some(*t),
            _ => None,
        }
    }

    /// The locals not `None` when `cond` is true, and when it is false.
    fn facts(&self, cond: ExprId) -> (FxHashSet<LocalId>, FxHashSet<LocalId>) {
        let one = |l| [l].into_iter().collect::<FxHashSet<_>>();
        let is_none = |e: ExprId| matches!(self.body.expr(e), Expr::Literal(Literal::None));
        match *self.body.expr(cond) {
            Expr::Local(l) => (one(l), FxHashSet::default()),
            Expr::Cast { expr, .. } => self.facts(expr),
            Expr::Unary { op: UnaryOp::Not, expr } => {
                let (t, f) = self.facts(expr);
                (f, t)
            }
            Expr::Binary { op: op @ (BinaryOp::Eq | BinaryOp::Ne), lhs, rhs } => {
                let local = match (self.body.expr(lhs), self.body.expr(rhs)) {
                    (&Expr::Local(l), _) if is_none(rhs) => l,
                    (_, &Expr::Local(l)) if is_none(lhs) => l,
                    _ => return Default::default(),
                };
                if op == BinaryOp::Ne {
                    (one(local), FxHashSet::default())
                } else {
                    (FxHashSet::default(), one(local))
                }
            }
            Expr::Binary { op: BinaryOp::And, lhs, rhs } => {
                let (mut t, _) = self.facts(lhs);
                t.extend(self.facts(rhs).0);
                (t, FxHashSet::default())
            }
            Expr::Binary { op: BinaryOp::Or, lhs, rhs } => {
                let (_, mut f) = self.facts(lhs);
                f.extend(self.facts(rhs).1);
                (FxHashSet::default(), f)
            }
            _ => Default::default(),
        }
    }
}
