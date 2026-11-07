//! The built-in natives: what can be done without the game.
//!
//! - `Debug.Trace`, `Debug.Notification`, `Debug.MessageBox`: to
//!   [`Vm::output`];
//! - `Math.*`: computed (angles in degrees, like the game);
//! - `Utility.RandomInt`, `Utility.RandomFloat`: from [`Vm::seed`];
//! - `Utility.Wait`, `Utility.WaitGameTime`: advance [`Vm::clock`] and send
//!   the due update events ([`Vm::advance`]); `Utility.GetCurrentRealTime`,
//!   `Utility.GetCurrentGameTime` read it;
//! - `RegisterForUpdate`, `RegisterForSingleUpdate`, their `GameTime`
//!   versions and `UnregisterForUpdate*` of `Form`, `Alias` and
//!   `ActiveMagicEffect`: schedule `OnUpdate` / `OnUpdateGameTime`;
//! - `RegisterForModEvent`, `SendModEvent`, `ModEvent.*`: custom events, whose
//!   callbacks run when sent;
//! - `Game.GetPlayer`: one object of the script `Actor`;
//! - `StringUtil.GetLength`, `StringUtil.Substring`, `StringUtil.Find`;
//! - `PapyrusAnalyzer`, see [`PAPYRUS_ANALYZER_PSC`]: objects for tests, the native
//!   calls made, and assertions: a failed one is an error of [`Vm::errors`], which
//!   fails the test.

use std::rc::Rc;

use crate::{ObjId, Value, Vm};

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

fn text(vm: &Vm, args: &[Value], i: usize) -> String {
    vm.text(&arg(args, i))
}

fn random(vm: &mut Vm) -> u64 {
    // xorshift64*
    let mut x = vm.seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    vm.seed = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// A game hour in seconds, at the default time scale of 20.
const GAME_HOUR: f32 = 3600.0 / 20.0;

/// Advances the clock, sending the due update events; an error of one stops
/// the run.
fn wait(vm: &mut Vm, seconds: f32) -> Value {
    if let Err(e) = vm.advance(seconds) {
        vm.abort = Some(e);
    }
    Value::None
}

/// The name of the built-in script of tests, [`PAPYRUS_ANALYZER_PSC`].
pub const PAPYRUS_ANALYZER: &str = "PapyrusAnalyzer";

/// `PapyrusAnalyzer.psc`, built into papyrus_analyzer: the assertions of
/// tests, for the test directories only. Only `Native` declarations, which
/// the machine implements. `Import PapyrusAnalyzer` calls them unqualified.
pub const PAPYRUS_ANALYZER_PSC: &str = include_str!("PapyrusAnalyzer.psc");

/// Records a failed assertion.
fn fail(vm: &mut Vm, what: String, message: &Value) -> Value {
    let message = message.to_string();
    let message = if message.is_empty() { String::new() } else { format!(": {message}") };
    vm.errors.push(format!("assertion failed: {what}{message}"));
    Value::None
}

fn register_asserts(vm: &mut Vm) {
    let eq = |f: fn(&Value, &Value) -> bool| {
        move |vm: &mut Vm, _, args: &[Value]| {
            let (actual, expected) = (arg(args, 0), arg(args, 1));
            if f(&actual, &expected) {
                return Value::None;
            }
            fail(vm, format!("expected {expected}, got {actual}"), &arg(args, 2))
        }
    };
    const S: &str = PAPYRUS_ANALYZER;
    vm.register_native(S, "AssertIntEq", eq(|a, b| a.as_int() == b.as_int()));
    vm.register_native(S, "AssertBoolEq", eq(|a, b| a.truthy() == b.truthy()));
    vm.register_native(S, "AssertStringEq", eq(|a, b| a.loose_eq(b)));
    vm.register_native(S, "AssertFloatEq", |vm, _, args| {
        let (actual, expected) = (arg(args, 0).as_float(), arg(args, 1).as_float());
        if (actual - expected).abs() <= arg(args, 2).as_float() {
            return Value::None;
        }
        fail(vm, format!("expected {expected}, got {actual}"), &arg(args, 3))
    });
    let check = |want: bool, what: &'static str| {
        move |vm: &mut Vm, _, args: &[Value]| {
            if arg(args, 0).truthy() == want {
                return Value::None;
            }
            fail(vm, what.to_owned(), &arg(args, 1))
        }
    };
    vm.register_native(S, "AssertTrue", check(true, "expected True"));
    vm.register_native(S, "AssertFalse", check(false, "expected False"));
    vm.register_native(S, "AssertNotNone", check(true, "expected an object, got None"));
    vm.register_native(S, "AssertNone", check(false, "expected None"));
    vm.register_native(S, "Fail", |vm, _, args| fail(vm, "failed".to_owned(), &arg(args, 0)));

    for name in ["NewObject", "NewAlias", "NewEffect"] {
        vm.register_native(S, name, |vm, _, args| {
            let script = text(vm, args, 0);
            match vm.new_object(&script) {
                Ok(obj) => {
                    if let Err(e) = vm.send_event(obj, "OnInit", &[]) {
                        vm.abort = Some(e);
                    }
                    Value::Object(obj)
                }
                Err(e) => fail(vm, format!("cannot create the object: {e}"), &Value::None),
            }
        });
    }
    vm.register_native(S, "CallCount", |vm, _, args| {
        let native = text(vm, args, 0);
        Value::Int(vm.count_calls(&native, None) as i32)
    });
    vm.register_native(S, "AssertCalled", |vm, _, args| {
        let native = text(vm, args, 0);
        assert_called(vm, &native, None, arg(args, 1).as_int(), &arg(args, 2))
    });
    vm.register_native(S, "AssertNotCalled", |vm, _, args| {
        let native = text(vm, args, 0);
        assert_called(vm, &native, None, 0, &arg(args, 1))
    });
    vm.register_native(S, "AssertCalledOn", |vm, _, args| {
        let native = text(vm, args, 1);
        let Value::Object(obj) = arg(args, 0) else {
            return fail(vm, format!("{native} called on None"), &arg(args, 3));
        };
        assert_called(vm, &native, Some(obj), arg(args, 2).as_int(), &arg(args, 3))
    });
}

/// Fails unless `native` was called `times` times; negative: at least once.
fn assert_called(
    vm: &mut Vm,
    native: &str,
    on: Option<ObjId>,
    times: i32,
    message: &Value,
) -> Value {
    let count = vm.count_calls(native, on) as i32;
    let ok = if times < 0 { count > 0 } else { count == times };
    if ok {
        return Value::None;
    }
    let expected = if times < 0 { "at least once".to_owned() } else { format!("{times} times") };
    fail(vm, format!("expected {native} to be called {expected}, called {count} times"), message)
}

/// The custom events of SKSE.
fn register_mod_events(vm: &mut Vm) {
    for script in ["Form", "Alias", "ActiveMagicEffect"] {
        vm.register_native(script, "RegisterForModEvent", |vm, this, args| {
            if let Some(obj) = this {
                let (event, callback) = (text(vm, args, 0), text(vm, args, 1));
                vm.register_mod_event(obj, &event, callback);
            }
            Value::None
        });
        vm.register_native(script, "UnregisterForModEvent", |vm, this, args| {
            if let Some(obj) = this {
                let event = text(vm, args, 0);
                vm.unregister_mod_event(obj, Some(&event));
            }
            Value::None
        });
        vm.register_native(script, "UnregisterForAllModEvents", |vm, this, _| {
            if let Some(obj) = this {
                vm.unregister_mod_event(obj, None);
            }
            Value::None
        });
        vm.register_native(script, "SendModEvent", |vm, this, args| {
            let event = text(vm, args, 0);
            // eventName, strArg, numArg, sender.
            let sender = this.map_or(Value::None, Value::Object);
            let sent = [Value::Str(Rc::from(event.as_str())), arg(args, 1), arg(args, 2), sender];
            vm.send_mod_event(&event, &sent);
            Value::None
        });
    }

    const M: &str = "ModEvent";
    vm.register_native(M, "Create", |vm, _, args| {
        let event = text(vm, args, 0);
        vm.mod_events.push(Some((event, Vec::new())));
        Value::Int(vm.mod_events.len() as i32)
    });
    for name in ["PushBool", "PushInt", "PushFloat", "PushString", "PushForm"] {
        vm.register_native(M, name, |vm, _, args| {
            if let Some(Some((_, pushed))) = mod_event(vm, &arg(args, 0)) {
                pushed.push(arg(args, 1));
            }
            Value::None
        });
    }
    vm.register_native(M, "Send", |vm, _, args| {
        match mod_event(vm, &arg(args, 0)).map(Option::take) {
            Some(Some((event, pushed))) => {
                vm.send_mod_event(&event, &pushed);
                Value::Bool(true)
            }
            _ => Value::Bool(false),
        }
    });
    vm.register_native(M, "Release", |vm, _, args| {
        if let Some(event) = mod_event(vm, &arg(args, 0)) {
            *event = None;
        }
        Value::None
    });
}

/// The event of a handle of `ModEvent.Create`.
fn mod_event<'a>(vm: &'a mut Vm, handle: &Value) -> Option<&'a mut Option<(String, Vec<Value>)>> {
    let index = usize::try_from(handle.as_int()).ok()?.checked_sub(1)?;
    vm.mod_events.get_mut(index)
}

pub(crate) fn register_builtins(vm: &mut Vm) {
    register_asserts(vm);
    register_mod_events(vm);
    vm.register_native("Game", "GetPlayer", |vm, _, _| {
        vm.player().map_or(Value::None, Value::Object)
    });
    for name in ["Trace", "Notification", "MessageBox", "TraceStack"] {
        vm.register_native("Debug", name, |vm, _, args| {
            vm.output.push(text(vm, args, 0));
            Value::None
        });
    }

    let float = |f: fn(f32) -> f32| {
        move |_: &mut Vm, _, args: &[Value]| Value::Float(f(arg(args, 0).as_float()))
    };
    vm.register_native("Math", "Abs", float(f32::abs));
    vm.register_native("Math", "Sqrt", float(f32::sqrt));
    vm.register_native("Math", "Sin", float(|d| d.to_radians().sin()));
    vm.register_native("Math", "Cos", float(|d| d.to_radians().cos()));
    vm.register_native("Math", "Tan", float(|d| d.to_radians().tan()));
    vm.register_native("Math", "aSin", float(|x| x.asin().to_degrees()));
    vm.register_native("Math", "aCos", float(|x| x.acos().to_degrees()));
    vm.register_native("Math", "aTan", float(|x| x.atan().to_degrees()));
    vm.register_native("Math", "Floor", |_, _, args| {
        Value::Int(arg(args, 0).as_float().floor() as i32)
    });
    vm.register_native("Math", "Ceiling", |_, _, args| {
        Value::Int(arg(args, 0).as_float().ceil() as i32)
    });
    vm.register_native("Math", "Pow", |_, _, args| {
        Value::Float(arg(args, 0).as_float().powf(arg(args, 1).as_float()))
    });

    vm.register_native("Utility", "RandomInt", |vm, _, args| {
        let (lo, hi) = (arg(args, 0).as_int(), arg(args, 1).as_int());
        let span = (i64::from(hi) - i64::from(lo) + 1).max(1) as u64;
        Value::Int((i64::from(lo) + (random(vm) % span) as i64) as i32)
    });
    vm.register_native("Utility", "RandomFloat", |vm, _, args| {
        let (lo, hi) = (arg(args, 0).as_float(), arg(args, 1).as_float());
        let unit = (random(vm) >> 40) as f32 / (1u64 << 24) as f32;
        Value::Float(lo + (hi - lo) * unit)
    });
    vm.register_native("Utility", "Wait", |vm, _, args| wait(vm, arg(args, 0).as_float()));
    vm.register_native("Utility", "WaitGameTime", |vm, _, args| {
        wait(vm, arg(args, 0).as_float() * GAME_HOUR)
    });
    for script in ["Form", "Alias", "ActiveMagicEffect"] {
        let update = |repeat: bool, game_time: bool| {
            move |vm: &mut Vm, this: Option<ObjId>, args: &[Value]| {
                let hours = if game_time { GAME_HOUR } else { 1.0 };
                if let Some(obj) = this {
                    vm.register_update(obj, arg(args, 0).as_float() * hours, repeat, game_time);
                }
                Value::None
            }
        };
        vm.register_native(script, "RegisterForUpdate", update(true, false));
        vm.register_native(script, "RegisterForSingleUpdate", update(false, false));
        vm.register_native(script, "RegisterForUpdateGameTime", update(true, true));
        vm.register_native(script, "RegisterForSingleUpdateGameTime", update(false, true));
        let unregister = |game_time: bool| {
            move |vm: &mut Vm, this: Option<ObjId>, _: &[Value]| {
                if let Some(obj) = this {
                    vm.unregister_update(obj, game_time);
                }
                Value::None
            }
        };
        vm.register_native(script, "UnregisterForUpdate", unregister(false));
        vm.register_native(script, "UnregisterForUpdateGameTime", unregister(true));
    }
    vm.register_native("Utility", "GetCurrentRealTime", |vm, _, _| Value::Float(vm.clock));
    vm.register_native("Utility", "GetCurrentGameTime", |vm, _, _| {
        Value::Float(vm.clock / GAME_HOUR / 24.0)
    });

    vm.register_native("StringUtil", "GetLength", |vm, _, args| {
        Value::Int(text(vm, args, 0).chars().count() as i32)
    });
    vm.register_native("StringUtil", "Substring", |vm, _, args| {
        let s: Vec<char> = text(vm, args, 0).chars().collect();
        let start = arg(args, 1).as_int().clamp(0, s.len() as i32) as usize;
        let len = arg(args, 2).as_int();
        let end = if len <= 0 { s.len() } else { (start + len as usize).min(s.len()) };
        Value::Str(Rc::from(s[start..end].iter().collect::<String>()))
    });
    vm.register_native("StringUtil", "Find", |vm, _, args| {
        let (s, needle) =
            (text(vm, args, 0).to_ascii_lowercase(), text(vm, args, 1).to_ascii_lowercase());
        let start = arg(args, 2).as_int().max(0) as usize;
        let found = s.get(start..).and_then(|rest| rest.find(&needle)).map(|i| (i + start) as i32);
        Value::Int(found.unwrap_or(-1))
    });
}
