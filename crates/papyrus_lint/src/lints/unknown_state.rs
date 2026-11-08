//! `unknown-state`: `GoToState` or `GetState() ==` with a state declared nowhere.
//!
//! ```papyrus
//! State Busy
//! EndState
//!
//! Function F()
//!     GoToState("Bussy")     ; no state `Bussy`: did you mean `Busy`?
//!     If GetState() == "Idle" ; no state `Idle`
//!     EndIf
//! EndFunction
//! ```
//!
//! The compiler accepts it, since the name is a string. It may be a typo, or a
//! state left out; some scripts of the game do it on purpose, hence not
//! `correctness`.
//!
//! The states of the script, of its parents and of the known scripts extending
//! it count; `""` is the empty state. A name both set with `GoToState` and
//! compared with `GetState()` in the file is a flag, on purpose. Only string
//! literals are checked, and only when every parent of the script is known.
//!
//! Fix: the closest names. It changes what the code does, so `lint --fix`
//! leaves the choice to the author, unless `--allow-manual-fixes` is passed and
//! one name is the closest.

use papyrus_ide::{
    Diagnostic,
    assists::{Assist, AssistCtx, AssistKind, TextEdit},
    sema::Sema,
    states::{StateRef, candidates, is_declared, state_ref},
};

use crate::{Group, Lint};

const CODE: &str = "unknown-state";

pub(crate) const LINT: Lint = Lint {
    code: CODE,
    group: Group::Suspicious,
    summary: "`GoToState` or `GetState() ==` with a state declared nowhere",
    run,
    fix: Some(fix),
    manual_fix: true,
};

/// Whether no script declares the state of `state`.
fn is_unknown(sema: &Sema<'_>, state: &StateRef<'_>) -> bool {
    !state.name.is_empty()
        && sema.chain_is_complete(state.script)
        && !is_declared(sema, state.script, state.name)
        && !is_flag(sema, state)
}

/// Whether the file uses the name of `state` the other way too, setting it
/// with `GoToState` and testing it with `GetState()`: a flag then, on purpose
/// without any code.
fn is_flag(sema: &Sema<'_>, state: &StateRef<'_>) -> bool {
    sema.root
        .descendant_tokens()
        .filter_map(|t| state_ref(sema, t))
        .any(|other| other.usage != state.usage && other.name.eq_ignore_ascii_case(state.name))
}

/// The declared states closest to the name of `state`.
fn closest(sema: &Sema<'_>, state: &StateRef<'_>) -> Vec<&'static str> {
    let names = candidates(sema, state);
    stdx::closest_matches(state.name, names.iter().map(|d| d.name.as_str()))
}

fn run(sema: &Sema<'_>, out: &mut Vec<Diagnostic>) {
    for token in sema.root.descendant_tokens() {
        let Some(state) = state_ref(sema, token) else { continue };
        if !is_unknown(sema, &state) {
            continue;
        }
        let hint = match closest(sema, &state).first() {
            Some(name) => format!(": did you mean `{name}`?"),
            None => String::new(),
        };
        out.push(Diagnostic::warning(
            token.text_range(),
            CODE,
            format!("no state `{}` in `{}`{hint}", state.name, state.script),
        ));
    }
}

fn fix(ctx: &AssistCtx<'_>, acc: &mut Vec<Assist>) {
    let Some(state) = ctx.token().and_then(|t| state_ref(ctx.sema, t)) else { return };
    if !is_unknown(ctx.sema, &state) {
        return;
    }
    for name in closest(ctx.sema, &state) {
        acc.push(Assist {
            id: "fix_state_name",
            label: format!("Change to `{name}`"),
            kind: AssistKind::QuickFix,
            target: state.token.text_range(),
            edits: vec![TextEdit { range: state.name_range, new_text: name.to_owned() }],
            fixes: Some(CODE),
            other_files: Vec::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use papyrus_ide::assists::test_utils::{check_assist, check_assist_not_applicable};

    use super::fix;
    use crate::test_utils::check;

    const DECLS: &str = "ScriptName T\nState Busy\nEndState\nState Idle\nEndState\n";

    #[test]
    fn reports_states_declared_nowhere() {
        let src = format!(
            "{DECLS}Function F(T akOther, String asName)\n    GoToState(\"Bussy\")\n    GoToState(\"busy\")\n    GoToState(\"\")\n    GoToState(\"Nothing Like It\")\n    GoToState(asName)\n    GoToState(\"a\" + asName)\n    akOther.GoToState(\"Idel\")\n    GoToState(asNewState = \"Idel\")\n    Debug.Trace(\"Bussy\")\nEndFunction\n"
        );
        assert_eq!(
            check("unknown-state", &src),
            [
                "7: no state `Bussy` in `T`: did you mean `Busy`?",
                "10: no state `Nothing Like It` in `T`",
                "13: no state `Idel` in `T`: did you mean `Idle`?",
                "14: no state `Idel` in `T`: did you mean `Idle`?",
            ]
        );
    }

    #[test]
    fn names_compared_with_get_state_are_flags() {
        let src = format!(
            "{DECLS}Function F()\n    GoToState(\"Ready\")\n    GoToState(\"Other\")\n    GoToState(\"Left\")\n    If GetState() == \"ready\" || \"LEFT\" != GetState()\n    EndIf\n    If \"Other\" == \"Other\"\n    EndIf\nEndFunction\n"
        );
        assert_eq!(check("unknown-state", &src), ["8: no state `Other` in `T`"]);
    }

    #[test]
    fn a_flag_may_be_close_to_a_state() {
        let src = format!(
            "{DECLS}Function F()\n    GoToState(\"Bussy\")\n    If GetState() == \"Bussy\"\n    EndIf\nEndFunction\n"
        );
        assert_eq!(check("unknown-state", &src), [""; 0]);
    }

    #[test]
    fn comparisons_with_get_state_name_states_too() {
        let src = format!(
            "{DECLS}Function F(T akOther)\n    If GetState() == \"Bussy\"\n    EndIf\n    If \"Idel\" != akOther.GetState()\n    EndIf\n    If GetState() == \"Busy\" || GetState() == \"\"\n    EndIf\n    If GetState() < \"Bussy\"\n    EndIf\nEndFunction\n"
        );
        assert_eq!(
            check("unknown-state", &src),
            [
                "7: no state `Bussy` in `T`: did you mean `Busy`?",
                "9: no state `Idel` in `T`: did you mean `Idle`?",
            ]
        );
    }

    #[test]
    fn lint_fix_leaves_the_guess_to_the_author() {
        let src = format!("{DECLS}Function F()\n    GoToState(\"Bussy\")\nEndFunction\n");
        let mut ws = papyrus_ide::Workspace::new();
        let file = ws.set_open("/ws/T.psc".into(), src.as_str().into(), 0);
        let config = crate::LintConfig::default();
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, false);
        assert_eq!((fixed.as_str(), applied), (src.as_str(), 0));

        // On demand; not when two names are as close.
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, true);
        assert_eq!(applied, 1);
        assert!(fixed.contains("GoToState(\"Busy\")"), "{fixed}");
        let ties = "ScriptName T\nState Bat\nEndState\nState Bar\nEndState\nFunction F()\n    GoToState(\"Baz\")\nEndFunction\n";
        let file = ws.set_open("/ws/T2.psc".into(), ties.into(), 0);
        let (fixed, applied) = crate::fix_all(&mut ws, file, &config, true);
        assert_eq!((fixed.as_str(), applied), (ties, 0));
    }

    #[test]
    fn fix_changes_the_name_only() {
        check_assist(
            fix,
            &format!("{DECLS}Function F()\n    GoToState(\"$0Bussy\")\nEndFunction\n"),
            &format!("{DECLS}Function F()\n    GoToState(\"Busy\")\nEndFunction\n"),
        );
        check_assist_not_applicable(
            fix,
            &format!("{DECLS}Function F()\n    GoToState(\"$0Busy\")\nEndFunction\n"),
        );
        check_assist_not_applicable(
            fix,
            &format!("{DECLS}Function F()\n    GoToState(\"$0Nothing Like It\")\nEndFunction\n"),
        );
    }
}
