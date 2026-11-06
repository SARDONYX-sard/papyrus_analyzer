use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{features::completion::Completions, *};

/// Builds a workspace from `(file name, text)` pairs, all opened in the editor.
/// The cursor of the first file containing `$0` is returned.
fn workspace(files: &[(&str, &str)]) -> (Workspace, FileId, TextSize) {
    let mut ws = Workspace::new();
    let mut cursor = None;
    for (name, text) in files {
        let path = PathBuf::from(format!("/ws/{name}"));
        let (text, offset) = match text.find("$0") {
            Some(i) => (text.replacen("$0", "", 1), Some(i)),
            None => ((*text).to_owned(), None),
        };
        let id = ws.set_open(path, Arc::from(text), 0);
        if let Some(offset) = offset {
            cursor = Some((id, TextSize::new(offset as u32)));
        }
    }
    let (file, offset) = cursor.expect("no $0 cursor");
    (ws, file, offset)
}

#[test]
fn hover_tells_whether_a_local_may_be_none() {
    let hover_at = |body: &str| {
        let src = format!(
            "ScriptName T\nFunction F(Form f)\n    Actor a = f as Actor\n{body}EndFunction\n"
        );
        let (ws, file, offset) =
            workspace(&[("Form.psc", FORM), ("Actor.psc", ACTOR), ("T.psc", &src)]);
        hover(&ws, file, offset).unwrap().markdown
    };
    let maybe = hover_at("    a$0.Kill()\n");
    assert!(
        maybe.contains("comes from [here](file:///ws/T.psc#L3,15)")
            && maybe.contains("`a` may be None here"),
        "{maybe}"
    );
    let checked = hover_at("    If a\n        a$0.Kill()\n    EndIf\n");
    assert!(checked.contains("`a` is not None here."), "{checked}");
    let after_return = hover_at("    If !a\n        Return\n    EndIf\n    a$0.Kill()\n");
    assert!(after_return.contains("`a` is not None here."), "{after_return}");
}

#[test]
fn check_none_binds_a_cast_in_parentheses() {
    let src = "ScriptName T\nFunction F(Form f)\n    (f as Actor).Kill$0()\nEndFunction\n";
    let (ws, file, offset) = workspace(&[("Form.psc", FORM), ("Actor.psc", ACTOR), ("T.psc", src)]);
    let found = assists::assists(&ws, file, TextRange::empty(offset));
    let fix = found.iter().find(|a| a.id == "check_none").expect("check_none applies");
    assert_eq!(
        fix.edits[0].new_text,
        "Actor akActor = f as Actor\n    If akActor\n        akActor.Kill()\n    EndIf"
    );
}

#[test]
fn attributes_keep_the_grammar_colors() {
    let src = "$0; #![allow(unused_local)] why\nScriptName T\n";
    let (ws, file, _) = workspace(&[("T.psc", src)]);
    let first = semantic_tokens(&ws, file, None).into_iter().next().unwrap();
    assert_ne!(first.ty, TokenType::Comment, "the attribute has no semantic token");
}

#[test]
fn hover_parent_tells_the_script_and_function() {
    let src = "ScriptName T extends Actor\nEvent OnDeath(Actor akKiller)\n    Par$0ent.OnDeath(akKiller)\nEndEvent\n";
    let (ws, file, offset) = workspace(&[("Form.psc", FORM), ("Actor.psc", ACTOR), ("T.psc", src)]);
    let md = hover(&ws, file, offset).unwrap().markdown;
    assert!(md.starts_with("`Parent` is the parent script:"), "{md}");
    assert!(
        md.contains("ScriptName Actor")
            && md.contains("Calls:\n\n```papyrus\nEvent OnDeath(Actor akKiller)"),
        "{md}"
    );
}

#[test]
fn hover_operators_with_their_types() {
    let src = "ScriptName T\nFunction F(Int i, Float f, String s)\n    f = i $0+ f\nEndFunction\n";
    let (ws, file, offset) = workspace(&[("T.psc", src)]);
    let md = hover(&ws, file, offset).unwrap().markdown;
    assert!(md.contains("*operator*") && md.contains("Here: `Int` + `Float` → `Float`"), "{md}");
    let src = "ScriptName T\nFunction F(Int i)\n    Bool b = i $0== 3\nEndFunction\n";
    let (ws, file, offset) = workspace(&[("T.psc", src)]);
    assert!(hover(&ws, file, offset).unwrap().markdown.contains("ignoring case"));
}

#[test]
fn name_spellings_follow_definitions() {
    let (ws, file, _) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Debug.psc", DEBUG),
        (
            "Foo.psc",
            "$0scriptname foo extends actor hidden\nint myValue\nevent ondeath(actor killer)\nmyvalue = getformid()\ndebug.trace(\"x\")\nendevent\n",
        ),
    ]);
    let text = ws.text(file).unwrap();
    let got: Vec<(String, NameKind, String)> = name_spellings(&ws, file)
        .into_iter()
        .map(|n| (text[n.range].to_owned(), n.kind, n.spelling))
        .collect();
    let has = |written: &str, kind, spelling: &str| {
        assert!(
            got.iter().any(|g| g.0 == written && g.1 == kind && g.2 == spelling),
            "{written}: {got:?}"
        );
    };
    has("actor", NameKind::Script, "Actor");
    has("hidden", NameKind::Flag, "Hidden");
    // An event overriding one of a parent script takes its spelling.
    has("ondeath", NameKind::Function, "OnDeath");
    has("myvalue", NameKind::Variable, "myValue");
    has("getformid", NameKind::Function, "GetFormID");
    has("debug", NameKind::Script, "Debug");
    has("trace", NameKind::Function, "Trace");
}

fn script_of(file: NavFile) -> FileId {
    match file {
        NavFile::Script(f) => f,
        NavFile::Flags => panic!("expected a script"),
    }
}

const FORM: &str = "ScriptName Form Hidden\nInt Function GetFormID() Native\n";
const ACTOR: &str = "ScriptName Actor extends Form Hidden\n; Kills the actor.\nFunction Kill(Actor akKiller = None) Native\nFloat Function GetAV(String asName) Native\nEvent OnDeath(Actor akKiller)\nEndEvent\n";
const DEBUG: &str =
    "ScriptName Debug Hidden\nFunction Trace(String asText, Int aiSeverity = 0) Global Native\n";

#[test]
fn hover_member_with_docs() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Test.psc", "ScriptName Test\nFunction F(Actor a)\n  a.Ki$0ll()\nEndFunction\n"),
    ]);
    let hover = hover(&ws, file, offset).unwrap();
    assert_eq!(
        hover.markdown,
        "```papyrus\nFunction Kill(Actor akKiller = None) Native\n```\n*Actor*\n\n---\n\nKills the actor."
    );
}

#[test]
fn hover_local_and_global() {
    let (ws, file, offset) = workspace(&[
        ("Debug.psc", DEBUG),
        (
            "Test.psc",
            "ScriptName Test\nFunction F()\n  Int count = 1\n  Debug.Trace(\"x\" + cou$0nt)\nEndFunction\n",
        ),
    ]);
    assert_eq!(
        hover(&ws, file, offset).unwrap().markdown,
        "```papyrus\nInt count\n```\n*local variable*"
    );

    let (ws, file, offset) = workspace(&[
        ("Debug.psc", DEBUG),
        ("Test.psc", "ScriptName Test\nFunction F()\n  Debug.Tr$0ace(\"x\")\nEndFunction\n"),
    ]);
    assert!(
        hover(&ws, file, offset)
            .unwrap()
            .markdown
            .contains("Function Trace(String asText, Int aiSeverity = 0) Global Native")
    );
}

#[test]
fn goto_inherited_and_imported() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Debug.psc", DEBUG),
        (
            "Test.psc",
            "ScriptName Test extends Actor\nImport Debug\nEvent OnInit()\n  GetForm$0ID()\n  Trace(\"\")\nEndEvent\n",
        ),
    ]);
    let targets = goto_definition(&ws, file, offset);
    assert_eq!(targets.len(), 1);
    assert_eq!(ws.file(script_of(targets[0].file)).path, Path::new("/ws/Form.psc"));

    let text = ws.text(file).unwrap();
    let trace = TextSize::new(text.find("Trace").unwrap() as u32 + 1);
    let targets = goto_definition(&ws, file, trace);
    assert_eq!(ws.file(script_of(targets[0].file)).path, Path::new("/ws/Debug.psc"));
}

#[test]
fn goto_property_through_cast_and_index() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        (
            "Test.psc",
            "ScriptName Test\nForm[] forms\nFunction F()\n  (forms[0] as Actor).Kil$0l()\nEndFunction\n",
        ),
    ]);
    let targets = goto_definition(&ws, file, offset);
    assert_eq!(ws.file(script_of(targets[0].file)).path, Path::new("/ws/Actor.psc"));
}

#[test]
fn locals_shadowing_and_scope() {
    let src = "ScriptName Test\nInt x\nFunction F(Int x)\n  If True\n    Int y = 1\n  EndIf\n  x$0 = y\nEndFunction\n";
    let (ws, file, offset) = workspace(&[("Test.psc", src)]);
    let targets = goto_definition(&ws, file, offset);
    // The parameter shadows the script variable.
    assert_eq!(targets[0].focus_range, TextRange::at(TextSize::new(37), TextSize::new(1)));
    // `y` is out of scope after `EndIf`.
    let diags: Vec<_> = diagnostics(&ws, file).into_iter().map(|d| d.code).collect();
    assert!(!diags.contains(&"syntax"));
}

#[test]
fn references_across_files() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("A.psc", "ScriptName A\nFunction F(Actor a)\n  a.Kill()\n  a.Kill(a)\nEndFunction\n"),
        ("B.psc", "ScriptName B extends Actor\nEvent OnInit()\n  Ki$0ll()\nEndEvent\n"),
    ]);
    let refs = references(&ws, file, offset, true);
    assert_eq!(refs.len(), 4, "{refs:?}");
    let refs = references(&ws, file, offset, false);
    assert_eq!(refs.len(), 3, "{refs:?}");
}

fn labels(c: &Completions) -> Vec<&str> {
    c.items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn complete_members() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Test.psc", "ScriptName Test\nFunction F(Actor a)\n  a.$0\nEndFunction\n"),
    ]);
    let c = crate::features::completion::completions(&ws, file, offset);
    let l = labels(&c);
    assert!(l.contains(&"Kill") && l.contains(&"GetAV") && l.contains(&"GetFormID"), "{l:?}");
    assert!(l.contains(&"GoToState"));
}

#[test]
fn complete_static_and_arrays() {
    let (ws, file, offset) = workspace(&[
        ("Debug.psc", DEBUG),
        ("Test.psc", "ScriptName Test\nFunction F(Int[] arr)\n  Debug.T$0\n  arr.\nEndFunction\n"),
    ]);
    let c = crate::features::completion::completions(&ws, file, offset);
    assert_eq!(labels(&c), ["Trace"]);
    assert_eq!(c.items[0].replace_range, TextRange::new(46.into(), 47.into()));

    let text = ws.text(file).unwrap();
    let after_dot = TextSize::new(text.find("arr.").unwrap() as u32 + 4);
    let c = crate::features::completion::completions(&ws, file, after_dot);
    assert_eq!(labels(&c), ["Length", "Find", "RFind"]);
}

#[test]
fn complete_locals_in_body() {
    let (ws, file, offset) = workspace(&[(
        "Test.psc",
        "ScriptName Test\nInt Property P Auto\nFunction F(Int param)\n  Int local = 1\n  $0\nEndFunction\n",
    )]);
    let c = crate::features::completion::completions(&ws, file, offset);
    let l = labels(&c);
    for expected in ["param", "local", "P", "F", "If", "While", "Int"] {
        assert!(l.contains(&expected), "missing {expected}: {l:?}");
    }
    assert!(!l.contains(&"EndState"));
}

#[test]
fn complete_event_overrides() {
    let (ws, file, offset) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Test.psc", "ScriptName Test extends Actor\nEvent $0\n"),
    ]);
    let c = crate::features::completion::completions(&ws, file, offset);
    let item = c.items.iter().find(|i| i.label == "OnDeath(Actor akKiller)").unwrap();
    assert_eq!(item.snippet.as_deref(), Some("OnDeath(Actor akKiller)\n\t$0\nEndEvent"));
}

#[test]
fn signature_help_active_param() {
    let (ws, file, offset) = workspace(&[
        ("Debug.psc", DEBUG),
        ("Test.psc", "ScriptName Test\nFunction F()\n  Debug.Trace(\"x\", $0)\nEndFunction\n"),
    ]);
    let help = signature_help(&ws, file, offset).unwrap();
    assert_eq!(help.label, "Trace(String asText, Int aiSeverity = 0)");
    assert_eq!(help.active_param, Some(1));
    assert_eq!(help.params, [(6, 19), (21, 39)]);
}

#[test]
fn fragment_markers_have_hovers_and_grammar_colors() {
    let src = ";BEGIN FRAGMENT CODE - Do not edit\n;NEXT FRAGMENT INDEX 1\nScriptName T\n;BEGIN FRAGMENT Fragment_0\nFunction Fragment_0()\n;BEGIN CODE\n;just a comment\n;END CODE\nEndFunction\n;END FRAGMENT\n;END FRAGMENT CODE - Do not edit\n;END CODE outside\n";
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src), 0);
    let hover_at = |needle: &str, delta: usize| {
        let offset = src.find(needle).unwrap() + delta;
        hover(&ws, file, TextSize::new(offset as u32))
            .map(|h| (src[h.range].to_owned(), h.markdown))
    };
    let (word, md) = hover_at("BEGIN FRAGMENT CODE", 3).unwrap();
    assert_eq!(word, "BEGIN FRAGMENT CODE");
    assert!(md.contains("Creation Kit fragment") && md.contains("Example:"), "{md}");
    assert_eq!(hover_at("BEGIN FRAGMENT Fragment_0", 3).unwrap().0, "BEGIN FRAGMENT");
    assert_eq!(hover_at(";END FRAGMENT\n", 3).unwrap().0, "END FRAGMENT");
    assert_eq!(hover_at("INDEX 1", 2).unwrap().0, "NEXT FRAGMENT INDEX");
    // The text after the marker, other comments, outside of the fragment.
    assert!(hover_at("Do not edit", 3).is_none());
    assert!(hover_at("just a comment", 3).is_none());
    assert!(hover_at("END CODE outside", 3).is_none());

    let comments: Vec<&str> = semantic_tokens(&ws, file, None)
        .into_iter()
        .filter(|t| t.ty == TokenType::Comment)
        .map(|t| src[t.range].trim_end())
        .collect();
    assert_eq!(comments, [";just a comment", ";END CODE outside"]);
}

#[test]
fn assists_leave_fragments_alone() {
    let fragment = ";BEGIN FRAGMENT CODE\nScriptName T\nFunction Fragment_0(Bool a, Bool b)\nIf !(a && b)\nEndIf\nEndFunction\n;END FRAGMENT CODE\n";
    let src =
        format!("{fragment}Function G(Bool a, Bool b)\n    If !(a && b)\n    EndIf\nEndFunction\n");
    let mut ws = Workspace::new();
    let file = ws.set_open(PathBuf::from("/ws/T.psc"), Arc::from(src.as_str()), 0);
    let at = |from: usize| {
        let start = src[from..].find("&&").unwrap() + from;
        TextRange::empty(TextSize::new(start as u32))
    };
    let demorgan =
        |range| assists::assists(&ws, file, range).iter().any(|a| a.id == "apply_demorgan");
    assert!(!demorgan(at(0)));
    assert!(demorgan(at(fragment.len())));
}

#[test]
fn builtin_scripts_are_seen_by_tests_only() {
    let builtin = "ScriptName PapyrusAnalyzer\nFunction AssertTrue(Bool b) Global Native\n";
    let uses = |name: &str| {
        format!(
            "ScriptName {name}\nImport PapyrusAnalyzer\nFunction F()\n    PapyrusAnalyzer.AssertTrue(True)\n    AssertTrue(True)\nEndFunction\n"
        )
    };
    let mut ws = Workspace::new();
    ws.set_test_dirs(&[PathBuf::from("/ws/tests")]);
    // Semantic checks need the base scripts.
    ws.set_open(PathBuf::from("/ws/src/Form.psc"), Arc::from("ScriptName Form\n"), 0);
    ws.set_disk_index(
        PathBuf::from("/tmp/PapyrusAnalyzer.psc"),
        index_text(builtin),
        Origin::Builtin,
    );
    let test = ws.set_open(PathBuf::from("/ws/tests/T.psc"), Arc::from(uses("T")), 0);
    let src = ws.set_open(PathBuf::from("/ws/src/S.psc"), Arc::from(uses("S")), 0);
    let messages =
        |file| -> Vec<String> { diagnostics(&ws, file).into_iter().map(|d| d.message).collect() };
    assert_eq!(messages(test), Vec::<String>::new());
    assert!(
        messages(src).iter().any(|m| m.contains("only available in the [test] sources")),
        "{:?}",
        messages(src)
    );

    // A script of the project wins over the built-in one.
    let own = "ScriptName PapyrusAnalyzer\nFunction AssertTrue(Bool b) Global\nEndFunction\n";
    let own = ws.set_open(PathBuf::from("/ws/src/PapyrusAnalyzer.psc"), Arc::from(own), 0);
    assert_eq!(ws.script_near(papyrus_intern::Name::new("PapyrusAnalyzer"), test), Some(own));
}

#[test]
fn diagnostics_semantic() {
    let (ws, file, _) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        (
            "Wrong.psc",
            "ScriptName Test$0 extends Actor\nInt x\nInt x\nFunction F() Gloabl\n  Kill()\n  Fly()\n  Self.Swim()\n  Missing m\n  GoToState(\"Nowhere\")\nEndFunction\nState S\n  Function OnlyHere()\n  EndFunction\nEndState\n",
        ),
    ]);
    let diags: Vec<String> =
        diagnostics(&ws, file).into_iter().map(|d| format!("{}: {}", d.code, d.message)).collect();
    assert_eq!(
        diags,
        [
            "script-name-mismatch: script name `Test` does not match the file name `Wrong.psc`",
            "duplicate-definition: `x` is already defined in this scope",
            "unknown-flag: unknown flag `Gloabl`, not defined in built-in Skyrim flags",
            "unresolved-name: cannot find `Fly` in this scope",
            "unknown-member: `Test` has no member `Swim`",
            "unknown-type: unknown type `Missing`",
            "state-function-not-declared: function `OnlyHere` in state `S` must also be declared outside of any state",
        ]
    );
}

#[test]
fn parameter_inlay_hints() {
    let (ws, file, _) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Debug.psc", DEBUG),
        (
            "Test.psc",
            "ScriptName Test$0\nFunction F(Actor killer, Int[] arr)\n  killer.Kill(killer)\n  killer.Kill(None)\n  Debug.Trace(\"x\", aiSeverity = 1)\n  Debug.Trace(\"x\", 2)\n  arr.Find(3)\nEndFunction\n",
        ),
    ]);
    let text = ws.text(file).unwrap();
    let hints: Vec<String> = inlay_hints(&ws, file, None)
        .into_iter()
        .map(|h| format!("{} {}", h.label, &text[usize::from(h.offset)..usize::from(h.offset) + 4]))
        .collect();
    // `killer` repeats `akKiller`; named arguments get no hint.
    assert_eq!(
        hints,
        [
            "akKiller: None",
            "asText: \"x\",",
            "asText: \"x\",",
            "aiSeverity: 2)\n ",
            "akElement: 3)\nE"
        ]
    );
}

#[test]
fn type_errors() {
    let (ws, file, _) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Debug.psc", DEBUG),
        (
            "Test.psc",
            r#"ScriptName Test$0
Int Property RO = 1 AutoReadOnly
Int Property Full
    Int Function Get()
        Return 1
    EndFunction
EndProperty
Int bad = 1.5
Int Function F(Actor a, Form f, Int[] arr)
    Int i = 1.5
    Float ok = 1
    String s = a
    Actor down = f
    Actor fine = f as Actor
    i = "x" * 2
    i = arr[1.0]
    i = i[0]
    a.Kill(1, 2)
    a.GetAV()
    Debug.Trace("x", aiSeverity = "high")
    Actor.Kill()
    RO = 2
    Full = 3
    Return
EndFunction
Function G()
    Return 1
EndFunction
"#,
        ),
    ]);
    let diags: Vec<String> = diagnostics(&ws, file)
        .into_iter()
        .filter(|d| d.needs_workspace())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert_eq!(
        diags,
        [
            "type-mismatch: expected `Int`, found `Float`",
            "type-mismatch: expected `Actor`, found `Form`",
            "invalid-operator: cannot apply `*` to `String` and `Int`",
            "type-mismatch: expected `Int`, found `Float`",
            "not-an-array: `Int` is not an array",
            "type-mismatch: expected `Actor`, found `Int`",
            "wrong-arg-count: `Kill` takes 1 arguments, but more were given",
            "wrong-arg-count: missing arguments for `GetAV`: asName",
            "type-mismatch: expected `Int`, found `String`",
            "not-global: `Kill` is not a Global function and needs an object, not the script `Actor`",
            "read-only: property `RO` is read-only",
            "read-only: property `Full` is read-only",
            "missing-return-value: this function must return a `Int`",
            "unexpected-return-value: this function does not return a value",
        ],
        "{diags:#?}"
    );
}

#[test]
fn goto_declaration_of_overrides() {
    let base = "ScriptName Base\nFunction Run()\nEndFunction\n";
    let mid = "ScriptName Mid extends Base\n";
    let check = |child: &str, expected_file: &str, expected: &str| {
        let (ws, file, offset) =
            workspace(&[("Base.psc", base), ("Mid.psc", mid), ("Child.psc", child)]);
        let targets = crate::goto_declaration(&ws, file, offset);
        assert_eq!(targets.len(), 1, "{child}");
        let target = script_of(targets[0].file);
        let path = &ws.file(target).path;
        assert!(path.ends_with(expected_file), "{path:?}");
        let text = ws.text(target).unwrap();
        let range = targets[0].focus_range;
        assert_eq!(&text[usize::from(range.start())..usize::from(range.end())], expected);
    };
    // The override, through a script that does not redefine it.
    check("ScriptName Child extends Mid\nFunction R$0un()\nEndFunction\n", "Base.psc", "Run");
    // A call of the override.
    check(
        "ScriptName Child extends Mid\nFunction Run()\nEndFunction\nFunction G()\n  R$0un()\nEndFunction\n",
        "Base.psc",
        "Run",
    );
    // A state function: the one outside of states first.
    check(
        "ScriptName Child extends Mid\nFunction Run()\nEndFunction\nState S\n  Function R$0un()\n  EndFunction\nEndState\n",
        "Child.psc",
        "Run",
    );
}

#[test]
fn overrides_and_overridden() {
    let base = "ScriptName Base\nEvent OnInit() Native\nFunction Run()\nEndFunction\nFunction Own()\nEndFunction\n";
    let child = "ScriptName Child extends Base\n$0Event OnInit()\nEndEvent\nFunction Run() Global\nEndFunction\nFunction Mine()\nEndFunction\n";
    let (ws, file, _) = workspace(&[("Base.psc", base), ("Child.psc", child)]);
    let found: Vec<(String, crate::OverrideKind)> = crate::overrides(&ws, file)
        .into_iter()
        .map(|o| (o.base_script.to_string(), o.kind))
        .collect();
    assert_eq!(
        found,
        [
            ("Base".to_owned(), crate::OverrideKind::EngineEvent),
            ("Base".to_owned(), crate::OverrideKind::Override)
        ]
    );
    // The hint goes after the flags of the header line.
    let o = &crate::overrides(&ws, file)[1];
    let text = ws.text(file).unwrap();
    assert_eq!(
        &text[..usize::from(o.hint_offset)].lines().last().unwrap(),
        &"Function Run() Global"
    );

    let base_file = ws.file_id(std::path::Path::new("/ws/Base.psc")).unwrap();
    let overridden = crate::overridden(&ws, base_file);
    let base_text = ws.text(base_file).unwrap();
    let names: Vec<&str> = overridden
        .iter()
        .map(|o| &base_text[usize::from(o.name_range.start())..usize::from(o.name_range.end())])
        .collect();
    assert_eq!(names, ["OnInit", "Run"]);
    assert_eq!(overridden[1].by.len(), 1);
}

/// Applies a rename and returns the new texts by file name.
fn renamed(files: &[(&str, &str)], new_name: &str) -> Result<Vec<(String, String)>, String> {
    let (ws, file, offset) = workspace(files);
    let edit = crate::rename(&ws, file, offset, new_name)?;
    let mut out = Vec::new();
    for (name, _) in files {
        let id = ws.file_id(std::path::Path::new(&format!("/ws/{name}"))).unwrap();
        let mut text = ws.text(id).unwrap().to_string();
        let mut ranges: Vec<_> = edit
            .ranges
            .iter()
            .filter(|(f, _)| *f == NavFile::Script(id))
            .map(|(_, r)| *r)
            .collect();
        ranges.sort_by_key(|r| std::cmp::Reverse(r.start()));
        for r in ranges {
            text.replace_range(usize::from(r.start())..usize::from(r.end()), new_name);
        }
        let renamed_to = edit
            .file_renames
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned());
        out.push((renamed_to.unwrap_or_else(|| (*name).to_owned()), text));
    }
    Ok(out)
}

#[test]
fn rename_functions_with_their_overrides() {
    let base =
        "ScriptName Base\nFunction Run()\nEndFunction\nFunction Go()\n  Run()\nEndFunction\n";
    let child = "ScriptName Child extends Base\nFunction R$0un()\n  Parent.Run()\nEndFunction\nState S\n  Function Run()\n  EndFunction\nEndState\n";
    let out = renamed(&[("Base.psc", base), ("Child.psc", child)], "Start").unwrap();
    assert_eq!(
        out[0].1,
        "ScriptName Base\nFunction Start()\nEndFunction\nFunction Go()\n  Start()\nEndFunction\n"
    );
    assert_eq!(
        out[1].1,
        "ScriptName Child extends Base\nFunction Start()\n  Parent.Start()\nEndFunction\nState S\n  Function Start()\n  EndFunction\nEndState\n"
    );
}

#[test]
fn rename_parameters_and_named_arguments() {
    let lib = "ScriptName Lib\nFunction Hit(Actor actor, Int amount) Global\n  amount += 1\nEndFunction\n";
    let user = "ScriptName User\nFunction F()\n  Lib.Hit(None, amo$0unt = 2)\nEndFunction\n";
    let out = renamed(&[("Lib.psc", lib), ("User.psc", user)], "damage").unwrap();
    assert_eq!(
        out[0].1,
        "ScriptName Lib\nFunction Hit(Actor actor, Int damage) Global\n  damage += 1\nEndFunction\n"
    );
    assert_eq!(
        out[1].1,
        "ScriptName User\nFunction F()\n  Lib.Hit(None, damage = 2)\nEndFunction\n"
    );
}

#[test]
fn rename_scripts_with_their_file() {
    let base = "ScriptName B$0ase\n";
    let child = "ScriptName Child extends Base\nBase Property Other Auto\n";
    let out = renamed(&[("Base.psc", base), ("Child.psc", child)], "Root").unwrap();
    assert_eq!(out[0], ("Root.psc".to_owned(), "ScriptName Root\n".to_owned()));
    assert_eq!(out[1].1, "ScriptName Child extends Root\nRoot Property Other Auto\n");
}

#[test]
fn rename_refusals() {
    let form = "ScriptName Form\nEvent OnInit() Native\n";
    let child = "ScriptName Child extends Form\nEvent OnI$0nit()\nEndEvent\n";
    assert!(
        renamed(&[("Form.psc", form), ("Child.psc", child)], "Start")
            .unwrap_err()
            .contains("sent by the game")
    );
    let local = "ScriptName T\nFunction F()\n  Int $0x = 1\nEndFunction\n";
    assert!(renamed(&[("T.psc", local)], "While").unwrap_err().contains("keyword"));
    assert!(renamed(&[("T.psc", local)], "1x").unwrap_err().contains("not a valid name"));
    assert_eq!(
        renamed(&[("T.psc", local)], "y").unwrap()[0].1,
        "ScriptName T\nFunction F()\n  Int y = 1\nEndFunction\n"
    );
}

#[test]
fn snippets_by_context() {
    let has = |src: &str, label: &str| {
        let (ws, file, offset) = workspace(&[("T.psc", src)]);
        let c = crate::features::completion::completions(&ws, file, offset);
        c.items.iter().any(|i| i.label == label && i.kind == crate::CompletionKind::Snippet)
    };
    // Statements in bodies, declarations at script level and in states.
    assert!(has("ScriptName T\nFunction F()\n    wh$0\nEndFunction\n", "while"));
    assert!(!has("ScriptName T\nFunction F()\n    wh$0\nEndFunction\n", "function"));
    assert!(has("ScriptName T\nfu$0\n", "function"));
    assert!(has("ScriptName T\nState S\n    ev$0\nEndState\n", "event"));
    // Not inside expressions.
    assert!(!has("ScriptName T\nFunction F(Int x)\n    x = wh$0\nEndFunction\n", "while"));
}

#[test]
fn rename_only_workspace_scripts() {
    let mut ws = Workspace::new();
    let lib = "ScriptName Lib\nFunction Hit() Global\nEndFunction\n";
    ws.set_disk_index(PathBuf::from("/game/Lib.psc"), index_text(lib), Origin::Import(0));
    let base = "ScriptName Base\nFunction Run()\nEndFunction\n";
    ws.set_disk_index(PathBuf::from("/game/Base.psc"), index_text(base), Origin::Import(0));
    let src = "ScriptName User extends Base\nFunction Run()\n  Lib.H$0it()\nEndFunction\n";
    let offset = TextSize::new(src.find("$0").unwrap() as u32);
    let file =
        ws.set_open(PathBuf::from("/ws/User.psc"), Arc::from(src.replace("$0", "").as_str()), 0);
    let err = crate::rename(&ws, file, offset, "Strike").unwrap_err();
    assert!(err.contains("imported script"), "{err}");
    // An override of a function of an imported script.
    let run = TextSize::new(src.find("Run").unwrap() as u32);
    let err = crate::rename(&ws, file, run, "Go").unwrap_err();
    assert!(err.contains("imported script"), "{err}");
}

#[test]
fn scopes_limit_visible_scripts() {
    let mut ws = Workspace::new();
    let add = |ws: &mut Workspace, path: &str, text: &str| {
        ws.set_disk_index(PathBuf::from(path), index_text(text), Origin::Workspace)
    };
    let mcm = add(&mut ws, "/ws/Helper/MCM.psc", "ScriptName MCM\n");
    add(&mut ws, "/ws/Other/Util.psc", "ScriptName Util\n");
    let app = add(&mut ws, "/ws/App/App.psc", "ScriptName App\nInt MCM\n");
    ws.set_scopes(&[
        Scope {
            sources: vec!["/ws/App".into()],
            visible: vec!["/ws/App".into(), "/ws/Helper".into()],
        },
        Scope { sources: vec!["/ws/Other".into()], visible: vec!["/ws/Other".into()] },
        Scope { sources: vec!["/ws/Helper".into()], visible: vec!["/ws/Helper".into()] },
    ]);
    let util = papyrus_intern::Name::new("Util");
    assert_eq!(ws.script_near(papyrus_intern::Name::new("MCM"), app), Some(mcm));
    assert_eq!(ws.script_near(util, app), None);
    assert!(ws.script_near(util, mcm).is_none());
    // Unscoped lookups see everything.
    assert!(ws.script(util).is_some());
    ws.set_scopes(&[]);
    assert!(ws.script_near(util, app).is_some());
}

/// Rules found by comparing with the official compiler.
#[test]
fn compiler_rules() {
    let (ws, file, _) = workspace(&[
        ("Form.psc", FORM),
        ("Actor.psc", ACTOR),
        ("Debug.psc", DEBUG),
        (
            "Test.psc",
            r#"ScriptName Test$0
Int speed
Int Property Speed Auto
Int Property Length Auto
Float Property F1 = 1 Auto
Float Property F2 = 1 AutoReadOnly
String s1 = None
Int Property P = 1 + 1 Auto
Int v = 1 Bogus
Function Fn(Int speed, Bool b) Conditional
    Int b = 1
    Int Debug = 2
    Int x = 1
    If b
        Int x = 2
        Int y = 1
    Else
        Int y = 2
    EndIf
    Int[] arr = new Int[0]
    arr = new Int[x]
EndFunction
Function G(Actor a, Debug d) Global
    Self.Fn(1, True)
    Bool same = a == d
EndFunction
String Function GetState()
    Return ""
EndFunction
"#,
        ),
    ]);
    let diags: Vec<String> =
        diagnostics(&ws, file).into_iter().map(|d| format!("{}: {}", d.code, d.message)).collect();
    assert_eq!(
        diags,
        [
            "reserved-name: `Length` is reserved for the length of arrays and cannot be a name",
            "initializer-mismatch: cannot initialize a `Float` with `1`: initial values are not converted",
            "initializer-mismatch: cannot initialize a `String` with `None`: initial values are not converted",
            "non-literal-initializer: initial values must be literals",
            "unknown-flag: unknown flag `Bogus`, not defined in built-in Skyrim flags",
            "name-conflict: `speed` is already defined as a variable of the script",
            "misplaced-flag: flag `Conditional` is not allowed on functions (allowed on: Script, Variable)",
            "name-conflict: `b` is already defined as a parameter",
            "name-is-type: `Debug` is the name of a script; variables and properties cannot use it",
            "name-conflict: `x` is already defined in an enclosing block, and locals cannot shadow it",
            "array-size: arrays must have between 1 and 128 elements",
            "array-size: the size of a new array must be an integer literal (1 to 128)",
            "global-self: `Self` does not exist in a Global function",
            "invalid-operator: cannot apply `==` to `Actor` and `Debug`",
            "builtin-redefined: `GetState` is generated by the compiler and cannot be defined",
        ],
        "{diags:#?}"
    );
}

#[test]
fn document_symbols_nest_states_and_accessors() {
    let (ws, file, _) = workspace(&[(
        "Test.psc",
        "ScriptName Test$0\nInt Property P\n  Int Function Get()\n    Return 1\n  EndFunction\nEndProperty\nState S\n  Event OnInit()\n  EndEvent\nEndState\n",
    )]);
    let symbols = document_symbols(&ws, file);
    assert_eq!(symbols.len(), 1);
    let children: Vec<_> =
        symbols[0].children.iter().map(|s| (s.name.as_str(), s.children.len())).collect();
    assert_eq!(children, [("P", 1), ("S", 1)]);
}

const FLAGS: &str = "// Hides things.\nFlag Hidden 0 { Script Property }\nFlag Conditional 1 { Script Variable }\nFlag Everywhere 5\n";

fn with_flags(files: &[(&str, &str)]) -> (Workspace, FileId, TextSize) {
    let (mut ws, file, offset) = workspace(files);
    ws.set_flags_file(PathBuf::from("/ws/Custom.flg"), Arc::from(FLAGS));
    (ws, file, offset)
}

#[test]
fn flag_completion_depends_on_declaration() {
    let complete = |src: &str| {
        let (ws, file, offset) = with_flags(&[("Test.psc", src)]);
        let c = crate::features::completion::completions(&ws, file, offset);
        let mut l: Vec<String> = c.items.iter().map(|i| i.label.clone()).collect();
        l.sort();
        l
    };
    assert_eq!(complete("ScriptName Test $0\n"), ["Conditional", "Everywhere", "Hidden", "Native"]);
    assert_eq!(
        complete("ScriptName Test\nFunction F() $0\nEndFunction\n"),
        ["Everywhere", "Global", "Native"]
    );
    assert_eq!(
        complete("ScriptName Test\nInt Property P = 1 Auto $0\n"),
        ["Conditional", "Everywhere", "Hidden"]
    );
    assert_eq!(complete("ScriptName Test\nInt x = 1 H$0\n"), ["Conditional", "Everywhere"]);
    assert_eq!(
        complete("ScriptName Test\nInt Property P $0\n"),
        ["Auto", "AutoReadOnly", "Everywhere", "Hidden"]
    );
}

#[test]
fn flag_hover_goto_and_references() {
    let (ws, file, offset) = with_flags(&[
        ("Test.psc", "ScriptName Test Hid$0den\nInt Property P Auto Hidden\n"),
        ("Other.psc", "ScriptName Other Hidden\n"),
    ]);
    let h = hover(&ws, file, offset).unwrap();
    assert_eq!(
        h.markdown,
        "```papyrus\nFlag Hidden 0 { Script Property }\n```\n*Custom.flg*\n\n---\n\nHides things."
    );
    let targets = goto_definition(&ws, file, offset);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].file, NavFile::Flags);
    assert_eq!(&FLAGS[targets[0].focus_range], "Hidden");

    let refs = references(&ws, file, offset, true);
    assert_eq!(refs.len(), 4, "{refs:?}");
    let from_flg = flag_references(&ws, targets[0].focus_range.start(), false);
    assert_eq!(from_flg.len(), 3);

    assert!(flag_file_hover(&ws, targets[0].focus_range.start()).is_some());
    assert_eq!(flag_file_symbols(&ws).len(), 3);
}

#[test]
fn keyword_hover_has_examples() {
    let (ws, file, offset) = workspace(&[(
        "Test.psc",
        "ScriptName Test\nFunction F()\n  Wh$0ile True\n  EndWhile\nEndFunction\n",
    )]);
    let h = hover(&ws, file, offset).unwrap();
    assert!(h.markdown.starts_with("```papyrus\nWhile\n```\n*keyword*"), "{}", h.markdown);
    assert!(h.markdown.contains("# Examples\n\n```papyrus\nInt i = 0"), "{}", h.markdown);

    let (ws, file, offset) = workspace(&[(
        "Test.psc",
        "ScriptName Test\nFunction F()\n  Se$0lf.Disable()\nEndFunction\n",
    )]);
    let h = hover(&ws, file, offset).unwrap();
    assert!(
        h.markdown.contains("Refers to:") && h.markdown.contains("ScriptName Test"),
        "{}",
        h.markdown
    );

    let (ws, _, _) = with_flags(&[("Test.psc", "ScriptName Test$0\n")]);
    let at = |needle: &str| TextSize::new(FLAGS.find(needle).unwrap() as u32 + 1);
    assert!(flag_file_hover(&ws, at("Flag Hidden")).unwrap().markdown.contains("bit index"));
    assert!(
        flag_file_hover(&ws, at("Property }")).unwrap().markdown.contains("property declarations")
    );
    assert!(
        flag_file_hover(&ws, at("Hidden 0"))
            .unwrap()
            .markdown
            .starts_with("```papyrus\nFlag Hidden 0")
    );
    assert!(flag_file_hover(&ws, at("Hides")).is_none(), "comments have no hover");
}

#[test]
fn flag_file_completion() {
    let (ws, _, _) = with_flags(&[("Test.psc", "ScriptName Test$0\n")]);
    let at = |needle: &str| TextSize::new((FLAGS.find(needle).unwrap() + needle.len()) as u32);
    let labels = |offset| -> Vec<String> {
        flag_file_completions(&ws, offset).items.into_iter().map(|i| i.label).collect()
    };
    // Inside `{ Script Property }` of Hidden: only the missing targets.
    assert_eq!(labels(at("{ Script ")), ["Variable", "Function"]);
    assert_eq!(labels(TextSize::of(FLAGS)), ["Flag", "Flag"]);
    let snippet = flag_file_completions(&ws, TextSize::of(FLAGS)).items[0].snippet.clone().unwrap();
    assert!(snippet.contains("${2:2}"), "next free index: {snippet}");
    assert!(labels(at("// Hid")).is_empty());
}

#[test]
fn misplaced_flags() {
    let (ws, file, _) = with_flags(&[(
        "Test.psc",
        "ScriptName Test$0\nFunction F() Hidden Global\nEndFunction\nInt x Hidden\nInt Property P Auto Conditional\nInt Property Q Global Auto\n",
    )]);
    let diags: Vec<String> = diagnostics(&ws, file).into_iter().map(|d| d.message).collect();
    assert_eq!(
        diags,
        [
            "flag `Hidden` is not allowed on functions (allowed on: Script, Property)",
            "flag `Hidden` is not allowed on variables (allowed on: Script, Property)",
            "flag `Global` is not allowed on properties (allowed on: Function)",
        ]
    );
}

// === Corpus ===

fn collect_psc(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    // Sorted: among scripts declared twice (two versions of a library), the
    // first indexed wins, which must not depend on the order of the OS.
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_psc(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psc")) {
            out.push(path);
        }
    }
}

/// The corpus compiles with the official compiler, so semantic checks must not
/// report anything on it. Each mod is checked against itself plus the vanilla
/// and SKSE sources of `psc_deps`.
#[test]
fn corpus_has_no_semantic_diagnostics() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().join("tests/corpus");
    let deps = root.join("psc_deps");
    let mut dep_files = Vec::new();
    collect_psc(&deps, &mut dep_files);
    if dep_files.len() < 100 {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("skipping corpus test: submodule not checked out");
        }
        return;
    }

    let mut ws = Workspace::new();
    let flg = deps.join("TESV_Papyrus_Flags.flg");
    ws.set_flags_file(flg.clone(), Arc::from(read_file(&flg).unwrap()));
    assert!(ws.flags().errors.is_empty());
    for path in &dep_files {
        let text = read_file(path).unwrap();
        ws.set_disk_index(path.clone(), index_text(&text), Origin::Import(0));
    }
    let mut files = Vec::new();
    collect_psc(&root, &mut files);
    files.retain(|p| !p.starts_with(&deps));
    let ids: Vec<_> = files
        .iter()
        .map(|p| {
            let text = read_file(p).unwrap();
            ws.set_disk_index(p.clone(), index_text(&text), Origin::Workspace)
        })
        .collect();

    let mut failures = Vec::new();
    let checked = ids.len();
    for id in ids {
        // The corpus puts unrelated mods in one workspace: a script of one mod
        // is not a known type for another.
        for d in diagnostics(&ws, id).into_iter().filter(|d| d.code != "name-is-type") {
            let text = ws.text(id).unwrap();
            let line = text[..usize::from(d.range.start())].lines().count().max(1);
            let path = ws
                .file(id)
                .path
                .strip_prefix(&root)
                .unwrap_or(&ws.file(id).path)
                .display()
                .to_string();
            failures.push(format!("{path}:{line}: [{}] {}", d.code, d.message));
        }
    }
    if !failures.is_empty() {
        let mut by_code = std::collections::BTreeMap::<String, usize>::new();
        for f in &failures {
            let code = f.split('[').nth(1).and_then(|s| s.split(']').next()).unwrap_or("?");
            *by_code.entry(code.to_owned()).or_default() += 1;
        }
        let mut shown_per_code = std::collections::BTreeMap::<String, usize>::new();
        let shown = failures
            .iter()
            .filter(|f| {
                let code =
                    f.split('[').nth(1).and_then(|s| s.split(']').next()).unwrap_or("?").to_owned();
                let n = shown_per_code.entry(code).or_default();
                *n += 1;
                *n <= 12
            })
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        panic!("{} diagnostics in {checked} files: {by_code:?}\n{shown}", failures.len());
    }
}
