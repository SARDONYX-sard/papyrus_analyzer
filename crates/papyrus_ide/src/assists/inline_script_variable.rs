//! Inlines a script variable used as a constant: never assigned by the
//! script, every use gets its initial value and the declaration goes away.
//!
//! ```papyrus
//! Int MaxRetries = 3                ; removed
//! ...
//! If retries < MaxRetries           ; → If retries < 3
//! ```
//!
//! Variables are private to their script, so this script is the only one
//! that could change it. Variables with user flags (`Conditional`) are left
//! alone: the game reads them.

use papyrus_parser::{
    SyntaxKind,
    ast::{self, AstNode},
};

use super::{Assist, AssistCtx, AssistKind, TextEdit, line_range};
use crate::{features::decl_def, index::MemberKind, sema::Def};

pub(crate) fn inline_script_variable(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(tok) = ctx.token().filter(|t| t.kind() == SyntaxKind::IDENT) else { return };
    let Some(parent) = tok.parent() else { return };
    let def = match parent.kind() {
        SyntaxKind::NAME => decl_def(ctx.sema, parent),
        SyntaxKind::NAME_REF => {
            ast::NameRef::cast(parent).and_then(|nr| ctx.sema.resolve_name_ref(nr))
        }
        _ => None,
    };
    let Some(def @ Def::Member { file, index }) = def else { return };
    let member = ctx.sema.member(file, index);
    if file != ctx.sema.file
        || member.kind != MemberKind::Variable
        || member.state.is_some()
        || !member.user_flags.is_empty()
    {
        return;
    }
    let Some(value) = member.default.as_deref() else { return };
    let Some(var) =
        ctx.sema.root.covering_node(member.name_range).ancestors().find_map(ast::VarDecl::cast)
    else {
        return;
    };

    let mut uses = Vec::new();
    for node in ctx.sema.root.descendants().filter(|n| n.kind() == SyntaxKind::NAME_REF) {
        let Some(nr) = ast::NameRef::cast(node) else { continue };
        if ctx.sema.resolve_name_ref(nr) != Some(def) {
            continue;
        }
        if is_assigned(node) {
            return;
        }
        uses.push(node);
    }
    if uses.is_empty() {
        return;
    }
    // A negative number next to an operator reads better in parentheses.
    let compound = value.starts_with('-');
    let mut edits: Vec<TextEdit> = uses
        .iter()
        .map(|u| TextEdit {
            range: u.text_range(),
            new_text: super::inline_local_variable::wrapped(value, compound, *u),
        })
        .collect();
    edits.push(TextEdit {
        range: line_range(ctx.sema.text, var.syntax().trimmed_range()),
        new_text: String::new(),
    });
    acc.push(Assist {
        id: "inline_script_variable",
        label: format!("Inline `{}` (= {value})", member.name),
        kind: AssistKind::RefactorRewrite,
        target: member.name_range,
        edits,
        fixes: None,
        other_files: Vec::new(),
    });
}

/// Whether the name is the target of an assignment (`=` or compound).
fn is_assigned(name_ref: papyrus_parser::SyntaxNode<'_>) -> bool {
    name_ref
        .parent()
        .and_then(ast::AssignStmt::cast)
        .and_then(|a| a.target())
        .is_some_and(|t| t.syntax() == name_ref)
}

#[cfg(test)]
mod tests {
    use super::inline_script_variable;
    use crate::assists::test_utils::{check_assist, check_assist_not_applicable};

    #[test]
    fn inlines_a_constant_variable() {
        check_assist(
            inline_script_variable,
            "ScriptName T\nInt $0MaxRetries = 3\nBool Function F(Int n)\n    Return n < MaxRetries\nEndFunction\nInt Function G()\n    Return MaxRetries * 2\nEndFunction\n",
            "ScriptName T\nBool Function F(Int n)\n    Return n < 3\nEndFunction\nInt Function G()\n    Return 3 * 2\nEndFunction\n",
        );
    }

    #[test]
    fn not_when_assigned() {
        check_assist_not_applicable(
            inline_script_variable,
            "ScriptName T\nInt $0count = 0\nFunction F()\n    count += 1\nEndFunction\n",
        );
    }

    #[test]
    fn not_with_user_flags() {
        check_assist_not_applicable(
            inline_script_variable,
            "ScriptName T\nBool $0done = False Conditional\nBool Function F()\n    Return done\nEndFunction\n",
        );
    }
}
