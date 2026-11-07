//! `unknown-mod-callback`: `RegisterForModEvent` with a callback declared nowhere.
//!
//! ```papyrus
//! RegisterForModEvent("MyEvent", "OnMyEvnt")   ; no `OnMyEvnt`: did you mean `OnMyEvent`?
//!
//! Event OnMyEvent(String asName, String asArg, Float afArg, Form akSender)
//! EndEvent
//! ```
//!
//! The compiler accepts it, since the name is a string; the event then never
//! arrives. Only `RegisterForModEvent` on `Self` is checked: another object may
//! be of any script extending its type. The script, its parents and the known
//! scripts extending it count, and they must all be known.
//!
//! Fix: the closest names. It changes what the code does, so `lint --fix`
//! leaves the choice to the author, unless `--allow-manual-fixes` is passed and
//! one name is the closest.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    mod_events::{ModEventRef, ModEventUse, callables, is_declared, is_event, mod_event_ref},
    sema::Sema,
};
use papyrus_intern::Name;

use crate::{Group, Lint};

const CODE: &str = "unknown-mod-callback";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "`RegisterForModEvent` with a callback declared nowhere",
    run,
    fix: Some(fix),
    manual_fix: true,
};

fn is_unknown(sema: &Sema<'_>, callback: &ModEventRef<'_>) -> bool {
    callback.usage == ModEventUse::Callback
        && callback.on_self
        && !callback.name.is_empty()
        && callback.script.is_some_and(|s| sema.chain_is_complete(s))
        && !is_declared(sema, callback)
}

/// The events closest to the name of `callback`.
fn closest(sema: &Sema<'_>, callback: &ModEventRef<'_>) -> Vec<&'static str> {
    let names = callables(sema, callback);
    stdx::closest_matches(
        callback.name,
        names.iter().filter(|d| is_event(sema, d)).map(|d| d.name.as_str()),
    )
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !sema.index.mentions(Name::new("RegisterForModEvent")) {
        return;
    }
    for token in sema.root.descendant_tokens() {
        let Some(callback) = mod_event_ref(sema, token) else { continue };
        if !is_unknown(sema, &callback) {
            continue;
        }
        let hint = match closest(sema, &callback).first() {
            Some(name) => format!(": did you mean `{name}`?"),
            None => String::new(),
        };
        out.push(Diagnostic::warning(
            token.text_range(),
            CODE,
            format!("no event `{}` in `{}`{hint}", callback.name, sema.index.name_str()),
        ));
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(callback) = ctx.token().and_then(|t| mod_event_ref(ctx.sema, t)) else { return };
    if !is_unknown(ctx.sema, &callback) {
        return;
    }
    for name in closest(ctx.sema, &callback) {
        acc.push(Assist {
            id: "fix_mod_callback_name",
            label: format!("Change to `{name}`"),
            kind: AssistKind::QuickFix,
            target: callback.token.text_range(),
            edits: vec![TextEdit { range: callback.name_range, new_text: name.to_owned() }],
            fixes: Some(CODE),
        });
    }
}

#[cfg(test)]
mod tests {
    use papyrus_ide::Workspace;

    use crate::{Level, LintConfig};

    const FORM: &str = "ScriptName Form Hidden\nFunction RegisterForModEvent(String eventName, String callbackName) Native\n";

    fn workspace(src: &str) -> (Workspace, papyrus_ide::FileId) {
        let mut ws = Workspace::new();
        ws.set_open("/ws/Form.psc".into(), FORM.into(), 0);
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        (ws, file)
    }

    fn config() -> LintConfig {
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("unknown-mod-callback", Level::Warn).unwrap();
        config
    }

    fn check(src: &str) -> Vec<String> {
        let (ws, file) = workspace(src);
        crate::lint(&ws, file, &config()).into_iter().map(|d| d.message).collect()
    }

    fn script(extends: &str, call: &str) -> String {
        format!(
            "ScriptName T extends {extends}\nFunction F(Form akOther, String asName)\n    {call}\nEndFunction\nEvent OnMyEvent()\nEndEvent\n"
        )
    }

    #[test]
    fn reports_callbacks_declared_nowhere() {
        assert_eq!(
            check(&script("Form", "RegisterForModEvent(\"A\", \"OnMyEvnt\")")),
            ["no event `OnMyEvnt` in `T`: did you mean `OnMyEvent`?"]
        );
        assert_eq!(
            check(&script("Form", "Self.RegisterForModEvent(\"A\", \"NothingLikeIt\")")),
            ["no event `NothingLikeIt` in `T`"]
        );
    }

    #[test]
    fn what_cannot_be_told_is_not_reported() {
        for call in [
            "RegisterForModEvent(\"A\", \"OnMyEvent\")",
            "RegisterForModEvent(\"A\", \"F\")",
            "RegisterForModEvent(\"A\", asName)",
            "akOther.RegisterForModEvent(\"A\", \"OnElsewhere\")",
        ] {
            assert_eq!(check(&script("Form", call)), [""; 0], "{call}");
        }
        let unknown_parent = script("Missing", "RegisterForModEvent(\"A\", \"OnMyEvnt\")");
        assert_eq!(check(&unknown_parent), [""; 0]);
    }

    #[test]
    fn callbacks_of_scripts_extending_it_count() {
        let (mut ws, file) = workspace(&script("Form", "RegisterForModEvent(\"A\", \"OnChild\")"));
        ws.set_open(
            "/ws/C.psc".into(),
            "ScriptName C extends T\nEvent OnChild()\nEndEvent\n".into(),
            0,
        );
        assert!(crate::lint(&ws, file, &config()).is_empty());
    }

    #[test]
    fn lint_fix_leaves_the_guess_to_the_author() {
        let src = script("Form", "RegisterForModEvent(\"A\", \"OnMyEvnt\")");
        let (mut ws, file) = workspace(&src);
        let config = LintConfig::default();
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!((fixed.as_str(), applied), (src.as_str(), 0));
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, true);
        assert_eq!(applied, 1);
        assert!(fixed.contains("\"OnMyEvent\")"), "{fixed}");
    }
}
