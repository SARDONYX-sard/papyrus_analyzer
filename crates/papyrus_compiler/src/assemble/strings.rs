//! The string table of a `.pex` file.

use rustc_hash::FxHashMap;

use papyrus_ide::index::TypeRef;
use papyrus_intern::Name;
use papyrus_pex::{
    PexStr,
    pex::{StringId, VariableValue},
};

use crate::codegen::ir::Operand;

/// Interns the strings of a file.
///
/// Like the official compiler, entries are shared ignoring ASCII case: the
/// first spelling wins. The game looks identifiers up ignoring case anyway,
/// but string *literals* are affected too: `"busy"` reads `"Busy"` when
/// `"Busy"` was interned first. This is kept for compatibility.
///
/// Lookups allocate nothing: keys are lowercased on the stack, and names and
/// types, which are looked up over and over, have their own caches.
#[derive(Default)]
pub(crate) struct StringTable {
    /// The entries, in the order of the file.
    strings: Vec<String>,
    /// Lowercase entry to its index.
    index: FxHashMap<Box<str>, StringId>,
    /// Cache of [`StringTable::name`] (`Name` already ignores case).
    names: FxHashMap<Name, StringId>,
    /// Cache of [`StringTable::type_name`].
    types: FxHashMap<Option<TypeRef>, StringId>,
}

impl StringTable {
    /// The entry of `s`, added when new.
    pub(crate) fn id(&mut self, s: &str) -> StringId {
        if let Some(id) = with_ascii_lowercase(s, |key| self.index.get(key).copied()) {
            return id;
        }
        let id = StringId(self.strings.len() as u16);
        self.strings.push(s.to_owned());
        self.index.insert(s.to_ascii_lowercase().into_boxed_str(), id);
        id
    }

    /// The entry of a name.
    pub(crate) fn name(&mut self, name: Name) -> StringId {
        if let Some(&id) = self.names.get(&name) {
            return id;
        }
        let id = self.id(name.as_str());
        self.names.insert(name, id);
        id
    }

    /// The entry of a type name: `Int`, `Actor[]`, `None` for `None`.
    pub(crate) fn type_name(&mut self, ty: Option<TypeRef>) -> StringId {
        if let Some(&id) = self.types.get(&ty) {
            return id;
        }
        let id = match ty {
            Some(t) => self.id(&t.to_string()),
            None => self.id("None"),
        };
        self.types.insert(ty, id);
        id
    }

    /// An operand as a value of the file.
    pub(crate) fn value(&mut self, operand: &Operand) -> VariableValue {
        match operand {
            Operand::Var(v) => VariableValue::Identifier(self.name(*v)),
            Operand::None => VariableValue::Null,
            Operand::Int(i) => VariableValue::Integer(*i),
            Operand::Float(f) => VariableValue::Float(*f),
            Operand::Bool(b) => VariableValue::Boolean(*b),
            Operand::Str(s) => VariableValue::String(self.id(s)),
        }
    }

    pub(crate) fn get(&self, id: StringId) -> Option<&str> {
        self.strings.get(id.index()).map(String::as_str)
    }

    pub(crate) fn into_table(self) -> Vec<PexStr<'static>> {
        self.strings.iter().map(|s| PexStr::encode(s)).collect()
    }
}

/// Calls `f` with `s` in ASCII lowercase: `s` itself when it has no
/// uppercase, else a copy on the stack (on the heap only past 64 bytes).
fn with_ascii_lowercase<R>(s: &str, f: impl FnOnce(&str) -> R) -> R {
    if !s.bytes().any(|b| b.is_ascii_uppercase()) {
        return f(s);
    }
    let mut buf = [0u8; 64];
    match buf.get_mut(..s.len()) {
        Some(buf) => {
            buf.copy_from_slice(s.as_bytes());
            buf.make_ascii_lowercase();
            f(std::str::from_utf8(buf).expect("ASCII lowercasing keeps UTF-8"))
        }
        None => f(&s.to_ascii_lowercase()),
    }
}
