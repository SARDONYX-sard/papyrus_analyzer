//! `unknown-mod-event`: a custom event one typo away from another one, which
//! nothing connects: probably the same event.
//!
//! ```papyrus
//! RegisterForModEvent("MyEvnet", "OnMyEvent")   ; no script sends it: did you mean `MyEvent`?
//! SendModEvent("MyEvent")                       ; no script registers it: did you mean `MyEvnet`?
//! ```
//!
//! An event only registered, or only sent, is no problem by itself: a mod may
//! send it from a Flash menu or a DLL, and another mod may listen. So both names
//! must lack their other half, and differ by one typo only: many mods name their
//! events alike (`mymod_start`, `mymod_end`, `mymod_event`). The strings of every
//! known script count; other string expressions are not followed.
//!
//! Fix: the closest names. It changes what the code does, so `lint --fix`
//! leaves the choice to the author, unless `--allow-manual-fixes` is passed and
//! one name is the closest.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    mod_events::{ModEventRef, ModEventSite, ModEventUse, mod_event_ref, sites},
    sema::Sema,
};
use papyrus_parser::SyntaxKind;

use crate::{Group, Lint};

const CODE: &str = "unknown-mod-event";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "a custom event one typo away from another one, nothing connecting them",
    run,
    fix: Some(fix),
    manual_fix: true,
};

/// The other half of a use: who sends what is registered, and the other way.
fn other_half(usage: ModEventUse) -> Option<ModEventUse> {
    match usage {
        ModEventUse::Register => Some(ModEventUse::Send),
        ModEventUse::Send => Some(ModEventUse::Register),
        ModEventUse::Unregister | ModEventUse::Callback => None,
    }
}

/// The names `event` is probably a typo of; empty when it is fine.
fn typo_of(event: &ModEventRef<'_>, all: &[ModEventSite]) -> Vec<String> {
    let Some(other) = other_half(event.usage) else { return Vec::new() };
    let has = |usage: ModEventUse, name: &str| {
        all.iter().any(|s| s.usage == usage && s.name.eq_ignore_ascii_case(name))
    };
    if event.name.is_empty() || has(other, event.name) {
        return Vec::new();
    }
    // Names lacking the same half: a complete event is another event.
    let mut names: Vec<String> = all
        .iter()
        .filter(|s| s.usage == other && !has(event.usage, &s.name))
        .filter(|s| is_typo(event.name, &s.name))
        .map(|s| s.name.clone())
        .collect();
    names.sort_by_key(|n| n.to_ascii_lowercase());
    names.dedup_by_key(|n| n.to_ascii_lowercase());
    names
}

/// Whether two names differ by one typo: a character missing, added or wrong, or
/// two neighbors swapped. Names of a family (`mod_start`, `mod_end`, `mod_event`)
/// differ more; numbered ones (`Thread1`, `Thread2`) are other events.
fn is_typo(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    let without_digits = |s: &str| s.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    if without_digits(&a) == without_digits(&b) {
        return false;
    }
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let swapped = x.len() == y.len() && {
        let wrong: Vec<usize> = (0..x.len()).filter(|&i| x[i] != y[i]).collect();
        matches!(wrong[..], [i, j] if j == i + 1 && x[i] == y[j] && x[j] == y[i])
    };
    swapped || stdx::edit_distance(&a, &b) == 1
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    let events: Vec<ModEventRef<'_>> = sema
        .root
        .descendant_tokens()
        .filter(|t| t.kind() == SyntaxKind::STRING)
        .filter_map(|t| mod_event_ref(sema, t))
        .filter(|r| other_half(r.usage).is_some())
        .collect();
    if events.is_empty() {
        return;
    }
    let all = sites(sema.ws, sema.file);
    for event in events {
        let Some(name) = typo_of(&event, &all).into_iter().next() else { continue };
        let verb = if event.usage == ModEventUse::Register { "sends" } else { "registers" };
        out.push(Diagnostic::warning(
            event.token.text_range(),
            CODE,
            format!("no known script {verb} `{}`: did you mean `{name}`?", event.name),
        ));
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(event) = ctx.token().and_then(|t| mod_event_ref(ctx.sema, t)) else { return };
    if other_half(event.usage).is_none() {
        return;
    }
    for name in typo_of(&event, &sites(ctx.sema.ws, ctx.sema.file)) {
        acc.push(Assist {
            id: "fix_mod_event_name",
            label: format!("Change to `{name}`"),
            kind: AssistKind::QuickFix,
            target: event.token.text_range(),
            edits: vec![TextEdit { range: event.name_range, new_text: name }],
            fixes: Some(CODE),
        });
    }
}

#[cfg(test)]
mod tests {
    use papyrus_ide::Workspace;

    use crate::{Level, LintConfig};

    const FORM: &str = "ScriptName Form Hidden\nFunction RegisterForModEvent(String eventName, String callbackName) Native\nFunction SendModEvent(String eventName, String strArg = \"\", Float numArg = 0.0) Native\n";
    const MOD_EVENT: &str =
        "ScriptName ModEvent Hidden\nInt Function Create(String eventName) Global Native\n";

    fn workspace(body: &str) -> (Workspace, papyrus_ide::FileId) {
        let mut ws = Workspace::new();
        ws.set_open("/ws/Form.psc".into(), FORM.into(), 0);
        ws.set_open("/ws/ModEvent.psc".into(), MOD_EVENT.into(), 0);
        let src = format!(
            "ScriptName T extends Form\nFunction F()\n{body}EndFunction\nEvent OnA()\nEndEvent\n"
        );
        let file = ws.set_open("/ws/T.psc".into(), src.as_str().into(), 0);
        (ws, file)
    }

    fn check(body: &str) -> Vec<String> {
        let (ws, file) = workspace(body);
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("unknown-mod-event", Level::Warn).unwrap();
        crate::lint(&ws, file, &config).into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn reports_close_names_lacking_their_other_half() {
        let body = "    RegisterForModEvent(\"MyEvnet\", \"OnA\")\n    SendModEvent(\"MyEvent\")\n";
        assert_eq!(
            check(body),
            [
                "no known script sends `MyEvnet`: did you mean `MyEvent`?",
                "no known script registers `MyEvent`: did you mean `MyEvnet`?",
            ]
        );
    }

    #[test]
    fn mod_event_create_sends_too() {
        let body = "    RegisterForModEvent(\"MyEvnet\", \"OnA\")\n    Int h = ModEvent.Create(\"MyEvent\")\n";
        assert_eq!(check(body).len(), 2);
    }

    #[test]
    fn events_may_be_only_registered_or_only_sent() {
        assert_eq!(check("    RegisterForModEvent(\"MenuOpen\", \"OnA\")\n"), [""; 0]);
        assert_eq!(check("    SendModEvent(\"Started\")\n"), [""; 0]);
        // Far from each other.
        let body = "    RegisterForModEvent(\"Opened\", \"OnA\")\n    SendModEvent(\"Finished\")\n";
        assert_eq!(check(body), [""; 0]);
    }

    #[test]
    fn names_of_a_family_are_other_events() {
        // Sent from a DLL, or listened to by another mod.
        for (registered, sent) in [
            ("mymod_start", "mymod_event"),
            ("mymod_end", "mymod_event"),
            ("mymod_thread_end", "mymod_subthread_end"),
            ("Thread1", "Thread2"),
        ] {
            let body = format!(
                "    RegisterForModEvent(\"{registered}\", \"OnA\")\n    SendModEvent(\"{sent}\")\n"
            );
            assert_eq!(check(&body), [""; 0], "{registered} {sent}");
        }
    }

    #[test]
    fn complete_events_are_other_events() {
        let body = "    RegisterForModEvent(\"Thread1\", \"OnA\")\n    SendModEvent(\"Thread1\")\n    RegisterForModEvent(\"Thread2\", \"OnA\")\n    SendModEvent(\"Thread2\")\n    RegisterForModEvent(\"Thread3\", \"OnA\")\n";
        assert_eq!(check(body), [""; 0]);
    }

    #[test]
    fn lint_fix_leaves_the_guess_to_the_author() {
        let body = "    RegisterForModEvent(\"MyEvnet\", \"OnA\")\n    SendModEvent(\"MyEvent\")\n";
        let (mut ws, file) = workspace(body);
        let config = LintConfig::default();
        let (_, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!(applied, 0);
    }
}
