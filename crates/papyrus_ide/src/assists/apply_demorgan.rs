//! Applies De Morgan's law to the `&&` or `||` at the cursor.
//!
//! ```papyrus
//! If a && !b          ; → If !(!a || b)
//! If !(a || b < 2)    ; → If !a && b >= 2
//! ```
//!
//! A chain of the same operator is rewritten as a whole: `a && b && c`
//! becomes `!(!a || !b || !c)`.

use papyrus_parser::{
    SyntaxKind,
    ast::{self, AstNode},
};

use super::{Assist, AssistCtx, AssistKind, TextEdit, negate};

pub(crate) fn apply_demorgan(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    // The cursor must be on the operator.
    let Some(tok) = ctx.token() else { return };
    let Some(op) = [SyntaxKind::AMP2, SyntaxKind::PIPE2].into_iter().find(|&k| k == tok.kind())
    else {
        return;
    };
    let Some(mut bin) = tok.parent().and_then(ast::BinExpr::cast) else { return };
    // The top of the chain of `op`.
    while let Some(parent) = bin.syntax().parent().and_then(ast::BinExpr::cast)
        && parent.op().is_some_and(|t| t.kind() == op)
    {
        bin = parent;
    }
    let mut operands = Vec::new();
    collect(bin, op, &mut operands);
    let (new_op, label) = if op == SyntaxKind::AMP2 { ("||", "&&") } else { ("&&", "||") };
    let negated: Vec<String> = operands.iter().map(|&e| negate(ctx, e)).collect();
    let inner = negated.join(&format!(" {new_op} "));

    // `!(a && b)` loses its `!`; anything else gets one.
    let paren = bin.syntax().parent().and_then(ast::ParenExpr::cast);
    let not = paren
        .and_then(|p| p.syntax().parent())
        .and_then(ast::PrefixExpr::cast)
        .filter(|p| p.op().is_some_and(|t| t.kind() == SyntaxKind::BANG));
    let (replaced, new_text) = match not {
        Some(not) => {
            // `x && !(a || b)` must keep its meaning: `||` binds less than `&&`.
            let needs_parens = new_op == "||"
                && not
                    .syntax()
                    .parent()
                    .and_then(ast::BinExpr::cast)
                    .is_some_and(|p| p.op().is_some_and(|t| t.kind() != SyntaxKind::PIPE2));
            let text = if needs_parens { format!("({inner})") } else { inner };
            (not.syntax(), text)
        }
        None => (bin.syntax(), format!("!({inner})")),
    };
    acc.push(Assist {
        id: "apply_demorgan",
        label: format!("Apply De Morgan's law to `{label}`"),
        kind: AssistKind::RefactorRewrite,
        target: replaced.trimmed_range(),
        edits: vec![TextEdit { range: replaced.trimmed_range(), new_text }],
        fixes: None,
        other_files: Vec::new(),
    });
}

/// The operands of a chain of `op`, in order.
fn collect<'a>(bin: ast::BinExpr<'a>, op: SyntaxKind, out: &mut Vec<ast::Expr<'a>>) {
    for side in [bin.lhs(), bin.rhs()].into_iter().flatten() {
        match side {
            ast::Expr::Bin(b) if b.op().is_some_and(|t| t.kind() == op) => collect(b, op, out),
            e => out.push(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_demorgan;
    use crate::assists::test_utils::{check_assist, check_assist_not_applicable};

    fn wrap(cond: &str) -> String {
        format!(
            "ScriptName T\nFunction F(Bool a, Bool b, Int c)\n    If {cond}\n    EndIf\nEndFunction\n"
        )
    }

    #[test]
    fn and_becomes_negated_or() {
        check_assist(apply_demorgan, &wrap("a $0&& !b"), &wrap("!(!a || b)"));
    }

    #[test]
    fn negated_or_loses_its_not() {
        check_assist(apply_demorgan, &wrap("!(a |$0| c < 2)"), &wrap("!a && c >= 2"));
    }

    #[test]
    fn chains_are_rewritten_whole() {
        check_assist(apply_demorgan, &wrap("a && b $0&& c == 1"), &wrap("!(!a || !b || c != 1)"));
    }

    #[test]
    fn keeps_precedence() {
        check_assist(apply_demorgan, &wrap("b && !(a $0&& c > 1)"), &wrap("b && (!a || c <= 1)"));
    }

    #[test]
    fn needs_the_operator() {
        check_assist_not_applicable(apply_demorgan, &wrap("$0a && b"));
    }
}
