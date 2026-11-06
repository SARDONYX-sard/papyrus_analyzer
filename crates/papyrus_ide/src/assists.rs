//! Assists: code actions that edit the code, quick fixes of diagnostics
//! included.
//!
//! Like rust-analyzer, each assist lives in its own module below `assists/`
//! with its documentation (before / after) and its tests, and is listed in
//! [`HANDLERS`]. The fixes of lints live with their lint, in `papyrus_lint`.
//! To add one:
//!
//! 1. create `assists/my_assist.rs` with
//!    `pub(crate) fn my_assist(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>)`,
//!    which looks at the code around [`AssistCtx::range`] and pushes an
//!    [`Assist`] when it applies;
//! 2. add `mod my_assist;` and `my_assist::my_assist` to [`HANDLERS`];
//! 3. test it with [`test_utils::check_assist`] and
//!    [`test_utils::check_assist_not_applicable`].

mod apply_demorgan;
mod check_none;
mod fix_script_name;
mod inline_local_variable;
mod inline_script_variable;

use papyrus_parser::{
    SyntaxKind, SyntaxNode, TextRange, TextSize,
    ast::{self, AstNode},
    attrs::{fragments, touches_fragment},
};

use crate::{FileId, Workspace, features::with_sema, sema::Sema};

/// What an assist does, for the editor menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistKind {
    /// Fixes a diagnostic.
    QuickFix,
    /// Rewrites code without changing what it does.
    RefactorRewrite,
}

/// A replacement of a range of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// The replaced range.
    pub range: TextRange,
    /// The new text.
    pub new_text: String,
}

/// An applicable assist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assist {
    /// A stable identifier, the name of the module.
    pub id: &'static str,
    /// What the editor shows, e.g. `Apply De Morgan's law`.
    pub label: String,
    /// The kind.
    pub kind: AssistKind,
    /// The code the assist is about.
    pub target: TextRange,
    /// The edits, applied together.
    pub edits: Vec<TextEdit>,
    /// The code of the diagnostic it fixes, if any.
    pub fixes: Option<&'static str>,
}

/// What assists look at.
pub struct AssistCtx<'a> {
    /// The file.
    pub sema: &'a Sema<'a>,
    /// The selection, empty for a cursor.
    pub range: TextRange,
}

impl<'a> AssistCtx<'a> {
    /// The text of a node, without surrounding trivia.
    pub fn text(&self, node: SyntaxNode<'a>) -> &'a str {
        &self.sema.text[node.trimmed_range()]
    }

    /// The innermost node of type `N` covering the selection.
    pub fn find_node<N: AstNode<'a>>(&self) -> Option<N> {
        let start = self.sema.root.covering_node(self.range);
        std::iter::once(start).chain(start.ancestors()).find_map(N::cast)
    }

    /// The token at the cursor (the start of the selection), preferring the
    /// one on the right.
    pub fn token(&self) -> Option<papyrus_parser::SyntaxToken<'a>> {
        self.sema.root.token_at_offset(self.range.start()).right_biased()
    }
}

/// An assist handler: pushes the assists applicable at the selection.
pub type Handler = fn(&AssistCtx<'_>, &mut Vec<Assist>);

/// Every assist.
const HANDLERS: &[Handler] = &[
    fix_script_name::fix_script_name,
    apply_demorgan::apply_demorgan,
    check_none::check_none,
    inline_local_variable::inline_local_variable,
    inline_script_variable::inline_script_variable,
];

/// The assists applicable to `range` of `file`.
pub fn assists(ws: &Workspace, file: FileId, range: TextRange) -> Vec<Assist> {
    run_handlers(ws, file, range, HANDLERS)
}

/// Runs `handlers` at `range` of `file`; for assists of other crates. Assists
/// changing a fragment of the Creation Kit are left out, see
/// [`papyrus_parser::attrs::fragments`].
pub fn run_handlers(
    ws: &Workspace,
    file: FileId,
    range: TextRange,
    handlers: &[Handler],
) -> Vec<Assist> {
    with_sema(ws, file, |sema| {
        let ctx = AssistCtx { sema, range };
        let mut acc = Vec::new();
        for handler in handlers {
            handler(&ctx, &mut acc);
        }
        let fragments = fragments(sema.root);
        acc.retain(|a| !a.edits.iter().any(|e| touches_fragment(&fragments, e.range)));
        acc
    })
    .unwrap_or_default()
}

/// The negation of an expression, as source: `!x` for `x`, `x` for `!x`,
/// `a != b` for `a == b`, `!(a + b)` for compound expressions.
pub fn negate(ctx: &AssistCtx<'_>, expr: ast::Expr<'_>) -> String {
    match expr {
        ast::Expr::Prefix(p) if p.op().is_some_and(|t| t.kind() == SyntaxKind::BANG) => {
            match p.expr() {
                // `!(a && b)` → `a && b`: the parentheses only served the `!`.
                Some(ast::Expr::Paren(inner)) => {
                    inner.expr().map_or_else(String::new, |e| ctx.text(e.syntax()).to_owned())
                }
                Some(e) => ctx.text(e.syntax()).to_owned(),
                None => String::new(),
            }
        }
        ast::Expr::Bin(b) => {
            let flipped = b.op().and_then(|t| match t.kind() {
                SyntaxKind::EQ2 => Some("!="),
                SyntaxKind::NEQ => Some("=="),
                SyntaxKind::LT => Some(">="),
                SyntaxKind::LTEQ => Some(">"),
                SyntaxKind::GT => Some("<="),
                SyntaxKind::GTEQ => Some("<"),
                _ => None,
            });
            match (flipped, b.lhs(), b.rhs()) {
                (Some(op), Some(l), Some(r)) => {
                    format!("{} {op} {}", ctx.text(l.syntax()), ctx.text(r.syntax()))
                }
                _ => format!("!({})", ctx.text(expr.syntax())),
            }
        }
        // `!(a < b)` is `a >= b`.
        ast::Expr::Paren(p) => match p.expr() {
            Some(inner @ (ast::Expr::Bin(_) | ast::Expr::Prefix(_))) => {
                let n = negate(ctx, inner);
                if n.starts_with("!(") { format!("!{}", ctx.text(expr.syntax())) } else { n }
            }
            _ => format!("!{}", ctx.text(expr.syntax())),
        },
        ast::Expr::Cast(_) => format!("!({})", ctx.text(expr.syntax())),
        _ => format!("!{}", ctx.text(expr.syntax())),
    }
}

/// The whole lines of `range`, line break included, to delete statements.
pub fn line_range(text: &str, range: TextRange) -> TextRange {
    let start = text[..usize::from(range.start())].rfind('\n').map_or(0, |i| i + 1);
    let end = text[usize::from(range.end())..]
        .find('\n')
        .map_or(text.len(), |i| usize::from(range.end()) + i + 1);
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}

/// Helpers to test assists, here and in other crates.
#[doc(hidden)]
pub mod test_utils {
    use std::{path::PathBuf, sync::Arc};

    use papyrus_parser::{TextRange, TextSize};

    use super::{AssistCtx, Handler};
    use crate::{Workspace, features::with_sema};

    /// Runs `handler` with the cursor at `$0` (or the selection between two
    /// `$0`) of `before` in a file `path`, and applies the first assist.
    fn run(handler: Handler, path: &str, before: &str) -> Option<String> {
        let first = before.find("$0").expect("no $0 cursor");
        let text = before.replacen("$0", "", 1);
        let (text, range) = match text.find("$0") {
            Some(second) => (
                text.replacen("$0", "", 1),
                TextRange::new(TextSize::new(first as u32), TextSize::new(second as u32)),
            ),
            None => (text, TextRange::empty(TextSize::new(first as u32))),
        };
        let mut ws = Workspace::new();
        let file = ws.set_open(PathBuf::from(path), Arc::from(text.as_str()), 0);
        with_sema(&ws, file, |sema| {
            let mut acc = Vec::new();
            handler(&AssistCtx { sema, range }, &mut acc);
            let assist = acc.into_iter().next()?;
            let mut edits = assist.edits;
            edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
            let mut out = text.clone();
            for e in edits {
                out.replace_range(
                    usize::from(e.range.start())..usize::from(e.range.end()),
                    &e.new_text,
                );
            }
            Some(out)
        })
        .flatten()
    }

    /// Checks that the assist turns `before` into `after`.
    pub fn check_assist(handler: Handler, before: &str, after: &str) {
        check_assist_in(handler, "/ws/T.psc", before, after);
    }

    /// Like [`check_assist`], in a file at `path`.
    pub fn check_assist_in(handler: Handler, path: &str, before: &str, after: &str) {
        let result =
            run(handler, path, before).unwrap_or_else(|| panic!("not applicable:\n{before}"));
        assert_eq!(result, after);
    }

    /// Checks that the assist does not apply.
    pub fn check_assist_not_applicable(handler: Handler, before: &str) {
        assert_eq!(run(handler, "/ws/T.psc", before), None);
    }
}
