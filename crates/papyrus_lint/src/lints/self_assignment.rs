//! `self-assignment`: a place assigned to itself, which does nothing.
//!
//! ```papyrus
//! Function SetCount(Int count)
//!     count = count   ; probably `Self.count = count` or another name
//! EndFunction
//! ```
//!
//! Places reached through a call are not compared, and properties with their
//! own functions neither: the call or the `Set` function may do something.
//!
//! Fix: removes the statement.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit, line_range},
    hir::{Body, Expr, ExprId, Literal, Stmt},
    index::{Flags, MemberKind},
    sema::{Def, Sema},
};
use papyrus_parser::SyntaxKind;

use crate::{Group, Lint, bodies};

const CODE: &str = "self-assignment";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Complexity,
    summary: "a variable assigned to itself",
    run,
    fix: Some(fix),
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for body in bodies(sema) {
        for (stmt, range) in &body.stmts {
            if is_self_assignment(sema, &body, stmt) {
                out.push(Diagnostic::warning(*range, CODE, "assigning a variable to itself"));
            }
        }
    }
}

fn is_self_assignment(sema: &Sema<'_>, body: &Body, stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Assign { op: None, target, value } if same_place(sema, body, *target, *value))
}

/// Whether two expressions name the same place, without anything running.
fn same_place(sema: &Sema<'_>, body: &Body, a: ExprId, b: ExprId) -> bool {
    match (body.expr(a), body.expr(b)) {
        (Expr::Local(a), Expr::Local(b)) => a == b,
        (Expr::SelfRef, Expr::SelfRef) => true,
        (Expr::Def(a), Expr::Def(b)) => a == b && is_plain(sema, a),
        (
            Expr::Field { receiver: ra, name: na, def: Some(da) },
            Expr::Field { receiver: rb, name: nb, def: Some(db) },
        ) => na == nb && da == db && is_plain(sema, da) && same_place(sema, body, *ra, *rb),
        (Expr::Index { base: ba, index: ia }, Expr::Index { base: bb, index: ib }) => {
            same_place(sema, body, *ba, *bb) && same_index(sema, body, *ia, *ib)
        }
        _ => false,
    }
}

fn same_index(sema: &Sema<'_>, body: &Body, a: ExprId, b: ExprId) -> bool {
    match (body.expr(a), body.expr(b)) {
        (Expr::Literal(Literal::Int), Expr::Literal(Literal::Int)) => {
            sema.text[body.expr_range(a)] == sema.text[body.expr_range(b)]
        }
        _ => same_place(sema, body, a, b),
    }
}

/// A script variable or an `Auto` property: no function runs to access it.
fn is_plain(sema: &Sema<'_>, def: &Def) -> bool {
    let Def::Member { file, index } = *def else { return false };
    let member = sema.member(file, index);
    member.kind == MemberKind::Variable
        || (member.kind == MemberKind::Property && member.flags.contains(Flags::AUTO))
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let start = ctx.sema.root.covering_node(ctx.range);
    let Some(func) = std::iter::once(start)
        .chain(start.ancestors())
        .find(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))
    else {
        return;
    };
    let Some(body) = Body::lower(ctx.sema, func) else { return };
    let found = body.stmts.iter().find(|(stmt, range)| {
        range.contains_range(ctx.range) && is_self_assignment(ctx.sema, &body, stmt)
    });
    let Some((_, range)) = found else { return };
    acc.push(Assist {
        id: "remove_self_assignment",
        label: "Remove the assignment".to_owned(),
        kind: AssistKind::QuickFix,
        target: *range,
        edits: vec![TextEdit { range: line_range(ctx.sema.text, *range), new_text: String::new() }],
        fixes: Some(CODE),
    });
}

#[cfg(test)]
mod tests {
    use papyrus_ide::assists::test_utils::{check_assist, check_assist_not_applicable};

    use super::fix;
    use crate::test_utils::check;

    #[test]
    fn reports_places_assigned_to_themselves() {
        let src = "ScriptName T\nInt v\nInt Property A Auto\nInt[] arr\nFunction F(Int p, T o)\n    p = p\n    v = v\n    A = A\n    Self.v = Self.v\n    o.A = o.A\n    arr[0] = arr[0]\n    arr[p] = arr[p]\nEndFunction\n";
        assert_eq!(
            check("self-assignment", src),
            [
                "6: assigning a variable to itself",
                "7: assigning a variable to itself",
                "8: assigning a variable to itself",
                "9: assigning a variable to itself",
                "10: assigning a variable to itself",
                "11: assigning a variable to itself",
                "12: assigning a variable to itself",
            ]
        );
    }

    #[test]
    fn other_places_and_effects_are_fine() {
        let src = "ScriptName T\nInt v\nInt Property Full\n    Int Function Get()\n        Return v\n    EndFunction\n    Function Set(Int x)\n        v = x\n    EndFunction\nEndProperty\nInt[] arr\nT Function G()\n    Return Self\nEndFunction\nFunction F(Int p, Int q, T o, T r)\n    p = q\n    p += p\n    Full = Full\n    o.v = r.v\n    arr[0] = arr[1]\n    arr[p] = arr[q]\n    G().v = G().v\nEndFunction\n";
        assert_eq!(check("self-assignment", src), [""; 0]);
    }

    #[test]
    fn fix_removes_the_line() {
        check_assist(
            fix,
            "ScriptName T\nFunction F(Int p)\n    p $0= p\n    Debug.Trace(p)\nEndFunction\n",
            "ScriptName T\nFunction F(Int p)\n    Debug.Trace(p)\nEndFunction\n",
        );
        check_assist_not_applicable(
            fix,
            "ScriptName T\nFunction F(Int p, Int q)\n    p $0= q\nEndFunction\n",
        );
    }
}
