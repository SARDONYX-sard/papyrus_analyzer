//! Checks a value that may be `None` (see [`crate::none_flow`]) before the
//! statement using it, which then only runs when it is not.
//!
//! ```papyrus
//! Actor a = akTarget as Actor
//! a.Kill()                 ; → If a
//!                          ;       a.Kill()
//!                          ;   EndIf
//! ```
//!
//! A value used directly gets a variable first, like the binding quick fixes
//! of rust-analyzer:
//!
//! ```papyrus
//! (akTarget as Actor).Kill()   ; → Actor akActor = akTarget as Actor
//!                              ;   If akActor
//!                              ;       akActor.Kill()
//!                              ;   EndIf
//! ```
//!
//! The quick fix of `unchecked-cast` and `unchecked-call-result`; available
//! with the lints allowed too, where the hover shows the value may be `None`.

use papyrus_parser::{SyntaxKind, SyntaxNode, TextRange, TextSize};

use super::{Assist, AssistCtx, AssistKind, TextEdit};
use crate::{
    hir::{Body, Expr, ExprId},
    none_flow::{self, NoneFlow, NoneSource, Nullness},
    typeck::{self, Ty},
};

pub(crate) fn check_none(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let offset = ctx.range.start();
    let Some(func) = ctx
        .sema
        .root
        .covering_node(ctx.range)
        .ancestors()
        .find(|n| matches!(n.kind(), SyntaxKind::FUNCTION | SyntaxKind::EVENT))
    else {
        return;
    };
    let Some(body) = Body::lower(ctx.sema, func) else { return };
    let types = typeck::check_body(ctx.sema, &body);
    let flow = none_flow::analyze(ctx.sema, &body, &types);
    if let Some(assist) = check_local(ctx, &body, &flow, offset) {
        acc.push(assist);
    } else if let Some(assist) = bind(ctx, &body, &types, &flow, offset) {
        acc.push(assist);
    }
}

fn lint_code(source: NoneSource) -> &'static str {
    match source {
        NoneSource::Cast(_) => "unchecked-cast",
        NoneSource::Call(_) => "unchecked-call-result",
    }
}

/// A local at the cursor that may be `None`: its statement goes in an `If`.
fn check_local(
    ctx: &AssistCtx<'_>,
    body: &Body,
    flow: &NoneFlow,
    offset: TextSize,
) -> Option<Assist> {
    // The identifier at the cursor, also when the cursor is right after it.
    let tok = ctx
        .sema
        .root
        .token_at_offset(offset)
        .max_by_key(|t| t.kind() == SyntaxKind::IDENT)
        .filter(|t| t.kind() == SyntaxKind::IDENT)?;
    let range = tok.text_range();
    let i = body.exprs.iter().position(|(e, r)| matches!(e, Expr::Local(_)) && *r == range)?;
    let Some(Nullness::MaybeNone(source)) = flow.uses.get(&ExprId(i as u32)) else { return None };
    let stmt = statement(tok.parent()?)?;
    let name = &ctx.sema.text[range];
    let stmt_text = &ctx.sema.text[stmt.trimmed_range()];
    Some(Assist {
        id: "check_none",
        label: format!("Check `{name}` is not None"),
        kind: AssistKind::QuickFix,
        target: range,
        edits: vec![wrap(ctx, stmt, None, name, stmt_text)],
        fixes: Some(lint_code(*source)),
        other_files: Vec::new(),
    })
}

/// A cast or call used directly as a receiver at the cursor: it goes in a
/// new variable, checked before the statement.
fn bind(
    ctx: &AssistCtx<'_>,
    body: &Body,
    types: &typeck::TypeckResult,
    flow: &NoneFlow,
    offset: TextSize,
) -> Option<Assist> {
    // The cursor on the value, or on the member it accesses.
    let at_cursor = |range: TextRange| {
        let access = ctx.sema.root.covering_node(range).ancestors().find(|n| {
            matches!(n.kind(), SyntaxKind::FIELD_EXPR | SyntaxKind::CALL_EXPR)
                && n.text_range() != range
        });
        range.contains_inclusive(offset)
            || access.is_some_and(|a| a.text_range().contains_inclusive(offset))
    };
    let u = flow
        .unchecked
        .iter()
        .filter(|u| u.local.is_none() && at_cursor(u.range))
        .min_by_key(|u| u.range.len())?;
    let i = body.exprs.iter().position(|(_, r)| *r == u.range)?;
    let Some(Ty::Value(ty)) = types.types.get(i).copied() else { return None };
    let text = ctx.sema.text;
    let value = &text[u.range];
    // `(x as Actor).Kill()`: the parentheses go with the value.
    let mut replaced = u.range;
    let (start, end) = (usize::from(u.range.start()), usize::from(u.range.end()));
    if text[..start].ends_with('(') && text[end..].starts_with(')') {
        replaced = TextRange::new(
            replaced.start() - TextSize::of('('),
            replaced.end() + TextSize::of(')'),
        );
    }
    let stmt = statement(ctx.sema.root.covering_node(u.range))?;
    let stmt_range = stmt.trimmed_range();
    let name = fresh_name(body, &format!("ak{ty}"));
    let rel = |o: TextSize| usize::from(o - stmt_range.start());
    let mut stmt_text = text[stmt_range].to_owned();
    stmt_text.replace_range(rel(replaced.start())..rel(replaced.end()), &name);
    let decl = format!("{ty} {name} = {value}");
    Some(Assist {
        id: "check_none",
        label: format!("Check `{value}` is not None, in `{name}`"),
        kind: AssistKind::QuickFix,
        target: u.range,
        edits: vec![wrap(ctx, stmt, Some(&decl), &name, &stmt_text)],
        fixes: Some(lint_code(u.source)),
        other_files: Vec::new(),
    })
}

/// The statement containing `node`, directly in a block.
fn statement(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    std::iter::once(node)
        .chain(node.ancestors())
        .find(|n| n.parent().is_some_and(|p| p.kind() == SyntaxKind::BLOCK))
}

/// `name`, or with a number when a local already has it.
fn fresh_name(body: &Body, name: &str) -> String {
    let taken = |n: &str| body.locals.iter().any(|l| l.name.as_str().eq_ignore_ascii_case(n));
    if !taken(name) {
        return name.to_owned();
    }
    (2..).map(|i| format!("{name}{i}")).find(|n| !taken(n)).expect("a free name")
}

/// Replaces `stmt` by `decl` (if any), then `If name`, `stmt_text` one level
/// deeper and `EndIf`.
fn wrap(
    ctx: &AssistCtx<'_>,
    stmt: SyntaxNode<'_>,
    decl: Option<&str>,
    name: &str,
    stmt_text: &str,
) -> TextEdit {
    let range = stmt.trimmed_range();
    let text = ctx.sema.text;
    let line_start = text[..usize::from(range.start())].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..usize::from(range.start())];
    let indent = &indent[..indent.len() - indent.trim_start().len()];
    let unit = if indent.starts_with('\t') { "\t" } else { "    " };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let inner = stmt_text.replace(newline, &format!("{newline}{unit}"));
    let decl = decl.map(|d| format!("{d}{newline}{indent}")).unwrap_or_default();
    TextEdit {
        range,
        new_text: format!("{decl}If {name}{newline}{indent}{unit}{inner}{newline}{indent}EndIf"),
    }
}

#[cfg(test)]
mod tests {
    use super::check_none;
    use crate::assists::test_utils::{check_assist, check_assist_not_applicable};

    const HEAD: &str = "ScriptName T\nT Function G()\nEndFunction\nFunction H()\nEndFunction\n";

    #[test]
    fn wraps_the_statement() {
        check_assist(
            check_none,
            &format!("{HEAD}Function F()\n    T x = G()\n    x$0.H()\nEndFunction\n"),
            &format!(
                "{HEAD}Function F()\n    T x = G()\n    If x\n        x.H()\n    EndIf\nEndFunction\n"
            ),
        );
    }

    #[test]
    fn binds_a_value_used_directly() {
        check_assist(
            check_none,
            &format!("{HEAD}Function F()\n    Int akT = 1\n    G$0().H()\nEndFunction\n"),
            &format!(
                "{HEAD}Function F()\n    Int akT = 1\n    T akT2 = G()\n    If akT2\n        akT2.H()\n    EndIf\nEndFunction\n"
            ),
        );
    }

    #[test]
    fn not_when_checked() {
        check_assist_not_applicable(
            check_none,
            &format!(
                "{HEAD}Function F()\n    T x = G()\n    If x\n        x$0.H()\n    EndIf\nEndFunction\n"
            ),
        );
    }
}
