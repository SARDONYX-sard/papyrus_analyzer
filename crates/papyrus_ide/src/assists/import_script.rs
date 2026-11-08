//! `Import Script` and `Script.Function()`, one into the other.
//!
//! On `Import Lib`: the calls of its functions get `Lib.`, and the line goes.
//!
//! ```papyrus
//! Import Lib                ; →
//! Function F()
//!     Helper()              ;     Lib.Helper()
//! EndFunction
//! ```
//!
//! On `Lib.Helper()`: the other way, for every call of `Lib` that the bare name
//! would call: not when the script, or an import before, has the name too.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextRange, TextSize,
    ast::{self, AstNode},
};

use super::{Assist, AssistCtx, AssistKind, TextEdit, line_range};
use crate::{FileId, sema::Def};

/// On `Import Lib`: writes `Lib.` before the calls of its functions, and removes
/// the line.
pub(crate) fn inline_import(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(import) = ctx.find_node::<ast::Import<'_>>() else { return };
    let Some(name) = import.name_ref() else { return };
    let script = name.text();
    let sema = ctx.sema;
    let Some(file) = sema.ws.script_near(Name::new(script), sema.file) else { return };
    let mut edits: Vec<TextEdit> = sema
        .root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter_map(|call| match call.callee()? {
            ast::Expr::NameRef(callee) => Some(callee),
            _ => None,
        })
        .filter(|callee| matches!(sema.resolve_name_ref(*callee), Some(Def::Member { file: f, .. }) if f == file))
        .map(|callee| TextEdit {
            range: TextRange::empty(callee.syntax().text_range().start()),
            new_text: format!("{script}."),
        })
        .collect();
    let range = import.syntax().trimmed_range();
    edits.push(TextEdit { range: line_range(sema.text, range), new_text: String::new() });
    acc.push(Assist {
        id: "inline_import",
        label: format!("Call the functions of `{script}` as `{script}.Function()`"),
        kind: AssistKind::RefactorRewrite,
        target: range,
        edits,
        fixes: None,
        other_files: Vec::new(),
    });
}

/// The script `Lib` and the function of `Lib.Helper()`, a global one.
fn qualified_call<'a>(
    ctx: &AssistCtx<'a>,
    call: ast::CallExpr<'a>,
) -> Option<(FileId, ast::NameRef<'a>, ast::NameRef<'a>)> {
    let ast::Expr::Field(field) = call.callee()? else { return None };
    let ast::Expr::NameRef(receiver) = field.receiver()? else { return None };
    let Def::Script { file } = ctx.sema.resolve_name_ref(receiver)? else { return None };
    let function = field.name_ref()?;
    let Def::Member { file: declaring, index } = ctx.sema.resolve_name_ref(function)? else {
        return None;
    };
    (declaring == file && ctx.sema.member(declaring, index).is_global())
        .then_some((file, receiver, function))
}

/// On `Lib.Helper()`: adds `Import Lib`, and removes `Lib.` before the calls of
/// its functions.
pub(crate) fn add_import(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(call) = ctx.find_node::<ast::CallExpr<'_>>() else { return };
    let Some((file, receiver, _)) = qualified_call(ctx, call) else { return };
    let sema = ctx.sema;
    let script = receiver.text();
    if sema.index.imports.iter().any(|i| sema.ws.script_near(*i, sema.file) == Some(file)) {
        return;
    }
    let mut edits: Vec<TextEdit> = sema
        .root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter_map(|c| qualified_call(ctx, c))
        .filter(|&(f, _, function)| {
            // The bare name must not call something else: of the script, of
            // an import before.
            f == file && sema.resolve_function(Name::new(function.text())).is_none()
        })
        .map(|(_, receiver, function)| TextEdit {
            range: TextRange::new(
                receiver.syntax().text_range().start(),
                function.syntax().text_range().start(),
            ),
            new_text: String::new(),
        })
        .collect();
    let Some(at) = import_position(ctx) else { return };
    let newline = if sema.text.contains("\r\n") { "\r\n" } else { "\n" };
    edits.push(TextEdit {
        range: TextRange::empty(at),
        new_text: format!("{newline}Import {script}"),
    });
    acc.push(Assist {
        id: "add_import",
        label: format!("Import `{script}`, and call its functions without `{script}.`"),
        kind: AssistKind::RefactorRewrite,
        target: call.syntax().trimmed_range(),
        edits,
        fixes: None,
        other_files: Vec::new(),
    });
}

/// Where a new `Import` line goes: after the last one, else after the header
/// and its documentation.
fn import_position(ctx: &AssistCtx<'_>) -> Option<TextSize> {
    let sema = ctx.sema;
    let last_import =
        sema.root.descendants().filter_map(ast::Import::cast).map(|i| i.syntax().trimmed_range());
    let header = sema
        .root
        .children()
        .find(|n| n.kind() == SyntaxKind::SCRIPT_HEADER)
        .map(|h| h.trimmed_range());
    let docs = sema.index.docs.iter().copied();
    let anchor =
        last_import.last().or_else(|| header.into_iter().chain(docs).max_by_key(|r| r.end()))?;
    // The end of its line.
    let end = usize::from(anchor.end());
    let line_end = sema.text[end..].find(['\r', '\n']).map_or(sema.text.len(), |i| end + i);
    Some(TextSize::new(line_end as u32))
}

#[cfg(test)]
mod tests {
    use super::{add_import, inline_import};
    use crate::assists::test_utils::{check_assist_not_applicable_with, check_assist_with};

    const LIB: (&str, &str) = (
        "Lib",
        "ScriptName Lib\nFunction Helper() Global\nEndFunction\nInt Function Other(Int a) Global\n    Return a\nEndFunction\n",
    );

    #[test]
    fn inlines_an_import() {
        check_assist_with(
            inline_import,
            &[LIB],
            "ScriptName T\nImport $0Lib\nFunction F()\n    Helper()\n    Int x = Other(Other(1))\nEndFunction\n",
            "ScriptName T\nFunction F()\n    Lib.Helper()\n    Int x = Lib.Other(Lib.Other(1))\nEndFunction\n",
        );
    }

    #[test]
    fn imports_a_script() {
        check_assist_with(
            add_import,
            &[LIB],
            "ScriptName T\n{Docs.}\nFunction F()\n    Lib.Hel$0per()\n    Int x = Lib.Other(1)\nEndFunction\n",
            "ScriptName T\n{Docs.}\nImport Lib\nFunction F()\n    Helper()\n    Int x = Other(1)\nEndFunction\n",
        );
        // After the other imports; a name of the script stays qualified.
        check_assist_with(
            add_import,
            &[LIB, ("Debug", "ScriptName Debug\nFunction Trace(String s) Global\nEndFunction\n")],
            "ScriptName T\nImport Debug\nFunction Helper()\nEndFunction\nFunction F()\n    Lib.Ot$0her(1)\n    Lib.Helper()\nEndFunction\n",
            "ScriptName T\nImport Debug\nImport Lib\nFunction Helper()\nEndFunction\nFunction F()\n    Other(1)\n    Lib.Helper()\nEndFunction\n",
        );
    }

    #[test]
    fn round_trip() {
        let qualified = "ScriptName T\nFunction F()\n    Lib.Helper()\nEndFunction\n";
        check_assist_with(
            add_import,
            &[LIB],
            &qualified.replace("Lib.Helper", "Lib.Hel$0per"),
            "ScriptName T\nImport Lib\nFunction F()\n    Helper()\nEndFunction\n",
        );
        check_assist_with(
            inline_import,
            &[LIB],
            "ScriptName T\nImport $0Lib\nFunction F()\n    Helper()\nEndFunction\n",
            qualified,
        );
    }

    #[test]
    fn not_for_other_calls() {
        // Imported already; a method; a script unknown.
        check_assist_not_applicable_with(
            add_import,
            &[LIB],
            "ScriptName T\nImport Lib\nFunction F()\n    Lib.Hel$0per()\nEndFunction\n",
        );
        check_assist_not_applicable_with(
            add_import,
            &[LIB],
            "ScriptName T\nFunction M()\nEndFunction\nFunction F(T o)\n    o.$0M()\nEndFunction\n",
        );
        check_assist_not_applicable_with(inline_import, &[LIB], "ScriptName T\nImport $0Nope\n");
    }
}
