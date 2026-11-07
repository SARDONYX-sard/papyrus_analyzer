//! `repeating-update`: `RegisterForUpdate`, whose updates keep coming whether
//! the previous one is done or not.
//!
//! ```papyrus
//! Event OnInit()
//!     RegisterForUpdate(1.0)         ; → RegisterForSingleUpdate(1.0)
//! EndEvent
//!
//! Event OnUpdate()
//!     Work()
//!     RegisterForSingleUpdate(1.0)   ; added: the next one, once this one is done
//! EndEvent
//! ```
//!
//! Only the function of the game counts, not one a script declares.
//!
//! Fix: as above, in every `OnUpdate` of the file not registering again or
//! unregistering already. It must keep the updates coming, so there is none:
//!
//! - without `OnUpdate` in the file: it may be of another script;
//! - with `Return` in `OnUpdate`: the updates would stop there;
//! - when `OnUpdate` cannot read the interval: a local variable, a parameter;
//! - on another object, whose `OnUpdate` is another one.
//!
//! The first update also comes later than it did if the work is long, so
//! `lint --fix` leaves it to the author, unless `--allow-manual-fixes` is passed.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    hir::{Body, Expr, Stmt},
    index::Flags,
    sema::{Def, Sema},
};
use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{Group, Lint, bodies};

const CODE: &str = "repeating-update";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "`RegisterForUpdate`, whose updates can pile up",
    run,
    fix: Some(fix),
    manual_fix: true,
};

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !sema.index.mentions(Name::new("RegisterForUpdate")) {
        return;
    }
    for body in bodies(sema) {
        for (expr, range) in &body.exprs {
            let Expr::Call { name, def: Some(Def::Member { file, index }), .. } = expr else {
                continue;
            };
            if name.as_str().eq_ignore_ascii_case("RegisterForUpdate")
                && sema.member(*file, *index).flags.contains(Flags::NATIVE)
            {
                out.push(Diagnostic::warning(
                    *range,
                    CODE,
                    "updates can pile up: use `RegisterForSingleUpdate`, and call it again at the end of `OnUpdate`",
                ));
            }
        }
    }
}

/// Whether the interval can be written in `OnUpdate`: a literal, or a member of
/// the script.
fn is_readable_anywhere(sema: &Sema<'_>, value: ast::Expr<'_>) -> bool {
    match value {
        ast::Expr::Literal(_) => true,
        ast::Expr::NameRef(name) => matches!(sema.resolve_name_ref(name), Some(Def::Member { .. })),
        _ => false,
    }
}

/// The text added to the `OnUpdate` event `event`, if any; `None` when the
/// updates would stop in it.
fn register_again(
    ctx: &AssistCtx<'_>,
    event: ast::Event<'_>,
    interval: &str,
) -> Option<Option<TextEdit>> {
    let text = ctx.sema.text;
    let body = Body::lower(ctx.sema, event.syntax())?;
    if body.stmts.iter().any(|(s, _)| matches!(s, Stmt::Return(_))) {
        return None;
    }
    let calls = |name: &str| {
        body.exprs.iter().any(
            |(e, _)| matches!(e, Expr::Call { name: n, .. } if n.as_str().eq_ignore_ascii_case(name)),
        )
    };
    if calls("RegisterForSingleUpdate") || calls("UnregisterForUpdate") {
        return Some(None);
    }
    let end = event.syntax().child_token(SyntaxKind::ENDEVENT_KW)?.text_range().start();
    let line_start = |at: TextSize| {
        let at = usize::from(at);
        text[..at].rfind('\n').map_or(0, |i| i + 1)
    };
    let indent_of = |start: usize| -> String {
        text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    };
    let start = line_start(end);
    let indent = match body.root.last() {
        Some(&last) => indent_of(line_start(body.stmt_range(last).start())),
        None => format!("{}    ", indent_of(start)),
    };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let at = TextSize::new(start as u32);
    Some(Some(TextEdit {
        range: TextRange::empty(at),
        new_text: format!("{indent}RegisterForSingleUpdate({interval}){newline}"),
    }))
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(call) = ctx.find_node::<ast::CallExpr<'_>>() else { return };
    let Some(callee) = call.callee_name() else { return };
    if !callee.text().eq_ignore_ascii_case("RegisterForUpdate") {
        return;
    }
    let Some(Def::Member { file, index }) = ctx.sema.resolve_name_ref(callee) else { return };
    if !ctx.sema.member(file, index).flags.contains(Flags::NATIVE) {
        return;
    }
    let on_self = match call.callee() {
        Some(ast::Expr::Field(field)) => {
            matches!(field.receiver(), Some(ast::Expr::NameRef(n)) if n.is_self())
        }
        _ => true,
    };
    if !on_self {
        return;
    }
    let Some(value) = call.arg_list().and_then(|l| l.args().next()).and_then(|a| a.value()) else {
        return;
    };
    if !is_readable_anywhere(ctx.sema, value) {
        return;
    }
    let interval = ctx.text(value.syntax());
    let events: Vec<ast::Event<'_>> = ctx
        .sema
        .root
        .descendants()
        .filter_map(ast::Event::cast)
        .filter(|e| e.name().is_some_and(|n| n.text().eq_ignore_ascii_case("OnUpdate")))
        .collect();
    if events.is_empty() {
        return;
    }
    let Some(token) = callee.token() else { return };
    let mut edits = vec![TextEdit {
        range: token.text_range(),
        new_text: "RegisterForSingleUpdate".to_owned(),
    }];
    for event in events {
        match register_again(ctx, event, interval) {
            Some(edit) => edits.extend(edit),
            None => return,
        }
    }
    acc.push(Assist {
        id: "use_single_update",
        label: "Use `RegisterForSingleUpdate`, again at the end of `OnUpdate`".to_owned(),
        kind: AssistKind::QuickFix,
        target: call.syntax().trimmed_range(),
        edits,
        fixes: Some(CODE),
    });
}

#[cfg(test)]
mod tests {
    use papyrus_ide::assists::test_utils::{check_assist, check_assist_not_applicable};

    use super::fix;
    use crate::test_utils::check;

    const NATIVES: &str = "ScriptName T\nFunction RegisterForUpdate(Float f) Native\nFunction RegisterForSingleUpdate(Float f) Native\nFunction UnregisterForUpdate() Native\nFloat Property Interval Auto\n";

    #[test]
    fn reports_the_native_function_only() {
        let form = format!(
            "{NATIVES}Event OnInit()\n    RegisterForUpdate(1.0)\n    RegisterForSingleUpdate(1.0)\nEndEvent\n"
        );
        assert_eq!(
            check("repeating-update", &form),
            [
                "7: updates can pile up: use `RegisterForSingleUpdate`, and call it again at the end of `OnUpdate`"
            ]
        );
        let own = "ScriptName T\nFunction RegisterForUpdate(Float f)\nEndFunction\nEvent OnInit()\n    RegisterForUpdate(1.0)\nEndEvent\n";
        assert_eq!(check("repeating-update", own), [""; 0]);
    }

    #[test]
    fn fix_registers_again_at_the_end_of_on_update() {
        check_assist(
            fix,
            &format!(
                "{NATIVES}Event OnInit()\n    RegisterFor$0Update(1.0)\nEndEvent\nEvent OnUpdate()\n    Debug.Trace(\"x\")\nEndEvent\n"
            ),
            &format!(
                "{NATIVES}Event OnInit()\n    RegisterForSingleUpdate(1.0)\nEndEvent\nEvent OnUpdate()\n    Debug.Trace(\"x\")\n    RegisterForSingleUpdate(1.0)\nEndEvent\n"
            ),
        );
    }

    #[test]
    fn fix_covers_the_states_and_keeps_the_interval() {
        check_assist(
            fix,
            &format!(
                "{NATIVES}Event OnInit()\n    RegisterFor$0Update(Interval)\nEndEvent\nEvent OnUpdate()\nEndEvent\nState Busy\n\tEvent OnUpdate()\n\t\tDebug.Trace(\"x\")\n\tEndEvent\nEndState\n"
            ),
            &format!(
                "{NATIVES}Event OnInit()\n    RegisterForSingleUpdate(Interval)\nEndEvent\nEvent OnUpdate()\n    RegisterForSingleUpdate(Interval)\nEndEvent\nState Busy\n\tEvent OnUpdate()\n\t\tDebug.Trace(\"x\")\n\t\tRegisterForSingleUpdate(Interval)\n\tEndEvent\nEndState\n"
            ),
        );
    }

    #[test]
    fn fix_keeps_what_on_update_does_already() {
        for line in ["RegisterForSingleUpdate(2.0)", "UnregisterForUpdate()"] {
            check_assist(
                fix,
                &format!(
                    "{NATIVES}Event OnInit()\n    RegisterFor$0Update(1.0)\nEndEvent\nEvent OnUpdate()\n    {line}\nEndEvent\n"
                ),
                &format!(
                    "{NATIVES}Event OnInit()\n    RegisterForSingleUpdate(1.0)\nEndEvent\nEvent OnUpdate()\n    {line}\nEndEvent\n"
                ),
            );
        }
    }

    #[test]
    fn no_fix_when_the_updates_would_stop() {
        // Without OnUpdate here; with a way out of it.
        check_assist_not_applicable(
            fix,
            &format!("{NATIVES}Event OnInit()\n    RegisterFor$0Update(1.0)\nEndEvent\n"),
        );
        check_assist_not_applicable(
            fix,
            &format!(
                "{NATIVES}Event OnInit()\n    RegisterFor$0Update(1.0)\nEndEvent\nEvent OnUpdate()\n    If Interval\n        Return\n    EndIf\nEndEvent\n"
            ),
        );
        // OnUpdate cannot read the interval.
        check_assist_not_applicable(
            fix,
            &format!(
                "{NATIVES}Function F(Float afSeconds)\n    RegisterFor$0Update(afSeconds)\nEndFunction\nEvent OnUpdate()\nEndEvent\n"
            ),
        );
        // The update of another object.
        check_assist_not_applicable(
            fix,
            &format!(
                "{NATIVES}Function F(T akOther)\n    akOther.RegisterFor$0Update(1.0)\nEndFunction\nEvent OnUpdate()\nEndEvent\n"
            ),
        );
    }

    #[test]
    fn lint_fix_leaves_it_to_the_author() {
        let src = format!(
            "{NATIVES}Event OnInit()\n    RegisterForUpdate(1.0)\nEndEvent\nEvent OnUpdate()\nEndEvent\n"
        );
        let mut ws = papyrus_ide::Workspace::new();
        let file = ws.set_open("/ws/T.psc".into(), src.as_str().into(), 0);
        let config = crate::LintConfig::default();
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!((fixed.as_str(), applied), (src.as_str(), 0));
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, true);
        assert_eq!(applied, 1);
        assert!(fixed.contains("Event OnUpdate()\n    RegisterForSingleUpdate(1.0)\n"), "{fixed}");
    }
}
