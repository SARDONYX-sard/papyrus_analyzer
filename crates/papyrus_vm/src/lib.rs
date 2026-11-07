//! A small Papyrus virtual machine, to run scripts outside of the game, in
//! tests for example.
//!
//! It runs the code of `.pex` files ([`Script::from_pex`]), loaded on demand
//! through a [`ScriptLoader`]. Calls dispatch like in the game: through the
//! `extends` chain, the functions of the current state first; `Parent.`
//! starts at the parent of the calling script.
//!
//! Natives have no code. A native call runs, in order:
//!
//! 1. a mock: a function of a test object ([`Vm::mock`], `; #[mock(Script.Function)]`
//!    in tests);
//! 2. the implementation registered for `Script.Function`
//!    ([`Vm::register_native`]); `Debug.Trace`, `Math.*` and a few `Utility`
//!    functions are built in, see [`natives`];
//! 3. else nothing: the default value of the return type (`None`, `0`,
//!    `False`, `""`).
//!
//! Every native call is recorded in [`Vm::native_calls`], so that tests can
//! check what the script asked the game to do.
//!
//! Unlike the game, one thread runs everything: a call runs to completion,
//! `Utility.Wait` only advances [`Vm::clock`], sending on the way the
//! `OnUpdate` / `OnUpdateGameTime` events registered with `RegisterForUpdate`
//! and the like ([`Vm::advance`]). The callbacks of `SendModEvent` run when it is
//! called. Other events are sent by calling them
//! ([`Vm::send_event`]). Errors the game logs (a call on
//! `None`, an index out of bounds) go to [`Vm::errors`], and the code goes on
//! with a default value, like in the game.

pub mod natives;
mod script;
mod value;

use std::rc::Rc;

use papyrus_pex::pex::OpCode;
use rustc_hash::FxHashMap;

use crate::script::{Func, Operand, key};
pub use crate::{
    natives::{PAPYRUS_ANALYZER, PAPYRUS_ANALYZER_PSC},
    script::Script,
    value::{ObjId, Value},
};

/// Gives the scripts the machine needs, by name.
pub trait ScriptLoader {
    /// The script `name` (any case), `None` when unknown.
    fn load(&mut self, name: &str) -> Option<Script>;
}

/// A native implementation: the machine, `Self` (for non-global functions)
/// and the arguments.
pub type NativeFn = Rc<dyn Fn(&mut Vm, Option<ObjId>, &[Value]) -> Value>;

/// A recorded native call.
#[derive(Clone, Debug)]
pub struct NativeCall {
    /// The script declaring the native, as declared.
    pub script: String,
    /// The function, as declared.
    pub function: String,
    /// The object it was called on; `None` for global functions.
    pub this: Option<ObjId>,
    /// The arguments.
    pub args: Vec<Value>,
    /// Whether an implementation ran (else: the default value).
    pub implemented: bool,
}

/// A function running, for debuggers ([`Vm::stack`]).
#[derive(Clone, Debug)]
pub struct StackFrame {
    /// The script defining the function, as declared.
    pub script: String,
    /// The function, as declared.
    pub function: String,
    /// The source line running, when the script has debug info.
    pub line: Option<u32>,
    /// `Self`, for non-global functions.
    pub this: Option<ObjId>,
    /// The parameters and locals, as declared, as of the last line event or
    /// call of the function.
    pub locals: Vec<(String, Value)>,
}

/// Follows a run line by line, like a debugger: set with
/// [`Vm::set_debugger`].
pub trait Debugger {
    /// A line starts running: the top of [`Vm::stack`]. Returns `false` to
    /// stop the run ([`VmError::Stopped`]).
    fn line(&mut self, vm: &Vm) -> bool;
}

/// An error of the machine itself, which stops the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmError {
    /// The debugger stopped the run.
    Stopped,
    /// No script of that name.
    UnknownScript(String),
    /// No function of that name.
    UnknownFunction(String),
    /// Too many instructions: probably an endless loop.
    StepLimit,
    /// Calls nested too deep: probably an endless recursion.
    StackOverflow,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::Stopped => f.write_str("stopped by the debugger"),
            VmError::UnknownScript(s) => write!(f, "unknown script `{s}`"),
            VmError::UnknownFunction(s) => write!(f, "unknown function `{s}`"),
            VmError::StepLimit => f.write_str("too many instructions (endless loop?)"),
            VmError::StackOverflow => f.write_str("calls nested too deep (endless recursion?)"),
        }
    }
}

struct Instance {
    /// The script, lowercase.
    script: String,
    /// The variables of the script and its parents.
    vars: FxHashMap<String, Value>,
    types: FxHashMap<String, String>,
    /// The current state, as written.
    state: String,
}

struct Frame {
    locals: FxHashMap<String, Value>,
    types: FxHashMap<String, String>,
    this: Option<ObjId>,
    /// The script defining the running function, lowercase.
    script: String,
}

/// A registration of `RegisterForUpdate` and the like.
struct Timer {
    obj: ObjId,
    /// `OnUpdateGameTime`, else `OnUpdate`.
    game_time: bool,
    /// When it fires, on [`Vm::clock`].
    due: f32,
    /// The interval of a repeating timer.
    every: Option<f32>,
}

/// A registration of `RegisterForModEvent`.
struct ModEventListener {
    obj: ObjId,
    /// The name of the event, lowercase.
    event: String,
    callback: String,
}

/// The machine.
pub struct Vm {
    loader: Box<dyn ScriptLoader>,
    scripts: FxHashMap<String, Option<Rc<Script>>>,
    objects: Vec<Instance>,
    natives: FxHashMap<String, NativeFn>,
    /// `script.function` → the object and function replacing it.
    mocks: FxHashMap<String, (ObjId, String)>,
    /// The mocks running, which call the real native when they call it.
    mocking: rustc_hash::FxHashSet<String>,
    /// What `Debug.Trace`, `Debug.Notification`... printed.
    pub output: Vec<String>,
    /// Errors the game would log; the code went on.
    pub errors: Vec<String>,
    /// Every native call, in order.
    pub native_calls: Vec<NativeCall>,
    /// Seconds of game time, advanced by `Utility.Wait`.
    pub clock: f32,
    /// The state of `Utility.Random*`, fixed for reproducible runs.
    pub seed: u64,
    /// Instructions left before [`VmError::StepLimit`].
    pub step_limit: u64,
    depth: usize,
    timers: Vec<Timer>,
    mod_event_listeners: Vec<ModEventListener>,
    /// The events of `ModEvent.Create`, by handle - 1: the name and the arguments
    /// pushed; `None` once sent or released.
    mod_events: Vec<Option<(String, Vec<Value>)>>,
    /// The object of `Game.GetPlayer`.
    player: Option<ObjId>,
    /// An error of an event sent by a native, which stops the run when the
    /// native returns.
    abort: Option<VmError>,
    debugger: Option<Box<dyn Debugger>>,
    /// The functions running, kept with a debugger only.
    stack: Vec<StackFrame>,
}

impl Vm {
    /// A machine loading scripts with `loader`, with the built-in natives.
    pub fn new(loader: impl ScriptLoader + 'static) -> Vm {
        let mut vm = Vm {
            loader: Box::new(loader),
            scripts: FxHashMap::default(),
            objects: Vec::new(),
            natives: FxHashMap::default(),
            mocks: FxHashMap::default(),
            mocking: Default::default(),
            output: Vec::new(),
            errors: Vec::new(),
            native_calls: Vec::new(),
            clock: 0.0,
            seed: 0x2545_f491_4f6c_dd1d,
            step_limit: 10_000_000,
            depth: 0,
            timers: Vec::new(),
            mod_event_listeners: Vec::new(),
            mod_events: Vec::new(),
            player: None,
            abort: None,
            debugger: None,
            stack: Vec::new(),
        };
        natives::register_builtins(&mut vm);
        vm
    }

    /// Starts a new test: forgets what the previous one left, its output and native
    /// calls, and the update and custom events of its objects, which would fire
    /// during this one. Objects stay, unused.
    pub fn begin_test(&mut self) {
        self.output.clear();
        self.errors.clear();
        self.native_calls.clear();
        self.timers.clear();
        self.mod_event_listeners.clear();
        self.mod_events.clear();
        self.player = None;
    }

    /// Implements the native `script.function` (any case).
    pub fn register_native(
        &mut self,
        script: &str,
        function: &str,
        f: impl Fn(&mut Vm, Option<ObjId>, &[Value]) -> Value + 'static,
    ) {
        self.natives.insert(format!("{}.{}", key(script), key(function)), Rc::new(f));
    }

    fn script(&mut self, name: &str) -> Option<Rc<Script>> {
        let k = key(name);
        if let Some(s) = self.scripts.get(&k) {
            return s.clone();
        }
        let s = self.loader.load(name).map(Rc::new);
        self.scripts.insert(k, s.clone());
        s
    }

    /// `name` and its parents.
    fn chain(&mut self, name: &str) -> Vec<Rc<Script>> {
        let mut out = Vec::new();
        let mut next = Some(key(name));
        while let Some(name) = next.take() {
            if out.len() > 64 {
                break;
            }
            let Some(s) = self.script(&name) else { break };
            next = s.parent.clone();
            out.push(s);
        }
        out
    }

    /// Creates an object of `script`, its variables set to their initial
    /// values, in its `Auto` state. `OnInit` is not called: send it with
    /// [`Vm::send_event`].
    pub fn new_object(&mut self, script: &str) -> Result<ObjId, VmError> {
        let chain = self.chain(script);
        if chain.is_empty() {
            return Err(VmError::UnknownScript(script.to_owned()));
        }
        let mut vars = FxHashMap::default();
        let mut types = FxHashMap::default();
        for s in &chain {
            for (name, ty, init) in &s.variables {
                types.entry(name.clone()).or_insert_with(|| ty.clone());
                let value = match init {
                    Operand::None => Value::default_of(ty),
                    other => constant(other),
                };
                vars.entry(name.clone()).or_insert(value);
            }
        }
        let state = chain.iter().map(|s| s.auto_state.clone()).find(|s| !s.is_empty());
        self.objects.push(Instance {
            script: key(script),
            vars,
            types,
            state: state.unwrap_or_default(),
        });
        Ok(ObjId(self.objects.len() - 1))
    }

    /// The script of an object, as declared.
    pub fn script_of(&mut self, obj: ObjId) -> String {
        let k = self.objects[obj.0].script.clone();
        self.script(&k).map_or(k, |s| s.name.clone())
    }

    /// The current state of an object.
    pub fn state_of(&self, obj: ObjId) -> &str {
        &self.objects[obj.0].state
    }

    /// Follows the runs with `debugger`.
    pub fn set_debugger(&mut self, debugger: impl Debugger + 'static) {
        self.debugger = Some(Box::new(debugger));
    }

    /// The functions running, the innermost last; empty without debugger.
    pub fn stack(&self) -> &[StackFrame] {
        &self.stack
    }

    /// The script of an object as declared, and the values of its auto
    /// properties and variables as declared, for debuggers. Scripts are
    /// taken from the cache: the ones of objects are loaded.
    pub fn fields(&self, obj: ObjId) -> (String, Vec<(String, Value)>) {
        let object = &self.objects[obj.0];
        let mut chain = Vec::new();
        let mut next = Some(object.script.clone());
        while let Some(name) = next.take() {
            let Some(Some(s)) = self.scripts.get(&name) else { break };
            next = s.parent.clone();
            chain.push(s.clone());
        }
        let script = chain.first().map_or_else(|| object.script.clone(), |s| s.name.clone());
        let mut fields = Vec::new();
        for s in &chain {
            let mut properties: Vec<_> = s.properties.values().collect();
            properties.sort_by(|a, b| a.name.cmp(&b.name));
            for p in properties {
                if let Some(var) = &p.auto_var {
                    fields
                        .push((p.name.clone(), object.vars.get(var).cloned().unwrap_or_default()));
                }
            }
            for (var, _, _) in &s.variables {
                if var.starts_with("::") {
                    continue;
                }
                let name = s.variable_names.get(var).cloned().unwrap_or_else(|| var.clone());
                fields.push((name, object.vars.get(var).cloned().unwrap_or_default()));
            }
        }
        (script, fields)
    }

    /// `value` as `String` casts it, like the game: objects as
    /// `[Script < (FF000003)>]` (no editor ID; the form ID of a reference
    /// created at run time).
    pub fn text(&self, value: &Value) -> String {
        match value {
            Value::Object(obj) => {
                let (script, _) = self.fields(*obj);
                format!("[{script} < ({:08X})>]", 0xFF00_0000 | obj.0 as u32)
            }
            other => other.to_string(),
        }
    }

    /// The parameters and locals of a frame, as declared.
    fn locals_of(func: &Func, frame: &Frame) -> Vec<(String, Value)> {
        (func.params.iter().chain(&func.locals))
            .filter(|(name, _)| !name.starts_with("::"))
            .map(|(name, _)| {
                let shown = func.names.get(name).cloned().unwrap_or_else(|| name.clone());
                (shown, frame.locals.get(name).cloned().unwrap_or_default())
            })
            .collect()
    }

    /// Before instruction `pc`: with a debugger, sends the line events and
    /// keeps the locals of the frame for the calls it makes.
    fn trace(&mut self, func: &Func, frame: &Frame, pc: usize) -> Result<(), VmError> {
        if self.debugger.is_none() {
            return Ok(());
        }
        let line = func.lines.get(pc).copied();
        let calls = matches!(
            func.code[pc].op,
            OpCode::CallMethod
                | OpCode::CallParent
                | OpCode::CallStatic
                | OpCode::PropGet
                | OpCode::PropSet
        );
        let Some(top) = self.stack.last_mut() else { return Ok(()) };
        let new_line = line.is_some() && line != top.line;
        if !new_line && !calls {
            return Ok(());
        }
        top.locals = Vm::locals_of(func, frame);
        if new_line {
            top.line = line;
            if let Some(mut debugger) = self.debugger.take() {
                let go_on = debugger.line(self);
                self.debugger = Some(debugger);
                if !go_on {
                    return Err(VmError::Stopped);
                }
            }
        }
        Ok(())
    }

    /// Calls the function or event `name` of `obj`.
    pub fn call(&mut self, obj: ObjId, name: &str, args: &[Value]) -> Result<Value, VmError> {
        let script = self.objects[obj.0].script.clone();
        let state = self.objects[obj.0].state.clone();
        let (func, defined) = self
            .resolve(&script, &state, &key(name))
            .ok_or_else(|| VmError::UnknownFunction(name.to_owned()))?;
        self.invoke(func, Some(obj), &defined, args)
    }

    /// Calls the `Global` function `script.name`.
    pub fn call_static(
        &mut self,
        script: &str,
        name: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let (func, defined) = self
            .resolve(script, "", &key(name))
            .ok_or_else(|| VmError::UnknownFunction(format!("{script}.{name}")))?;
        self.invoke(func, None, &defined, args)
    }

    /// Sends the event `name` to `obj`: calls it when its script (or a
    /// parent) defines it, else does nothing, like the game.
    pub fn send_event(&mut self, obj: ObjId, name: &str, args: &[Value]) -> Result<(), VmError> {
        let script = self.objects[obj.0].script.clone();
        let state = self.objects[obj.0].state.clone();
        match self.resolve(&script, &state, &key(name)) {
            Some((func, defined)) => self.invoke(func, Some(obj), &defined, args).map(drop),
            None => Ok(()),
        }
    }

    /// Registers `obj` for `OnUpdate` (`OnUpdateGameTime` when `game_time`)
    /// in `seconds`, every `seconds` when `repeat`; replaces its previous
    /// registration, like the game.
    pub(crate) fn register_update(
        &mut self,
        obj: ObjId,
        seconds: f32,
        repeat: bool,
        game_time: bool,
    ) {
        self.unregister_update(obj, game_time);
        let seconds = seconds.max(0.0);
        self.timers.push(Timer {
            obj,
            game_time,
            due: self.clock + seconds,
            every: (repeat && seconds > 0.0).then_some(seconds),
        });
    }

    pub(crate) fn unregister_update(&mut self, obj: ObjId, game_time: bool) {
        self.timers.retain(|t| t.obj != obj || t.game_time != game_time);
    }

    /// Registers `obj` for the custom event `event`, calling `callback`; replaces
    /// its previous registration for it.
    pub(crate) fn register_mod_event(&mut self, obj: ObjId, event: &str, callback: String) {
        self.unregister_mod_event(obj, Some(event));
        self.mod_event_listeners.push(ModEventListener { obj, event: key(event), callback });
    }

    /// Unregisters `obj` from the custom event `event`, or from all of them.
    pub(crate) fn unregister_mod_event(&mut self, obj: ObjId, event: Option<&str>) {
        let event = event.map(key);
        self.mod_event_listeners
            .retain(|l| l.obj != obj || event.as_ref().is_some_and(|e| *e != l.event));
    }

    /// Calls the callbacks of the custom event `event`, in the order they
    /// registered. Unlike the game, where they run later, one thread runs
    /// everything: they run now. An error of one stops the run.
    pub(crate) fn send_mod_event(&mut self, event: &str, args: &[Value]) {
        let event = key(event);
        let listeners: Vec<(ObjId, String)> = (self.mod_event_listeners.iter())
            .filter(|l| l.event == event)
            .map(|l| (l.obj, l.callback.clone()))
            .collect();
        for (obj, callback) in listeners {
            if let Err(e) = self.send_event(obj, &callback, args) {
                self.abort = Some(e);
                return;
            }
        }
    }

    /// The object of `Game.GetPlayer`, of the script `Actor`, created when asked
    /// for; `None` without that script.
    pub(crate) fn player(&mut self) -> Option<ObjId> {
        if self.player.is_none() {
            self.player = self.new_object("Actor").ok();
        }
        self.player
    }

    /// How many native calls match `native`: `Function` of any script, or
    /// `Script.Function`, where `Script` may extend the script declaring it, like
    /// `Actor.Disable` for `ObjectReference.Disable`. `on`: only those on this
    /// object.
    pub(crate) fn count_calls(&mut self, native: &str, on: Option<ObjId>) -> usize {
        let (script, function) = match native.rsplit_once('.') {
            Some((script, function)) => (Some(script), function),
            None => (None, native),
        };
        let declaring: Option<Vec<String>> = script.map(|script| {
            let mut names: Vec<String> = self.chain(script).iter().map(|s| key(&s.name)).collect();
            names.push(key(script));
            names
        });
        (self.native_calls.iter())
            .filter(|c| c.function.eq_ignore_ascii_case(function))
            .filter(|c| on.is_none_or(|obj| c.this == Some(obj)))
            .filter(|c| declaring.as_ref().is_none_or(|names| names.contains(&key(&c.script))))
            .count()
    }

    /// Advances [`Vm::clock`] by `seconds`, sending on the way the due
    /// `OnUpdate` and `OnUpdateGameTime` events, in order.
    pub fn advance(&mut self, seconds: f32) -> Result<(), VmError> {
        let end = self.clock + seconds.max(0.0);
        loop {
            let next = (self.timers.iter().enumerate())
                .filter(|(_, t)| t.due <= end)
                .min_by(|(_, a), (_, b)| a.due.total_cmp(&b.due))
                .map(|(i, _)| i);
            let Some(i) = next else { break };
            let (obj, game_time, due) =
                (self.timers[i].obj, self.timers[i].game_time, self.timers[i].due);
            match self.timers[i].every {
                Some(every) => self.timers[i].due += every,
                None => drop(self.timers.remove(i)),
            }
            self.clock = self.clock.max(due);
            let event = if game_time { "OnUpdateGameTime" } else { "OnUpdate" };
            self.send_event(obj, event, &[])?;
        }
        self.clock = self.clock.max(end);
        Ok(())
    }

    /// Finds `name` up the chain of `script`, the functions of `state`
    /// first; returns it with the script defining it.
    fn resolve(&mut self, script: &str, state: &str, name: &str) -> Option<(Rc<Func>, String)> {
        self.chain(script).into_iter().find_map(|s| Some((s.function(state, name)?, key(&s.name))))
    }

    fn invoke(
        &mut self,
        func: Rc<Func>,
        this: Option<ObjId>,
        script: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        if func.native {
            return self.call_native(&func, this, script, args);
        }
        if self.depth >= 512 {
            return Err(VmError::StackOverflow);
        }
        let mut frame = Frame {
            locals: FxHashMap::default(),
            types: FxHashMap::default(),
            this,
            script: script.to_owned(),
        };
        for (i, (name, ty)) in func.params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or_else(|| Value::default_of(ty));
            frame.locals.insert(name.clone(), cast(self, value, ty));
            frame.types.insert(name.clone(), ty.clone());
        }
        for (name, ty) in &func.locals {
            frame.locals.insert(name.clone(), Value::default_of(ty));
            frame.types.insert(name.clone(), ty.clone());
        }
        let traced = self.debugger.is_some();
        if traced {
            let script = self.script(script).map_or_else(|| script.to_owned(), |s| s.name.clone());
            self.stack.push(StackFrame {
                script,
                function: func.name.clone(),
                line: None,
                this,
                locals: Vm::locals_of(&func, &frame),
            });
        }
        self.depth += 1;
        let result = self.run(&func, &mut frame);
        self.depth -= 1;
        if traced {
            self.stack.pop();
        }
        result
    }

    fn call_native(
        &mut self,
        func: &Func,
        this: Option<ObjId>,
        script: &str,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let script_name = self.script(script).map_or_else(|| script.to_owned(), |s| s.name.clone());
        let native_key = format!("{script}.{}", key(&func.name));
        // A mock runs instead, unless it is running already (a mock may call
        // the native it replaces).
        let mock =
            self.mocks.get(&native_key).cloned().filter(|_| !self.mocking.contains(&native_key));
        let native = self.natives.get(&native_key).cloned();
        self.native_calls.push(NativeCall {
            script: script_name,
            function: func.name.clone(),
            this,
            args: args.to_vec(),
            implemented: mock.is_some() || native.is_some(),
        });
        if let Some((obj, function)) = mock {
            let mock_script = self.objects[obj.0].script.clone();
            let state = self.objects[obj.0].state.clone();
            let Some((f, defined)) = self.resolve(&mock_script, &state, &key(&function)) else {
                return Err(VmError::UnknownFunction(function));
            };
            // One more parameter than the native: the object it is called on.
            let mut mock_args = args.to_vec();
            if f.params.len() == args.len() + 1 {
                mock_args.insert(0, this.map_or(Value::None, Value::Object));
            }
            self.mocking.insert(native_key.clone());
            let result = self.invoke(f, Some(obj), &defined, &mock_args);
            self.mocking.remove(&native_key);
            return result;
        }
        let value = match native {
            Some(f) => f(self, this, args),
            None => Value::default_of(&func.return_type),
        };
        match self.abort.take() {
            Some(e) => Err(e),
            None => Ok(value),
        }
    }

    /// Runs the function unction of obj instead of the native
    /// script.name ("Game.GetPlayer"), until [Vm::clear_mocks]. With one
    /// more parameter than the native, the first one is the object the native
    /// is called on.
    pub fn mock(&mut self, native: &str, obj: ObjId, function: &str) {
        self.mocks.insert(key(native), (obj, function.to_owned()));
    }

    /// Removes every mock.
    pub fn clear_mocks(&mut self) {
        self.mocks.clear();
    }

    fn run(&mut self, func: &Func, frame: &mut Frame) -> Result<Value, VmError> {
        let mut pc = 0usize;
        while let Some(inst) = func.code.get(pc) {
            if self.step_limit == 0 {
                return Err(VmError::StepLimit);
            }
            self.step_limit -= 1;
            self.trace(func, frame, pc)?;
            let a = &inst.args;
            let arg = |vm: &Vm, frame: &Frame, i: usize| vm.read(frame, &a[i]);
            let mut next = pc + 1;
            match inst.op {
                OpCode::Nop => {}
                OpCode::IAdd | OpCode::ISub | OpCode::IMul | OpCode::IDiv | OpCode::IMod => {
                    let (x, y) = (arg(self, frame, 1).as_int(), arg(self, frame, 2).as_int());
                    let v = match inst.op {
                        OpCode::IAdd => x.wrapping_add(y),
                        OpCode::ISub => x.wrapping_sub(y),
                        OpCode::IMul => x.wrapping_mul(y),
                        _ if y == 0 => {
                            self.errors.push("Divide by zero".to_owned());
                            0
                        }
                        OpCode::IDiv => x.wrapping_div(y),
                        _ => x.wrapping_rem(y),
                    };
                    self.write(frame, &a[0], Value::Int(v));
                }
                OpCode::FAdd | OpCode::FSub | OpCode::FMul | OpCode::FDiv => {
                    let (x, y) = (arg(self, frame, 1).as_float(), arg(self, frame, 2).as_float());
                    let v = match inst.op {
                        OpCode::FAdd => x + y,
                        OpCode::FSub => x - y,
                        OpCode::FMul => x * y,
                        _ => x / y,
                    };
                    self.write(frame, &a[0], Value::Float(v));
                }
                OpCode::Not => {
                    let v = !arg(self, frame, 1).truthy();
                    self.write(frame, &a[0], Value::Bool(v));
                }
                OpCode::INeg => {
                    let v = arg(self, frame, 1).as_int().wrapping_neg();
                    self.write(frame, &a[0], Value::Int(v));
                }
                OpCode::FNeg => {
                    let v = -arg(self, frame, 1).as_float();
                    self.write(frame, &a[0], Value::Float(v));
                }
                OpCode::Assign => {
                    let v = arg(self, frame, 1);
                    self.write(frame, &a[0], v);
                }
                OpCode::Cast => {
                    let v = arg(self, frame, 1);
                    let ty = self.type_of(frame, &a[0]);
                    let v = cast(self, v, &ty);
                    self.write(frame, &a[0], v);
                }
                OpCode::CmpEq | OpCode::CmpLt | OpCode::CmpLe | OpCode::CmpGt | OpCode::CmpGe => {
                    let (x, y) = (arg(self, frame, 1), arg(self, frame, 2));
                    use std::cmp::Ordering::*;
                    let v = match inst.op {
                        OpCode::CmpEq => x.loose_eq(&y),
                        OpCode::CmpLt => x.compare(&y) == Less,
                        OpCode::CmpLe => x.compare(&y) != Greater,
                        OpCode::CmpGt => x.compare(&y) == Greater,
                        _ => x.compare(&y) != Less,
                    };
                    self.write(frame, &a[0], Value::Bool(v));
                }
                OpCode::Jmp => next = jump(pc, &a[0]),
                OpCode::JmpT | OpCode::JmpF => {
                    if arg(self, frame, 0).truthy() == (inst.op == OpCode::JmpT) {
                        next = jump(pc, &a[1]);
                    }
                }
                OpCode::CallMethod => {
                    let name = operand_name(&a[0]);
                    let receiver = arg(self, frame, 1);
                    let args: Vec<Value> = a[4..].iter().map(|o| self.read(frame, o)).collect();
                    let v = match receiver {
                        Value::Object(obj) => {
                            let script = self.objects[obj.0].script.clone();
                            let state = self.objects[obj.0].state.clone();
                            match self.resolve(&script, &state, &name) {
                                Some((f, defined)) => self.invoke(f, Some(obj), &defined, &args)?,
                                // `GotoState` sends these whether the script
                                // handles them or not.
                                None if matches!(name.as_str(), "onbeginstate" | "onendstate") => {
                                    Value::None
                                }
                                None => {
                                    self.errors
                                        .push(format!("Unbound native function \"{name}\" called"));
                                    Value::None
                                }
                            }
                        }
                        _ => {
                            self.errors.push(format!("Cannot call {name}() on a None object"));
                            Value::None
                        }
                    };
                    self.write(frame, &a[2], v);
                }
                OpCode::CallParent => {
                    let name = operand_name(&a[0]);
                    let args: Vec<Value> = a[3..].iter().map(|o| self.read(frame, o)).collect();
                    let parent = self.script(&frame.script).and_then(|s| s.parent.clone());
                    let state =
                        frame.this.map(|o| self.objects[o.0].state.clone()).unwrap_or_default();
                    let v = match parent.and_then(|p| self.resolve(&p, &state, &name)) {
                        Some((f, defined)) => self.invoke(f, frame.this, &defined, &args)?,
                        None => Value::None,
                    };
                    self.write(frame, &a[1], v);
                }
                OpCode::CallStatic => {
                    let script = operand_name(&a[0]);
                    let name = operand_name(&a[1]);
                    let args: Vec<Value> = a[4..].iter().map(|o| self.read(frame, o)).collect();
                    let v = match self.resolve(&script, "", &name) {
                        Some((f, defined)) => self.invoke(f, None, &defined, &args)?,
                        None => {
                            self.errors.push(format!("Unknown function {script}.{name}"));
                            Value::None
                        }
                    };
                    self.write(frame, &a[2], v);
                }
                OpCode::Ret => {
                    let v = a.first().map(|o| self.read(frame, o)).unwrap_or_default();
                    return Ok(match v {
                        Value::None => Value::default_of(&func.return_type),
                        v => v,
                    });
                }
                OpCode::StrCat => {
                    let (x, y) = (arg(self, frame, 1), arg(self, frame, 2));
                    let v = format!("{}{}", self.text(&x), self.text(&y));
                    self.write(frame, &a[0], Value::Str(Rc::from(v)));
                }
                OpCode::PropGet => {
                    let name = operand_name(&a[0]);
                    let v = match arg(self, frame, 1) {
                        Value::Object(obj) => self.get_property(obj, &name)?,
                        _ => {
                            self.errors.push(format!("Cannot get {name} of a None object"));
                            Value::None
                        }
                    };
                    self.write(frame, &a[2], v);
                }
                OpCode::PropSet => {
                    let name = operand_name(&a[0]);
                    let value = arg(self, frame, 2);
                    match arg(self, frame, 1) {
                        Value::Object(obj) => self.set_property(obj, &name, value)?,
                        _ => self.errors.push(format!("Cannot set {name} of a None object")),
                    }
                }
                OpCode::ArrayCreate => {
                    let ty = self.type_of(frame, &a[0]);
                    let elem = ty.strip_suffix("[]").unwrap_or(&ty).to_owned();
                    let size = arg(self, frame, 1).as_int().clamp(0, 128) as usize;
                    let array = vec![Value::default_of(&elem); size];
                    self.write(frame, &a[0], Value::Array(Rc::new(std::cell::RefCell::new(array))));
                }
                OpCode::ArrayLength => {
                    let len = match arg(self, frame, 1) {
                        Value::Array(a) => a.borrow().len() as i32,
                        _ => 0,
                    };
                    self.write(frame, &a[0], Value::Int(len));
                }
                OpCode::ArrayGetElement => {
                    let index = arg(self, frame, 2).as_int();
                    let v = match arg(self, frame, 1) {
                        Value::Array(arr) => {
                            let arr = arr.borrow();
                            match usize::try_from(index).ok().and_then(|i| arr.get(i)) {
                                Some(v) => v.clone(),
                                None => {
                                    self.errors.push(format!(
                                        "Array index {index} is out of range (0-{})",
                                        arr.len() as i32 - 1
                                    ));
                                    Value::None
                                }
                            }
                        }
                        _ => Value::None,
                    };
                    self.write(frame, &a[0], v);
                }
                OpCode::ArraySetElement => {
                    let index = arg(self, frame, 1).as_int();
                    let value = arg(self, frame, 2);
                    if let Value::Array(arr) = arg(self, frame, 0) {
                        let mut arr = arr.borrow_mut();
                        let len = arr.len();
                        match usize::try_from(index).ok().filter(|&i| i < len) {
                            Some(i) => arr[i] = value,
                            None => self.errors.push(format!(
                                "Array index {index} is out of range (0-{})",
                                len as i32 - 1
                            )),
                        }
                    }
                }
                OpCode::ArrayFindElement | OpCode::ArrayRFindElement => {
                    let value = arg(self, frame, 2);
                    let start = arg(self, frame, 3).as_int();
                    let found = match arg(self, frame, 0) {
                        Value::Array(arr) => {
                            let arr = arr.borrow();
                            let len = arr.len() as i32;
                            let at = |i: i32| arr[i as usize].loose_eq(&value);
                            if inst.op == OpCode::ArrayFindElement {
                                (start.max(0)..len).find(|&i| at(i))
                            } else {
                                let from = if start < 0 { len - 1 } else { start.min(len - 1) };
                                (0..=from).rev().find(|&i| at(i))
                            }
                        }
                        _ => None,
                    };
                    self.write(frame, &a[1], Value::Int(found.unwrap_or(-1)));
                }
            }
            pc = next;
        }
        Ok(Value::default_of(&func.return_type))
    }

    fn get_property(&mut self, obj: ObjId, name: &str) -> Result<Value, VmError> {
        let script = self.objects[obj.0].script.clone();
        for s in self.chain(&script) {
            let Some(p) = s.properties.get(name) else { continue };
            if let Some(var) = &p.auto_var {
                return Ok(self.objects[obj.0].vars.get(var).cloned().unwrap_or_default());
            }
            if let Some(get) = p.getter.clone() {
                return self.invoke(get, Some(obj), &key(&s.name), &[]);
            }
        }
        self.errors.push(format!("Property {name} not found"));
        Ok(Value::None)
    }

    fn set_property(&mut self, obj: ObjId, name: &str, value: Value) -> Result<(), VmError> {
        let script = self.objects[obj.0].script.clone();
        for s in self.chain(&script) {
            let Some(p) = s.properties.get(name) else { continue };
            if let Some(var) = &p.auto_var {
                self.objects[obj.0].vars.insert(var.clone(), value);
                return Ok(());
            }
            if let Some(set) = p.setter.clone() {
                self.invoke(set, Some(obj), &key(&s.name), &[value])?;
                return Ok(());
            }
        }
        self.errors.push(format!("Property {name} not found"));
        Ok(())
    }

    fn read(&self, frame: &Frame, operand: &Operand) -> Value {
        let Operand::Name(name) = operand else { return constant(operand) };
        if let Some(v) = frame.locals.get(name) {
            return v.clone();
        }
        let this = frame.this.map(|o| &self.objects[o.0]);
        match name.as_str() {
            "self" => frame.this.map_or(Value::None, Value::Object),
            "::state" => this.map_or(Value::None, |o| Value::Str(Rc::from(o.state.as_str()))),
            _ => this.and_then(|o| o.vars.get(name)).cloned().unwrap_or_default(),
        }
    }

    fn write(&mut self, frame: &mut Frame, operand: &Operand, value: Value) {
        let Operand::Name(name) = operand else { return };
        if let Some(slot) = frame.locals.get_mut(name) {
            *slot = value;
            return;
        }
        let Some(this) = frame.this else { return };
        let object = &mut self.objects[this.0];
        if name == "::state" {
            object.state = value.to_string();
        } else {
            object.vars.insert(name.clone(), value);
        }
    }

    fn type_of(&self, frame: &Frame, operand: &Operand) -> String {
        let Operand::Name(name) = operand else { return String::new() };
        frame
            .types
            .get(name)
            .or_else(|| frame.this.and_then(|o| self.objects[o.0].types.get(name)))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `obj` is a `script` (or extends it).
    fn is_a(&mut self, obj: ObjId, script: &str) -> bool {
        let own = self.objects[obj.0].script.clone();
        let target = key(script);
        self.chain(&own).iter().any(|s| key(&s.name) == target)
    }
}

fn constant(operand: &Operand) -> Value {
    match operand {
        Operand::Name(_) | Operand::None => Value::None,
        Operand::Int(i) => Value::Int(*i),
        Operand::Float(f) => Value::Float(*f),
        Operand::Bool(b) => Value::Bool(*b),
        Operand::Str(s) => Value::Str(s.clone()),
    }
}

fn operand_name(operand: &Operand) -> String {
    match operand {
        Operand::Name(n) => n.clone(),
        _ => String::new(),
    }
}

fn jump(pc: usize, offset: &Operand) -> usize {
    let offset = match offset {
        Operand::Int(i) => *i as isize,
        _ => 1,
    };
    (pc as isize + offset).max(0) as usize
}

/// Converts `value` to the type `ty`, like `as` and implicit conversions.
fn cast(vm: &mut Vm, value: Value, ty: &str) -> Value {
    if ty.is_empty() || ty.ends_with("[]") {
        return value;
    }
    match ty.to_ascii_lowercase().as_str() {
        "int" => Value::Int(value.as_int()),
        "float" => Value::Float(value.as_float()),
        "bool" => Value::Bool(value.truthy()),
        "string" => match value {
            Value::Str(s) => Value::Str(s),
            other => Value::Str(Rc::from(vm.text(&other))),
        },
        "var" | "none" => value,
        _ => match value {
            // A failed cast between objects gives `None`.
            Value::Object(obj) if vm.is_a(obj, ty) => value,
            _ => Value::None,
        },
    }
}

#[cfg(test)]
mod tests;
