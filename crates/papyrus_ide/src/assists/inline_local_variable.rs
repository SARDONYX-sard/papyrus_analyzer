//! Inlines a local variable whose value never changes: every use gets the
//! initial value and the declaration goes away.
//!
//! ```papyrus
//! Int limit = max - 1      ; removed
//! While i < limit          ; → While i < max - 1
//! ```
//!
//! Only when inlining cannot change what the code does: the variable is
//! never assigned again, the initial value is pure (literals, and variables
//! and parameters never assigned, combined with operators and casts; no
//! calls, properties or array elements, which may change), and no use comes
//! before the declaration (locals are hoisted: such a use reads the default
//! value).

use papyrus_parser::{
    SyntaxKind, SyntaxNode, TextRange,
    ast::{self, AstNode},
};

use super::{Assist, AssistCtx, AssistKind, TextEdit, line_range};
use crate::{
    features::decl_def,
    hir::{Body, Expr, ExprId, LocalId, Stmt},
    sema::{Def, LocalKind},
};

pub(crate) fn inline_local_variable(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(decl_range) = target(ctx) else { return };
    let Some(func) = enclosing_body(ctx, decl_range) else { return };
    let Some(body) = Body::lower(ctx.sema, func) else { return };
    let Some(local) =
        body.locals.iter().position(|l| l.range == decl_range).map(|i| LocalId(i as u32))
    else {
        return;
    };
    let assigned = assigned_locals(&body);
    if assigned.contains(&local) {
        return;
    }
    let Some(init) = body.stmts.iter().find_map(|(s, _)| match s {
        Stmt::Local { local: l, init: Some(e) } if *l == local => Some(*e),
        _ => None,
    }) else {
        return;
    };
    if !pure(&body, init, &assigned, decl_range) {
        return;
    }
    // Every use, all after the declaration.
    let uses = uses_of(ctx, func, decl_range);
    if uses.is_empty() || uses.iter().any(|u| u.text_range().start() < decl_range.start()) {
        return;
    }
    let Some(var) =
        ctx.sema.root.covering_node(decl_range).ancestors().find_map(ast::VarDecl::cast)
    else {
        return;
    };
    let init_text = ctx.text(var.initializer().expect("has an initializer").syntax());
    let compound = !matches!(body.expr(init), Expr::Literal(_) | Expr::Local(_));
    let mut edits: Vec<TextEdit> = uses
        .iter()
        .map(|u| TextEdit { range: u.text_range(), new_text: wrapped(init_text, compound, *u) })
        .collect();
    edits.push(TextEdit {
        range: line_range(ctx.sema.text, var.syntax().trimmed_range()),
        new_text: String::new(),
    });
    let name = &ctx.sema.text[decl_range];
    acc.push(Assist {
        id: "inline_local_variable",
        label: format!("Inline `{name}`"),
        kind: AssistKind::RefactorRewrite,
        target: decl_range,
        edits,
        fixes: None,
        other_files: Vec::new(),
    });
}

/// The name range of the local at the cursor: its declaration or a use.
fn target(ctx: &AssistCtx<'_>) -> Option<TextRange> {
    let tok = ctx.token().filter(|t| t.kind() == SyntaxKind::IDENT)?;
    let parent = tok.parent()?;
    let def = match parent.kind() {
        SyntaxKind::NAME => decl_def(ctx.sema, parent)?,
        SyntaxKind::NAME_REF => ctx.sema.resolve_name_ref(ast::NameRef::cast(parent)?)?,
        _ => return None,
    };
    match def {
        Def::Local { range, kind: LocalKind::Variable, .. } => Some(range),
        _ => None,
    }
}

pub(crate) fn enclosing_body<'a>(ctx: &AssistCtx<'a>, range: TextRange) -> Option<SyntaxNode<'a>> {
    ctx.sema
        .root
        .covering_node(range)
        .ancestors()
        .find(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))
}

/// The locals written by an assignment (`=` or compound).
fn assigned_locals(body: &Body) -> Vec<LocalId> {
    body.stmts
        .iter()
        .filter_map(|(s, _)| match s {
            Stmt::Assign { target, .. } => match body.expr(*target) {
                Expr::Local(l) => Some(*l),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Whether the value of `e` is the same wherever the variable is used.
fn pure(body: &Body, e: ExprId, assigned: &[LocalId], decl: TextRange) -> bool {
    match body.expr(e) {
        Expr::Literal(_) => true,
        // Parameters and locals declared before, never assigned.
        Expr::Local(l) => {
            let data = body.local(*l);
            !assigned.contains(l) && (data.is_param || data.range.start() < decl.start())
        }
        Expr::Unary { expr, .. } | Expr::Cast { expr, .. } => pure(body, *expr, assigned, decl),
        Expr::Binary { lhs, rhs, .. } => {
            pure(body, *lhs, assigned, decl) && pure(body, *rhs, assigned, decl)
        }
        _ => false,
    }
}

/// The `NAME_REF`s of `func` referring to the local declared at `decl`.
fn uses_of<'a>(ctx: &AssistCtx<'a>, func: SyntaxNode<'a>, decl: TextRange) -> Vec<SyntaxNode<'a>> {
    func.descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME_REF)
        .filter(|n| {
            ast::NameRef::cast(*n)
                .and_then(|nr| ctx.sema.resolve_name_ref(nr))
                .is_some_and(|d| matches!(d, Def::Local { range, .. } if range == decl))
        })
        .collect()
}

/// `value` at the place of `name_ref`, in parentheses when an operator
/// around would bind it differently.
pub(crate) fn wrapped(value: &str, compound: bool, name_ref: SyntaxNode<'_>) -> String {
    let tight = name_ref.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            SyntaxKind::BIN_EXPR
                | SyntaxKind::PREFIX_EXPR
                | SyntaxKind::CAST_EXPR
                | SyntaxKind::FIELD_EXPR
                | SyntaxKind::INDEX_EXPR
        )
    });
    if compound && tight { format!("({value})") } else { value.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::inline_local_variable;
    use crate::assists::test_utils::{check_assist, check_assist_not_applicable};

    fn wrap(body: &str) -> String {
        format!("ScriptName T\nInt Function F(Int max, Int k)\n{body}EndFunction\n")
    }

    #[test]
    fn inlines_a_constant() {
        check_assist(
            inline_local_variable,
            &wrap("    Int $0limit = 10\n    Return k + limit\n"),
            &wrap("    Return k + 10\n"),
        );
    }

    #[test]
    fn inlines_from_a_use_with_parentheses() {
        check_assist(
            inline_local_variable,
            &wrap("    Int limit = max - 1\n    Return k * lim$0it\n"),
            &wrap("    Return k * (max - 1)\n"),
        );
    }

    #[test]
    fn inlines_a_copy() {
        check_assist(
            inline_local_variable,
            &wrap("    Int $0copy = max\n    k = copy + copy\n    Return k\n"),
            &wrap("    k = max + max\n    Return k\n"),
        );
    }

    #[test]
    fn not_when_assigned_again() {
        check_assist_not_applicable(
            inline_local_variable,
            &wrap("    Int $0n = 1\n    n += 1\n    Return n\n"),
        );
    }

    #[test]
    fn not_when_the_source_changes() {
        check_assist_not_applicable(
            inline_local_variable,
            &wrap("    Int $0copy = max\n    max = 0\n    Return copy\n"),
        );
    }

    #[test]
    fn not_with_calls() {
        check_assist_not_applicable(
            inline_local_variable,
            &wrap("    Int $0n = F(1, 2)\n    Return n\n"),
        );
    }
}
