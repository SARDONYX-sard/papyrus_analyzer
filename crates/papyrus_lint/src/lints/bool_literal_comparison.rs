//! `bool-literal-comparison`: comparing with `True` or `False`.
//!
//! Fix: drops the comparison.
//!
//! ```papyrus
//! If done == True        ; → If done
//! If done != True        ; → If !done
//! If (a < b) == False    ; → If a >= b
//! ```

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit, negate},
    sema::Sema,
};
use papyrus_parser::{
    SyntaxKind,
    ast::{self, AstNode, LiteralKind},
};

use crate::{Group, Lint};

const CODE: &str = "bool-literal-comparison";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Complexity,
    summary: "a comparison with `True` or `False`",
    run,
    fix: Some(fix),
    manual_fix: false,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for node in sema.root.descendants() {
        let Some(bin) = ast::BinExpr::cast(node) else { continue };
        if bool_comparison(bin).is_some() {
            out.push(Diagnostic::warning(
                node.trimmed_range(),
                CODE,
                "comparing with a boolean literal is redundant",
            ));
        }
    }
}

/// For `x == True` and the like: the other operand, and whether the result
/// is `x` itself (`== True`, `!= False`) rather than its negation.
fn bool_comparison(bin: ast::BinExpr<'_>) -> Option<(ast::Expr<'_>, bool)> {
    let op = bin.op()?.kind();
    if !matches!(op, SyntaxKind::EQ2 | SyntaxKind::NEQ) {
        return None;
    }
    let literal = |e: Option<ast::Expr<'_>>| match e? {
        ast::Expr::Literal(l) if l.kind() == Some(LiteralKind::Bool) => {
            Some(l.token()?.kind() == SyntaxKind::TRUE_KW)
        }
        _ => None,
    };
    let (value, other) = match (literal(bin.lhs()), literal(bin.rhs())) {
        (_, Some(v)) => (v, bin.lhs()?),
        (Some(v), _) => (v, bin.rhs()?),
        _ => return None,
    };
    Some((other, value == (op == SyntaxKind::EQ2)))
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    // The innermost comparison with a literal around the cursor.
    let start = ctx.sema.root.covering_node(ctx.range);
    let Some((bin, (other, keep))) = std::iter::once(start)
        .chain(start.ancestors())
        .filter_map(ast::BinExpr::cast)
        .find_map(|b| Some((b, bool_comparison(b)?)))
    else {
        return;
    };
    let new_text = if keep { ctx.text(other.syntax()).to_owned() } else { negate(ctx, other) };
    let range = bin.syntax().trimmed_range();
    acc.push(Assist {
        id: "simplify_bool_comparison",
        label: format!("Simplify to `{new_text}`"),
        kind: AssistKind::QuickFix,
        target: range,
        edits: vec![TextEdit { range, new_text }],
        fixes: Some(CODE),
    });
}

#[cfg(test)]
mod tests {
    use papyrus_ide::assists::test_utils::{check_assist, check_assist_not_applicable};

    use super::fix;
    use crate::test_utils::check;

    fn wrap(cond: &str) -> String {
        format!("ScriptName T\nFunction F(Bool a, Int b)\n    If {cond}\n    EndIf\nEndFunction\n")
    }

    #[test]
    fn reports_comparisons_with_literals() {
        let src = "ScriptName T\nFunction F(Bool b)\n    If b == True\n    EndIf\n    If False != b\n    EndIf\n    If b == b\n    EndIf\nEndFunction\n";
        assert_eq!(
            check("bool-literal-comparison", src),
            [
                "3: comparing with a boolean literal is redundant",
                "5: comparing with a boolean literal is redundant"
            ]
        );
    }

    #[test]
    fn equal_true_is_the_value() {
        check_assist(fix, &wrap("a $0== True"), &wrap("a"));
    }

    #[test]
    fn not_true_is_the_negation() {
        check_assist(fix, &wrap("$0a != True"), &wrap("!a"));
    }

    #[test]
    fn negates_comparisons() {
        check_assist(fix, &wrap("False == (b $0< 2)"), &wrap("b >= 2"));
    }

    #[test]
    fn not_without_literal() {
        check_assist_not_applicable(fix, &wrap("a $0== a"));
    }
}
