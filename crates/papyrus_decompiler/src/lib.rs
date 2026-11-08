//! Decompiler of compiled Papyrus scripts (`.pex`, Skyrim) to source.
//!
//! The declarations come back exactly: variables, properties (`Auto`,
//! `AutoReadOnly` and full ones), states, functions and their flags and
//! documentation. Bodies are rebuilt from the patterns compilers generate
//! (see [`body`]), which gives back the structure of the source; what the
//! compiled code does not record is lost:
//!
//! - comments, formatting and parameter default values (call sites pass
//!   every argument);
//! - whether a function was written as an `Event` (functions of no result
//!   named `On...` are printed as events);
//! - explicit conversions the compiler would insert anyway.
//!
//! The output compiles back to the same code, which is how it is tested.
//!
//! Names are borrowed from the string table of the file, and compared through
//! [`ScriptCx::key`], which ignores case like Papyrus does without lowercase
//! copies.

mod ast;
mod body;

use std::{
    borrow::Cow,
    fmt::Write as _,
    hash::{Hash, Hasher},
};

use papyrus_pex::{
    PexFile,
    pex::{FunctionInfo, Object, OpCode, Property, StringId, UserFlag, VariableValue},
};
use rustc_hash::FxHashMap;

use crate::ast::{Expr, Text, indent};

/// What the bodies of a script need to know about it.
pub(crate) struct ScriptCx<'p> {
    pex: &'p PexFile<'p>,
    /// The script name.
    script: Text<'p>,
    /// The key of every string of the string table: see [`ScriptCx::key`].
    keys: Vec<StringId>,
    /// The hidden variable of an auto property (`::Name_var`) to the property
    /// name, by key.
    auto_vars: FxHashMap<StringId, StringId>,
    /// Variables and properties to their type, by key.
    member_types: FxHashMap<StringId, StringId>,
}

impl<'p> ScriptCx<'p> {
    fn new(pex: &'p PexFile<'p>, object: &'p Object) -> ScriptCx<'p> {
        let mut cx = ScriptCx {
            pex,
            script: pex.str(object.name),
            keys: case_insensitive_keys(pex),
            auto_vars: FxHashMap::default(),
            member_types: FxHashMap::default(),
        };
        for p in &object.properties {
            if let Some(var) = p.auto_var_name {
                cx.auto_vars.insert(cx.key(var), p.name);
            }
            cx.member_types.insert(cx.key(p.name), p.ty);
        }
        for v in &object.variables {
            cx.member_types.insert(cx.key(v.name), v.type_name);
        }
        cx
    }

    /// A string of the string table.
    fn str(&self, id: StringId) -> Text<'p> {
        self.pex.str(id)
    }

    /// The same key for strings that differ only in ASCII case: the first
    /// entry of the string table spelled that way.
    fn key(&self, id: StringId) -> StringId {
        self.keys.get(id.index()).copied().unwrap_or(id)
    }
}

/// [`ScriptCx::key`] of every entry of the string table.
fn case_insensitive_keys(pex: &PexFile<'_>) -> Vec<StringId> {
    /// Bytes compared and hashed ignoring ASCII case.
    struct NoCase<'s>(&'s [u8]);
    impl PartialEq for NoCase<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.0.eq_ignore_ascii_case(other.0)
        }
    }
    impl Eq for NoCase<'_> {}
    impl Hash for NoCase<'_> {
        fn hash<H: Hasher>(&self, state: &mut H) {
            for b in self.0 {
                state.write_u8(b.to_ascii_lowercase());
            }
        }
    }

    let mut first = FxHashMap::default();
    pex.string_table
        .iter()
        .enumerate()
        .map(|(i, s)| *first.entry(NoCase(s.as_bytes())).or_insert(StringId(i as u16)))
        .collect()
}

/// Decompiles every script of a file (there is one in files of the game).
pub fn decompile(pex: &PexFile<'_>) -> String {
    let mut out = String::new();
    for (i, object) in pex.objects.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        Writer::new(pex, object).script(&mut out);
    }
    out
}

struct Writer<'p> {
    cx: ScriptCx<'p>,
    object: &'p Object,
    /// The user flags of the file, by bit index.
    flags: Vec<&'p UserFlag>,
}

const GET_STATE_DOC: &str = "Function that returns the current state";
const GOTO_STATE_DOC: &str = "Function that switches this object to the specified state";

impl<'p> Writer<'p> {
    fn new(pex: &'p PexFile<'p>, object: &'p Object) -> Writer<'p> {
        let mut flags: Vec<&UserFlag> = pex.user_flags.iter().collect();
        flags.sort_by_key(|f| f.flag_index);
        Writer { cx: ScriptCx::new(pex, object), object, flags }
    }

    fn s(&self, id: StringId) -> Text<'p> {
        self.cx.str(id)
    }

    /// The names of the user flags set in `bits`, each with a leading space.
    fn flags(&self, out: &mut String, bits: u32) {
        for def in &self.flags {
            if def.flag_index < 32 && bits & (1 << def.flag_index) != 0 {
                // The official compiler writes them lowercase: capitalized
                // here, like the game scripts spell them.
                let name = self.s(def.name);
                let mut chars = name.chars();
                out.push(' ');
                out.extend(chars.next().map(|c| c.to_ascii_uppercase()));
                out.push_str(chars.as_str());
            }
        }
    }

    /// `{documentation}` on its own line, when there is one.
    fn docstring(&self, out: &mut String, id: StringId, depth: usize) {
        let doc = self.s(id);
        if !doc.is_empty() {
            indent(out, depth);
            let _ = writeln!(out, "{{{doc}}}");
        }
    }

    /// ` = value` for a constant that is not `None`.
    fn initial_value(&self, out: &mut String, v: &VariableValue) {
        let e = match *v {
            VariableValue::Null => return,
            VariableValue::Identifier(id) | VariableValue::String(id) => Expr::Str(self.s(id)),
            VariableValue::Integer(i) => Expr::Int(i),
            VariableValue::Float(f) => Expr::Float(f),
            VariableValue::Boolean(b) => Expr::Bool(b),
        };
        out.push_str(" = ");
        e.write(out);
    }

    fn script(&self, out: &mut String) {
        let obj = self.object;
        let _ = write!(out, "ScriptName {}", self.cx.script);
        let parent = self.s(obj.parent_class_name);
        if !parent.is_empty() {
            let _ = write!(out, " extends {parent}");
        }
        self.flags(out, obj.user_flags.bits());
        out.push('\n');
        self.docstring(out, obj.docstring, 0);

        let section = |out: &mut String| out.push('\n');

        // The variables of auto properties are written as the properties.
        let declared =
            |v: &&papyrus_pex::pex::Variable| !self.cx.auto_vars.contains_key(&self.cx.key(v.name));
        if obj.variables.iter().any(|v| declared(&v)) {
            section(out);
        }
        for v in obj.variables.iter().filter(declared) {
            let _ = write!(out, "{} {}", self.s(v.type_name), self.s(v.name));
            self.initial_value(out, &v.value);
            self.flags(out, v.user_flags);
            out.push('\n');
        }

        for p in &obj.properties {
            section(out);
            self.property(out, p);
        }

        // The functions of the empty state, then the states.
        let is_empty = |id: StringId| self.s(id).is_empty();
        for st in obj.states.iter().filter(|st| is_empty(st.name)) {
            for f in &st.functions {
                let name = self.s(f.name);
                if self.is_generated(&name, &f.info) {
                    continue;
                }
                section(out);
                self.function(out, &name, &f.info, 0);
            }
        }
        let auto = self.s(obj.auto_state_name);
        for st in obj.states.iter().filter(|st| !is_empty(st.name)) {
            let name = self.s(st.name);
            section(out);
            if name.eq_ignore_ascii_case(&auto) {
                out.push_str("Auto ");
            }
            let _ = writeln!(out, "State {name}");
            for (i, f) in st.functions.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                self.function(out, &self.s(f.name), &f.info, 1);
            }
            out.push_str("EndState\n");
        }
    }

    /// `GetState` and `GotoState` as the compiler generates them.
    fn is_generated(&self, name: &str, info: &FunctionInfo) -> bool {
        let doc = self.s(info.docstring);
        (name.eq_ignore_ascii_case("GetState")
            && doc == GET_STATE_DOC
            && info.instructions.len() == 1)
            || (name.eq_ignore_ascii_case("GotoState")
                && doc == GOTO_STATE_DOC
                && info.instructions.len() == 3)
    }

    fn property(&self, out: &mut String, p: &'p Property) {
        let ty = self.s(p.ty);
        let name = self.s(p.name);
        // `Auto`: the value and some flags are those of the hidden variable.
        if let Some(var) = p.auto_var_name {
            let var = self.object.variables.iter().find(|v| v.name == var);
            let _ = write!(out, "{ty} Property {name}");
            if let Some(v) = var {
                self.initial_value(out, &v.value);
            }
            out.push_str(" Auto");
            self.flags(out, p.user_flags);
            if let Some(v) = var {
                self.flags(out, v.user_flags);
            }
            out.push('\n');
            self.docstring(out, p.docstring, 0);
            return;
        }
        // `AutoReadOnly`: a getter returning a constant, without setter.
        if let (Some(get), None) = (&p.read_handler, &p.write_handler)
            && let [inst] = get.instructions.as_slice()
            && inst.opcode == OpCode::Ret
            && !matches!(inst.args[0], VariableValue::Identifier(_))
        {
            let _ = write!(out, "{ty} Property {name}");
            self.initial_value(out, &inst.args[0]);
            out.push_str(" AutoReadOnly");
            self.flags(out, p.user_flags);
            out.push('\n');
            self.docstring(out, p.docstring, 0);
            return;
        }
        // Full: `Get` and `Set`.
        let _ = write!(out, "{ty} Property {name}");
        self.flags(out, p.user_flags);
        out.push('\n');
        self.docstring(out, p.docstring, 0);
        if let Some(get) = &p.read_handler {
            self.function(out, "Get", get, 1);
        }
        if let Some(set) = &p.write_handler {
            self.function(out, "Set", set, 1);
        }
        out.push_str("EndProperty\n");
    }

    fn function(&self, out: &mut String, name: &str, info: &'p FunctionInfo, depth: usize) {
        let ret = self.s(info.return_type);
        let no_result = ret.eq_ignore_ascii_case("None");
        let event = no_result && name.len() > 2 && name[..2].eq_ignore_ascii_case("on");
        indent(out, depth);
        if event {
            out.push_str("Event ");
        } else {
            if !no_result {
                let _ = write!(out, "{ret} ");
            }
            out.push_str("Function ");
        }
        let _ = write!(out, "{name}(");
        for (i, p) in info.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{} {}", self.s(p.ty), self.s(p.name));
        }
        out.push(')');
        if info.is_global() {
            out.push_str(" Global");
        }
        if info.is_native() {
            out.push_str(" Native");
        }
        self.flags(out, info.user_flags);
        out.push('\n');
        self.docstring(out, info.docstring, depth);
        if info.is_native() {
            return;
        }
        let stmts = body::decompile(&self.cx, info);
        ast::write_block(out, &stmts, depth + 1);
        indent(out, depth);
        out.push_str(if event { "EndEvent\n" } else { "EndFunction\n" });
    }
}

/// The part of `text` that `pick` selects: borrowed when `text` is.
pub(crate) fn sub_text<'p>(text: Text<'p>, pick: impl FnOnce(&str) -> &str) -> Text<'p> {
    match text {
        Cow::Borrowed(s) => Cow::Borrowed(pick(s)),
        Cow::Owned(s) => Cow::Owned(pick(&s).to_owned()),
    }
}

#[cfg(test)]
mod tests;
