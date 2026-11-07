//! `unused-import`: an `Import` whose global functions are never called.
//!
//! ```papyrus
//! Import Utility   ; nothing calls a function of `Utility` without `Utility.`
//! Import Game
//!
//! Function F()
//!     Utility.Wait(1.0)
//!     GetPlayer().Kill()
//! EndFunction
//! ```
//!
//! `Import` only lets global functions be called without the script name. A
//! script that is not known is not reported: what it provides is not known.
//!
//! Fix: removes the line.

use papyrus_ide::{
    Diagnostic, FileId,
    assists::{Assist, AssistCtx, AssistKind, TextEdit, line_range},
    hir::Expr,
    sema::{Def, Sema},
};
use papyrus_intern::Name;
use papyrus_parser::ast::{self, AstNode};

use crate::{Group, Lint, bodies};

const CODE: &str = "unused-import";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "an `Import` whose global functions are never called",
    run,
    fix: Some(fix),
    manual_fix: false,
};

/// The files whose functions the script calls without receiver.
fn called_files(sema: &Sema<'_>) -> Vec<FileId> {
    let mut files = Vec::new();
    for body in bodies(sema) {
        for (expr, _) in &body.exprs {
            if let Expr::Call { receiver: None, def: Some(Def::Member { file, .. }), .. } = expr {
                files.push(*file);
            }
        }
    }
    files
}

/// Whether `import` is known and none of its functions in `called`.
fn is_unused(sema: &Sema<'_>, import: ast::Import<'_>, called: &[FileId]) -> bool {
    let Some(name) = import.name_ref() else { return false };
    let Some(file) = sema.ws.script_near(Name::new(name.text()), sema.file) else { return false };
    // Importing itself gives nothing either, but its calls resolve to it.
    !called.contains(&file)
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let imports = || sema.root.descendants().filter_map(ast::Import::cast);
    if imports().next().is_none() {
        return;
    }
    let called = called_files(sema);
    for import in imports() {
        if is_unused(sema, import, &called) {
            let name = import.name_ref().map_or("", |n| n.text());
            out.push(Diagnostic::warning(
                import.syntax().trimmed_range(),
                CODE,
                format!("no function of `{name}` is called without its script name"),
            ));
        }
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(import) = ctx.find_node::<ast::Import<'_>>() else { return };
    if !is_unused(ctx.sema, import, &called_files(ctx.sema)) {
        return;
    }
    let range = import.syntax().trimmed_range();
    acc.push(Assist {
        id: "remove_unused_import",
        label: "Remove the unused `Import`".to_owned(),
        kind: AssistKind::QuickFix,
        target: range,
        edits: vec![TextEdit { range: line_range(ctx.sema.text, range), new_text: String::new() }],
        fixes: Some(CODE),
    });
}

#[cfg(test)]
mod tests {
    use papyrus_ide::Workspace;

    use crate::{Level, LintConfig};

    const LIB: &str = "ScriptName Lib\nFunction Used() Global\nEndFunction\n";
    const OTHER: &str = "ScriptName Other\nFunction Unrelated() Global\nEndFunction\n";

    fn check(src: &str) -> Vec<String> {
        let mut ws = Workspace::new();
        ws.set_open("/ws/Lib.psc".into(), LIB.into(), 0);
        ws.set_open("/ws/Other.psc".into(), OTHER.into(), 0);
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("unused-import", Level::Warn).unwrap();
        crate::lint(&ws, file, &config).into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn reports_imports_without_calls() {
        let src = "ScriptName T\nImport Lib\nImport Other\nFunction F()\n    Used()\nEndFunction\n";
        assert_eq!(check(src), ["no function of `Other` is called without its script name"]);
    }

    #[test]
    fn a_call_with_the_script_name_needs_no_import() {
        let src = "ScriptName T\nImport Lib\nFunction F()\n    Lib.Used()\nEndFunction\n";
        assert_eq!(check(src), ["no function of `Lib` is called without its script name"]);
    }

    #[test]
    fn unknown_scripts_are_not_reported() {
        assert_eq!(check("ScriptName T\nImport Missing\n"), [""; 0]);
    }

    #[test]
    fn fix_removes_the_line() {
        let src = "ScriptName T\nImport Other\nImport Lib\nFunction F()\n    Used()\nEndFunction\n";
        let mut ws = Workspace::new();
        ws.set_open("/ws/Lib.psc".into(), LIB.into(), 0);
        ws.set_open("/ws/Other.psc".into(), OTHER.into(), 0);
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        let (fixed, applied) = crate::fix_all(&mut ws, file, &LintConfig::default(), false);
        assert_eq!(applied, 1);
        assert_eq!(fixed, "ScriptName T\nImport Lib\nFunction F()\n    Used()\nEndFunction\n");
    }
}
