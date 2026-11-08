//! `redundant-cast`: a cast to the type the value already has.
//!
//! ```papyrus
//! Function F(Actor akActor, Int count)
//!     Actor a = akActor as Actor   ; → Actor a = akActor
//!     Debug.Trace(count as Int)    ; → Debug.Trace(count)
//! EndFunction
//! ```
//!
//! Only the exact type counts. A cast to a parent (`akActor as ObjectReference`)
//! may be how the author picks a type, and is left alone.
//!
//! Fix: removes the cast, and the parentheses left around a simple value.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    hir::{Body, Expr},
    sema::Sema,
    typeck::{self, Ty},
};
use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextRange,
    ast::{self, AstNode},
};

use crate::{Group, Lint, bodies};

const CODE: &str = "redundant-cast";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Complexity,
    summary: "a cast to the type the value already has",
    run,
    fix: Some(fix),
    manual_fix: false,
};

/// The ranges of the redundant casts of `body`, with the type.
fn redundant(sema: &Sema<'_>, body: &Body) -> Vec<(TextRange, String)> {
    let types = typeck::check_body(sema, body);
    let mut out = Vec::new();
    for (expr, range) in &body.exprs {
        let Expr::Cast { expr: value, ty: Some(ty) } = expr else { continue };
        if let Some(Ty::Value(actual)) = types.types.get(value.0 as usize)
            && actual == ty
        {
            out.push((*range, ty.to_string()));
        }
    }
    out
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !sema.index.mentions(Name::new("as")) {
        return;
    }
    for body in bodies(sema) {
        for (range, ty) in redundant(sema, &body) {
            out.push(Diagnostic::warning(range, CODE, format!("the value is already `{ty}`")));
        }
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(cast) = ctx.find_node::<ast::CastExpr<'_>>() else { return };
    let Some(value) = cast.expr() else { return };
    let func = cast
        .syntax()
        .ancestors()
        .find(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT));
    let Some(body) = func.and_then(|f| Body::lower(ctx.sema, f)) else { return };
    let range = cast.syntax().trimmed_range();
    if !redundant(ctx.sema, &body).iter().any(|(r, _)| *r == range) {
        return;
    }
    // `(x as Actor).Kill()` → `x.Kill()`
    let simple = matches!(
        value.syntax().kind(),
        SyntaxKind::NAME_REF
            | SyntaxKind::LITERAL
            | SyntaxKind::CALL_EXPR
            | SyntaxKind::FIELD_EXPR
            | SyntaxKind::INDEX_EXPR
            | SyntaxKind::PAREN_EXPR
    );
    let parens = cast.syntax().parent().filter(|p| simple && p.kind() == SyntaxKind::PAREN_EXPR);
    let replaced = parens.map_or(range, |p| p.trimmed_range());
    acc.push(Assist {
        id: "remove_redundant_cast",
        label: "Remove the cast".to_owned(),
        kind: AssistKind::QuickFix,
        target: range,
        edits: vec![TextEdit { range: replaced, new_text: ctx.text(value.syntax()).to_owned() }],
        fixes: Some(CODE),
        other_files: Vec::new(),
    });
}

#[cfg(test)]
mod tests {
    use papyrus_ide::assists::test_utils::{check_assist, check_assist_not_applicable};

    use super::fix;
    use crate::test_utils::check;

    #[test]
    fn reports_casts_to_the_same_type() {
        let src = "ScriptName T\nFunction F(Int i, String s, T o, Int[] arr)\n    Debug.Trace(i as Int)\n    Debug.Trace(s as string)\n    T a = o as T\n    Debug.Trace(Self as T)\n    Int[] b = arr as Int[]\n    Debug.Trace(i as Float)\n    Debug.Trace(i as String)\n    Debug.Trace(None as T)\nEndFunction\n";
        assert_eq!(
            check("redundant-cast", src),
            [
                "3: the value is already `Int`",
                "4: the value is already `String`",
                "5: the value is already `T`",
                "6: the value is already `T`",
                "7: the value is already `Int[]`",
            ]
        );
    }

    #[test]
    fn casts_to_a_parent_are_left_alone() {
        let mut ws = papyrus_ide::Workspace::new();
        ws.set_open("/ws/P.psc".into(), "ScriptName P\n".into(), 0);
        let file = ws.set_open(
            "/ws/T.psc".into(),
            "ScriptName T extends P\nFunction F()\n    P a = Self as P\nEndFunction\n".into(),
            0,
        );
        let mut config = crate::LintConfig::default();
        config.set("all", crate::Level::Allow).unwrap();
        config.set("redundant-cast", crate::Level::Warn).unwrap();
        assert!(crate::lint(&ws, file, &config).is_empty());
    }

    #[test]
    fn fix_removes_the_cast() {
        check_assist(
            fix,
            "ScriptName T\nFunction F(Int i)\n    Int a = i $0as Int\nEndFunction\n",
            "ScriptName T\nFunction F(Int i)\n    Int a = i\nEndFunction\n",
        );
    }

    #[test]
    fn fix_removes_the_parentheses_around_a_simple_value() {
        check_assist(
            fix,
            "ScriptName T\nFunction G()\nEndFunction\nFunction F(T o)\n    (o $0as T).G()\nEndFunction\n",
            "ScriptName T\nFunction G()\nEndFunction\nFunction F(T o)\n    o.G()\nEndFunction\n",
        );
    }

    #[test]
    fn fix_keeps_one_pair_of_parentheses() {
        check_assist(
            fix,
            "ScriptName T\nFunction F(Int i)\n    Int a = 2 * ((i + 1) $0as Int)\nEndFunction\n",
            "ScriptName T\nFunction F(Int i)\n    Int a = 2 * (i + 1)\nEndFunction\n",
        );
    }

    #[test]
    fn no_fix_when_the_type_changes() {
        check_assist_not_applicable(
            fix,
            "ScriptName T\nFunction F(Int i)\n    Float a = i $0as Float\nEndFunction\n",
        );
    }
}
