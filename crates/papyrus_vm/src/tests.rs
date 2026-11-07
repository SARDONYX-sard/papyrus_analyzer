use std::{path::PathBuf, sync::Arc};

use papyrus_ide::Workspace;

use crate::{Script, ScriptLoader, Value, Vm};

/// Compiles the scripts of a workspace in memory, with the built-in compiler.
struct Compiled(Workspace);

impl ScriptLoader for Compiled {
    fn load(&mut self, name: &str) -> Option<Script> {
        let (file, _) = self
            .0
            .files()
            .find(|(_, d)| d.index.name.is_some_and(|n| n.as_str().eq_ignore_ascii_case(name)))?;
        let pex = papyrus_compiler::compile(&self.0, file, &Default::default())
            .unwrap_or_else(|e| panic!("{name} does not compile: {e:?}"));
        Script::from_pex(&pex)
    }
}

const DEBUG: &str =
    "ScriptName Debug Hidden\nFunction Trace(String asText, Int aiSeverity = 0) Global Native\n";
const FORM: &str = "ScriptName Form Hidden\nInt Function GetFormID() Native\nFunction RegisterForUpdate(Float afInterval) Native\nFunction RegisterForSingleUpdate(Float afInterval) Native\nFunction UnregisterForUpdate() Native\nFunction RegisterForSingleUpdateGameTime(Float afInterval) Native\n";
const UTILITY: &str = "ScriptName Utility Hidden\nFunction Wait(Float afSeconds) Global Native\nFunction WaitGameTime(Float afHours) Global Native\n";
const ACTOR: &str =
    "ScriptName Actor extends Form Hidden\nFloat Function GetActorValue(String asName) Native\n";

fn vm(scripts: &[(&str, &str)]) -> Vm {
    let mut ws = Workspace::new();
    let builtins = [("Debug", DEBUG), ("Form", FORM), ("Actor", ACTOR), ("Utility", UTILITY)];
    for (name, text) in builtins.iter().chain(scripts) {
        ws.set_open(PathBuf::from(format!("/ws/{name}.psc")), Arc::from(*text), 0);
    }
    Vm::new(Compiled(ws))
}

#[test]
fn runs_arithmetic_loops_and_strings() {
    let src = "ScriptName T\nInt Function Sum(Int n) Global\n    Int total = 0\n    Int i = 1\n    While i <= n\n        total += i\n        i += 1\n    EndWhile\n    Debug.Trace(\"sum of \" + n + \" = \" + total)\n    Return total\nEndFunction\nFloat Function Half(Int x) Global\n    Return x / 2.0\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    let v = vm.call_static("T", "Sum", &[Value::Int(10)]).unwrap();
    assert!(matches!(v, Value::Int(55)), "{v:?}");
    assert_eq!(vm.output, ["sum of 10 = 55"]);
    let half = vm.call_static("T", "Half", &[Value::Int(3)]).unwrap();
    assert_eq!(half.to_string(), "1.500000");
}

const FORM_WITH_MOD_EVENTS: &str = "ScriptName Form Hidden\nFunction RegisterForModEvent(String eventName, String callbackName) Native\nFunction UnregisterForModEvent(String eventName) Native\nFunction UnregisterForAllModEvents() Native\nFunction SendModEvent(String eventName, String strArg = \"\", Float numArg = 0.0) Native\nFunction RegisterForSingleUpdate(Float afInterval) Native\n";
const REF: &str = "ScriptName ObjectReference extends Form Hidden\nFunction Disable() Native\n";
const ACTOR_REF: &str = "ScriptName Actor extends ObjectReference Hidden\n";
const GAME: &str = "ScriptName Game Hidden\nActor Function GetPlayer() Global Native\n";
const MOD_EVENT: &str = "ScriptName ModEvent Hidden\nInt Function Create(String eventName) Global Native\nBool Function Send(Int handle) Global Native\nFunction Release(Int handle) Global Native\nFunction PushInt(Int handle, Int value) Global Native\nFunction PushString(Int handle, String value) Global Native\n";

/// A machine for a test script, with the scripts of the game it needs.
fn game_vm(src: &str) -> Vm {
    let mut ws = Workspace::new();
    let scripts = [
        ("Debug", DEBUG),
        ("Form", FORM_WITH_MOD_EVENTS),
        ("ObjectReference", REF),
        ("Actor", ACTOR_REF),
        ("Game", GAME),
        ("ModEvent", MOD_EVENT),
        ("Utility", UTILITY),
        ("PapyrusAnalyzer", crate::PAPYRUS_ANALYZER_PSC),
        ("Alias", "ScriptName Alias Hidden\n"),
        ("ActiveMagicEffect", "ScriptName ActiveMagicEffect Hidden\n"),
        ("T", src),
    ];
    for (name, text) in scripts {
        ws.set_open(PathBuf::from(format!("/ws/{name}.psc")), Arc::from(text), 0);
    }
    Vm::new(Compiled(ws))
}

/// Runs `body` as a function of a new object of a script extending `Form`.
fn run_body(members: &str, body: &str) -> Vm {
    let src = format!(
        "ScriptName T extends Form\nImport PapyrusAnalyzer\n{members}\nFunction Run()\n{body}\nEndFunction\n"
    );
    let mut vm = game_vm(&src);
    let obj = vm.new_object("T").unwrap();
    vm.call(obj, "Run", &[]).unwrap();
    vm
}

#[test]
fn every_function_of_papyrus_analyzer_has_examples() {
    // Each declaration, then the `;` comments right above it: the examples. (The
    // tests compiling it check that it parses, the doc tests that they work.)
    let lines: Vec<&str> = crate::PAPYRUS_ANALYZER_PSC.lines().collect();
    let declarations: Vec<usize> =
        (0..lines.len()).filter(|&i| lines[i].trim_end().ends_with(" Native")).collect();
    assert!(declarations.len() > 10);
    for &at in &declarations {
        let start = (0..at).rev().take_while(|&i| lines[i].starts_with(';')).last().unwrap_or(at);
        let docs = lines[start..at].join("\n");
        assert!(docs.contains("; # Examples\n;\n; ```papyrus\n"), "{}: no `# Examples`", lines[at]);
    }
}

#[test]
fn tests_create_objects() {
    let vm = run_body(
        "",
        "    Actor a = NewObject(\"Actor\") as Actor\n    AssertNotNone(a)\n    a.Disable()\n    AssertNotNone(Game.GetPlayer())\n    AssertTrue(Game.GetPlayer() == Game.GetPlayer())\n    AssertTrue(a != Game.GetPlayer())",
    );
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);

    // Its OnInit runs.
    let src = "ScriptName T extends Form\nImport PapyrusAnalyzer\nInt Property Started Auto\nEvent OnInit()\n    Started += 1\nEndEvent\nFunction Run()\n    T other = NewObject(\"T\") as T\n    AssertIntEq(other.Started, 1)\nEndFunction\n";
    let mut vm = game_vm(src);
    let obj = vm.new_object("T").unwrap();
    vm.call(obj, "Run", &[]).unwrap();
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);

    // No such script: None and a failure.
    let vm = run_body("", "    AssertNone(NewObject(\"Nope\"))");
    assert_eq!(vm.errors.len(), 1, "{:?}", vm.errors);
    assert!(vm.errors[0].contains("Nope"), "{:?}", vm.errors);
}

#[test]
fn tests_check_the_native_calls() {
    let body = "    Actor a = NewObject(\"Actor\") as Actor\n    Actor b = NewObject(\"Actor\") as Actor\n    a.Disable()\n    a.Disable()\n    AssertIntEq(CallCount(\"Disable\"), 2)\n    AssertIntEq(CallCount(\"ObjectReference.Disable\"), 2)\n    AssertIntEq(CallCount(\"actor.disable\"), 2)\n    AssertIntEq(CallCount(\"Form.Disable\"), 0)\n    AssertCalled(\"Disable\")\n    AssertCalled(\"ObjectReference.Disable\", 2)\n    AssertNotCalled(\"Delete\")\n    AssertCalledOn(a, \"Disable\", 2)\n    AssertCalledOn(b, \"Disable\", 0)";
    let vm = run_body("", body);
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);

    let vm = run_body(
        "",
        "    Actor a = NewObject(\"Actor\") as Actor\n    a.Disable()\n    AssertCalled(\"Disable\", 2, \"twice\")\n    AssertNotCalled(\"Disable\")\n    AssertCalled(\"Delete\")\n    AssertCalledOn(None, \"Disable\")",
    );
    assert_eq!(
        vm.errors,
        [
            "assertion failed: expected Disable to be called 2 times, called 1 times: twice",
            "assertion failed: expected Disable to be called 0 times, called 1 times",
            "assertion failed: expected Delete to be called at least once, called 0 times",
            "assertion failed: Disable called on None",
        ]
    );
}

#[test]
fn mod_events_call_their_callbacks() {
    let members = "String got\nFloat num\nForm sender\nInt calls\nEvent OnMine(String asName, String asArg, Float afNum, Form akSender)\n    got = asName + \"/\" + asArg\n    num = afNum\n    sender = akSender\n    calls += 1\nEndEvent\nEvent OnCustom(Int aiValue, String asText)\n    got = asText + aiValue\n    calls += 1\nEndEvent";
    let vm = run_body(
        members,
        "    RegisterForModEvent(\"Mine\", \"OnMine\")\n    SendModEvent(\"Other\")\n    AssertIntEq(calls, 0)\n    SendModEvent(\"MINE\", \"text\", 2.5)\n    AssertIntEq(calls, 1)\n    AssertStringEq(got, \"MINE/text\")\n    AssertFloatEq(num, 2.5)\n    AssertTrue(sender == Self)\n    UnregisterForModEvent(\"Mine\")\n    SendModEvent(\"Mine\")\n    AssertIntEq(calls, 1)\n    RegisterForModEvent(\"Custom\", \"OnCustom\")\n    Int handle = ModEvent.Create(\"Custom\")\n    ModEvent.PushInt(handle, 7)\n    ModEvent.PushString(handle, \"n\")\n    AssertTrue(ModEvent.Send(handle))\n    AssertIntEq(calls, 2)\n    AssertStringEq(got, \"n7\")\n    AssertFalse(ModEvent.Send(handle))\n    Int released = ModEvent.Create(\"Custom\")\n    ModEvent.Release(released)\n    AssertFalse(ModEvent.Send(released))\n    AssertFalse(ModEvent.Send(0))\n    AssertIntEq(calls, 2)\n    UnregisterForAllModEvents()\n    SendModEvent(\"Custom\")\n    AssertIntEq(calls, 2)",
    );
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);
}

#[test]
fn a_new_test_forgets_the_previous_one() {
    let src = "ScriptName T extends Form\nImport PapyrusAnalyzer\nInt updates\nFunction First()\n    RegisterForSingleUpdate(1.0)\n    RegisterForModEvent(\"E\", \"OnE\")\n    Debug.Trace(\"first\")\nEndFunction\nFunction Second()\n    Utility.Wait(5.0)\n    SendModEvent(\"E\")\n    AssertNotCalled(\"RegisterForSingleUpdate\")\nEndFunction\nEvent OnUpdate()\n    Fail(\"the update of the previous test\")\nEndEvent\nEvent OnE()\n    Fail(\"the event of the previous test\")\nEndEvent\n";
    let mut vm = game_vm(src);
    let first = vm.new_object("T").unwrap();
    vm.call(first, "First", &[]).unwrap();
    assert_eq!(vm.output, ["first"]);
    let player = vm.call_static("Game", "GetPlayer", &[]).unwrap();

    vm.begin_test();
    assert!(vm.output.is_empty());
    let second = vm.new_object("T").unwrap();
    vm.call(second, "Second", &[]).unwrap();
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);
    // Another player too.
    let other = vm.call_static("Game", "GetPlayer", &[]).unwrap();
    assert!(!player.loose_eq(&other));
}

#[test]
fn dispatches_through_parents_states_and_properties() {
    let base = "ScriptName Base extends Form\nString Function Greet()\n    Return \"hello\"\nEndFunction\n";
    let child = "ScriptName Child extends Base\nInt Property Count = 1 Auto\nInt doubled\nInt Property Doubled\n    Int Function Get()\n        Return Count * 2\n    EndFunction\nEndProperty\nString Function Greet()\n    Return Parent.Greet() + \"+child\"\nEndFunction\nFunction Bump()\n    Count += 1\n    GotoState(\"Busy\")\nEndFunction\nState Busy\n    String Function Greet()\n        Return \"working\"\n    EndFunction\nEndState\n";
    let mut vm = vm(&[("Base", base), ("Child", child)]);
    let obj = vm.new_object("Child").unwrap();
    assert_eq!(vm.call(obj, "Greet", &[]).unwrap().to_string(), "hello+child");
    vm.call(obj, "Bump", &[]).unwrap();
    assert_eq!(vm.state_of(obj), "Busy");
    assert_eq!(vm.call(obj, "Greet", &[]).unwrap().to_string(), "working");
    assert_eq!(vm.call(obj, "GetState", &[]).unwrap().to_string(), "Busy");
    // The string table shares entries ignoring case, like the game: the
    // literal `base` would read `Base` (the script name).
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);
}

#[test]
fn natives_none_and_failed_casts() {
    let src = "ScriptName T extends Form\nFloat Function Health(Form akForm) Global\n    Actor a = akForm as Actor\n    a.GetActorValue(\"Health\")\n    Return 1.0\nEndFunction\nFloat Function OwnHealth(Actor akActor) Global\n    Return akActor.GetActorValue(\"Health\")\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    // A Form that is not an Actor: the cast gives None, the call fails.
    let form = vm.new_object("Form").unwrap();
    vm.call_static("T", "Health", &[Value::Object(form)]).unwrap();
    assert_eq!(vm.errors, ["Cannot call getactorvalue() on a None object"]);

    // An Actor: the native returns its default, and is recorded.
    let actor = vm.new_object("Actor").unwrap();
    let hp = vm.call_static("T", "OwnHealth", &[Value::Object(actor)]).unwrap();
    assert_eq!(hp.as_float(), 0.0);
    let call = vm.native_calls.last().unwrap();
    assert_eq!(
        (call.script.as_str(), call.function.as_str(), call.implemented),
        ("Actor", "GetActorValue", false)
    );
    assert_eq!(call.args[0].to_string(), "Health");

    // A mock.
    vm.register_native("Actor", "GetActorValue", |_, _, _| Value::Float(50.0));
    let hp = vm.call_static("T", "OwnHealth", &[Value::Object(actor)]).unwrap();
    assert_eq!(hp.as_float(), 50.0);
}

#[test]
fn asserts_record_failures() {
    let src = "ScriptName T\nImport PapyrusAnalyzer\nFunction Check(Int x) Global\n    PapyrusAnalyzer.AssertIntEq(x, 3, \"three\")\n    AssertFloatEq(x / 2.0, 1.5)\n    AssertTrue(x > 0)\nEndFunction\n";
    let mut vm = vm(&[
        ("T", src),
        ("PapyrusAnalyzer", crate::PAPYRUS_ANALYZER_PSC),
        ("Alias", "ScriptName Alias Hidden\n"),
        ("ActiveMagicEffect", "ScriptName ActiveMagicEffect Hidden\n"),
    ]);
    vm.call_static("T", "Check", &[Value::Int(3)]).unwrap();
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);
    vm.call_static("T", "Check", &[Value::Int(-1)]).unwrap();
    assert_eq!(
        vm.errors,
        [
            "assertion failed: expected 3, got -1: three",
            "assertion failed: expected 1.5, got -0.5",
            "assertion failed: expected True",
        ]
    );
}

#[test]
fn mocks_replace_natives() {
    let src = "ScriptName T extends Form\nInt calls\nFloat Function Health(Actor akActor)\n    Return akActor.GetActorValue(\"Health\")\nEndFunction\nFloat Function MockAV(Actor akSelf, String asName)\n    calls += 1\n    If akSelf && asName == \"Health\"\n        Return 50.0\n    EndIf\n    Return 0.0\nEndFunction\nInt Function Calls()\n    Return calls\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    let test = vm.new_object("T").unwrap();
    let actor = vm.new_object("Actor").unwrap();
    vm.mock("Actor.GetActorValue", test, "MockAV");
    let hp = vm.call(test, "Health", &[Value::Object(actor)]).unwrap();
    assert_eq!(hp.as_float(), 50.0);
    assert_eq!(vm.call(test, "Calls", &[]).unwrap().as_int(), 1);
    vm.clear_mocks();
    assert_eq!(vm.call(test, "Health", &[Value::Object(actor)]).unwrap().as_float(), 0.0);
}

#[test]
fn waits_send_update_events() {
    let src = "ScriptName T extends Form\nInt ticks\nInt nights\nEvent OnInit()\n    RegisterForUpdate(1.0)\n    RegisterForSingleUpdateGameTime(1.0)\nEndEvent\nEvent OnUpdate()\n    ticks += 1\n    If ticks == 3\n        UnregisterForUpdate()\n    EndIf\nEndEvent\nEvent OnUpdateGameTime()\n    nights += 1\nEndEvent\nFunction Run()\n    Utility.Wait(2.5)\n    Debug.Trace(ticks)\n    Utility.Wait(10.0)\n    Debug.Trace(ticks)\n    Utility.WaitGameTime(1.0)\n    Debug.Trace(nights)\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    let obj = vm.new_object("T").unwrap();
    vm.send_event(obj, "OnInit", &[]).unwrap();
    // Not defined: nothing happens.
    vm.send_event(obj, "OnActivate", &[Value::None]).unwrap();
    vm.call(obj, "Run", &[]).unwrap();
    assert_eq!(vm.output, ["2", "3", "1"]);
    assert_eq!(vm.clock, 12.5 + 180.0);
    assert!(vm.errors.is_empty(), "{:?}", vm.errors);
}

#[test]
fn debuggers_follow_lines_frames_and_locals() {
    use std::{cell::RefCell, rc::Rc};
    struct Recorder(Rc<RefCell<Vec<String>>>);
    impl crate::Debugger for Recorder {
        fn line(&mut self, vm: &Vm) -> bool {
            let top = vm.stack().last().unwrap();
            let locals: Vec<String> = top.locals.iter().map(|(n, v)| format!("{n}={v}")).collect();
            let fields = top.this.map(|o| vm.fields(o).1.len()).unwrap_or_default();
            self.0.borrow_mut().push(format!(
                "{}.{}:{} depth={} [{}] fields={fields}",
                top.script,
                top.function,
                top.line.unwrap(),
                vm.stack().len(),
                locals.join(" ")
            ));
            // Stop on the third line event.
            self.0.borrow().len() < 6
        }
    }
    let src = "ScriptName T extends Form\nInt Property Count = 1 Auto\nInt Function Twice(Int aiValue)\n    Return aiValue * 2\nEndFunction\nInt Function Run()\n    Int total = Twice(Count)\n    total += 1\n    Return total\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    let events = Rc::new(RefCell::new(Vec::new()));
    vm.set_debugger(Recorder(events.clone()));
    let obj = vm.new_object("T").unwrap();
    assert_eq!(vm.call(obj, "Run", &[]).unwrap().as_int(), 3);
    assert_eq!(
        *events.borrow(),
        [
            "T.Run:7 depth=1 [total=0] fields=1",
            "T.Twice:4 depth=2 [aiValue=1] fields=1",
            "T.Run:8 depth=1 [total=2] fields=1",
            "T.Run:9 depth=1 [total=3] fields=1",
        ]
    );
    // `false` stops the run.
    events.borrow_mut().extend(["".to_owned(), "".to_owned()]);
    assert_eq!(vm.call(obj, "Run", &[]).unwrap_err(), crate::VmError::Stopped);
}

#[test]
fn objects_as_strings_like_the_game() {
    let src = "ScriptName T extends Form\nFunction Show(Form akOther)\n    Debug.Trace(\"self: \" + Self)\n    Debug.Trace(akOther as String)\n    Debug.Trace(\"none: \" + (None as Form))\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    let obj = vm.new_object("T").unwrap();
    let other = vm.new_object("Actor").unwrap();
    vm.call(obj, "Show", &[Value::Object(other)]).unwrap();
    assert_eq!(vm.output, ["self: [T < (FF000000)>]", "[Actor < (FF000001)>]", "none: None"]);
}

#[test]
fn errors_of_events_stop_the_wait() {
    let src = "ScriptName T extends Form\nEvent OnUpdate()\n    While True\n    EndWhile\nEndEvent\nFunction Run()\n    RegisterForSingleUpdate(1.0)\n    Utility.Wait(2.0)\n    Debug.Trace(\"after\")\nEndFunction\n";
    let mut vm = vm(&[("T", src)]);
    vm.step_limit = 10_000;
    let obj = vm.new_object("T").unwrap();
    assert_eq!(vm.call(obj, "Run", &[]).unwrap_err(), crate::VmError::StepLimit);
    assert!(vm.output.is_empty());
}
