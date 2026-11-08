//! `unknown-event`: an event spelled almost like one of the parents of the
//! script, which do not declare it, and that nothing refers to. The game will
//! never send it.
//!
//! ```papyrus
//! ScriptName MyDoor extends ObjectReference
//!
//! Event OnActivte(ObjectReference akActionRef)   ; did you mean `OnActivate`?
//!     Open()
//! EndEvent
//! ```
//!
//! The compiler accepts it, since an event may be new. Some are on purpose,
//! which are not reported:
//!
//! - not close to an event of the parents: a new event, like the ones a library
//!   declares for the scripts extending it, unknown here;
//! - named by another part of the script or by another known script: a call,
//!   or a string like `RegisterForModEvent("MyEvent", "OnMyEvent")`;
//! - without statements: the declaration of an event, as in the scripts of the
//!   game;
//! - in a script without parent, or whose parents are not all known.
//!
//! Fix: the closest names. It changes what the code does, so `lint --fix`
//! leaves the choice to the author, unless `--allow-manual-fixes` is passed and
//! one name is the closest.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    hir::Body,
    index::MemberKind,
    sema::Sema,
};
use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind,
    ast::{self, AstNode},
};

use crate::{Group, Lint};

const CODE: &str = "unknown-event";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "an event spelled almost like one of the parents, probably a typo",
    run,
    fix: Some(fix),
    manual_fix: true,
};

/// The events of the parents of the script.
fn parent_events(sema: &Sema<'_>) -> Vec<Name> {
    let Some(script) = sema.script_name() else { return Vec::new() };
    let events = sema
        .chain(script)
        .skip(1)
        .flat_map(|(_, index)| index.members.iter())
        .filter(|m| m.kind == MemberKind::Event);
    events.map(|m| m.name).collect()
}

/// Whether the script has parents, all known: its events can be looked up.
fn can_tell(sema: &Sema<'_>) -> bool {
    sema.index.extends.is_some() && sema.script_name().is_some_and(|s| sema.chain_is_complete(s))
}

/// Whether the event `event` of the script is a typo no one can receive.
fn is_unknown(sema: &Sema<'_>, event: ast::Event<'_>, parents: &[Name]) -> bool {
    let Some(name) = event.name() else { return false };
    let text = name.text();
    let name = Name::new(text);
    !parents.contains(&name)
        && !closest(text, parents).is_empty()
        && Body::lower(sema, event.syntax()).is_some_and(|b| !b.root.is_empty())
        && !is_named_elsewhere(sema, name)
}

/// Whether anything but the declarations of the event names it, in this file
/// or another known one.
fn is_named_elsewhere(sema: &Sema<'_>, name: Name) -> bool {
    let declarations: Vec<_> = sema
        .index
        .members
        .iter()
        .filter(|m| m.kind == MemberKind::Event && m.name == name)
        .map(|m| m.name_range)
        .collect();
    let in_file = sema.root.descendant_tokens().any(|t| {
        let spelled = match t.kind() {
            SyntaxKind::IDENT => t.text(),
            SyntaxKind::STRING => t.text().trim_matches('"'),
            _ => return false,
        };
        spelled.eq_ignore_ascii_case(name.as_str()) && !declarations.contains(&t.text_range())
    });
    in_file
        || sema
            .ws
            .all_scripts_from(sema.file)
            .any(|(_, file)| file != sema.file && sema.ws.file(file).index.mentions(name))
}

fn closest<'a>(name: &str, parents: &'a [Name]) -> Vec<&'a str> {
    stdx::closest_matches(name, parents.iter().map(|n| n.as_str()))
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    if !can_tell(sema) {
        return;
    }
    let parents = parent_events(sema);
    for event in sema.root.descendants().filter_map(ast::Event::cast) {
        if !is_unknown(sema, event, &parents) {
            continue;
        }
        let Some(name) = event.name() else { continue };
        let Some(found) = closest(name.text(), &parents).first().copied() else { continue };
        out.push(Diagnostic::warning(
            name.syntax().text_range(),
            CODE,
            format!(
                "no parent of `{}` has the event `{}`: did you mean `{found}`?",
                sema.index.name_str(),
                name.text()
            ),
        ));
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(event) = ctx.find_node::<ast::Event<'_>>() else { return };
    let Some(name) = event.name() else { return };
    let range = name.syntax().text_range();
    if !range.contains_range(ctx.range) || !can_tell(ctx.sema) {
        return;
    }
    let parents = parent_events(ctx.sema);
    if !is_unknown(ctx.sema, event, &parents) {
        return;
    }
    for found in closest(name.text(), &parents) {
        acc.push(Assist {
            id: "fix_event_name",
            label: format!("Change to `{found}`"),
            kind: AssistKind::QuickFix,
            target: range,
            edits: vec![TextEdit { range, new_text: found.to_owned() }],
            fixes: Some(CODE),
            other_files: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use papyrus_ide::Workspace;

    use crate::{Level, LintConfig};

    const BASE: &str = "ScriptName Base Hidden\nEvent OnActivate(Base akRef)\nEndEvent\nEvent OnInit()\nEndEvent\n";

    fn workspace(src: &str) -> (Workspace, papyrus_ide::FileId) {
        let mut ws = Workspace::new();
        ws.set_open("/ws/Base.psc".into(), BASE.into(), 0);
        let file = ws.set_open("/ws/T.psc".into(), src.into(), 0);
        (ws, file)
    }

    fn check(src: &str) -> Vec<String> {
        let (ws, file) = workspace(src);
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("unknown-event", Level::Warn).unwrap();
        crate::lint(&ws, file, &config).into_iter().map(|d| d.message).collect()
    }

    fn event(name: &str) -> String {
        format!("Event {name}()\n    Debug.Trace(\"x\")\nEndEvent\n")
    }

    #[test]
    fn reports_events_close_to_one_of_the_parents() {
        let src = format!(
            "ScriptName T extends Base\n{}{}{}",
            event("OnActivte"),
            event("OnInit"),
            event("OnNothingLikeIt")
        );
        assert_eq!(
            check(&src),
            ["no parent of `T` has the event `OnActivte`: did you mean `OnActivate`?"]
        );
    }

    #[test]
    fn events_named_elsewhere_are_on_purpose() {
        let by_string = format!(
            "ScriptName T extends Base\n{}Function F()\n    RegisterForModEvent(\"My\", \"OnActivte\")\nEndFunction\n",
            event("OnActivte"),
        );
        assert_eq!(check(&by_string), [""; 0]);
        let by_call = format!(
            "ScriptName T extends Base\n{}Function F()\n    OnActivte()\nEndFunction\n",
            event("OnActivte"),
        );
        assert_eq!(check(&by_call), [""; 0]);

        let src = format!("ScriptName T extends Base\n{}", event("OnActivte"));
        let (mut ws, file) = workspace(&src);
        ws.set_open(
            "/ws/O.psc".into(),
            "ScriptName O\nFunction F(T o)\n    o.OnActivte()\nEndFunction\n".into(),
            0,
        );
        let mut config = LintConfig::default();
        config.set("all", Level::Allow).unwrap();
        config.set("unknown-event", Level::Warn).unwrap();
        assert!(crate::lint(&ws, file, &config).is_empty());
    }

    #[test]
    fn declarations_and_unknown_parents_are_not_reported() {
        assert_eq!(check("ScriptName T extends Base\nEvent OnActivte()\nEndEvent\n"), [""; 0]);
        assert_eq!(check(&format!("ScriptName T\n{}", event("OnActivte"))), [""; 0]);
        let unknown_parent = format!("ScriptName T extends Missing\n{}", event("OnActivte"));
        assert_eq!(check(&unknown_parent), [""; 0]);
    }

    #[test]
    fn lint_fix_leaves_the_guess_to_the_author() {
        let src = format!("ScriptName T extends Base\n{}", event("OnActivte"));
        let (mut ws, file) = workspace(&src);
        let config = LintConfig::default();
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!((fixed.as_str(), applied), (src.as_str(), 0));
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, true);
        assert_eq!(applied, 1);
        assert!(fixed.contains("Event OnActivate()"), "{fixed}");
    }
}
