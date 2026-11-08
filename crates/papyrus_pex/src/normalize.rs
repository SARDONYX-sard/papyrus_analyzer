//! A canonical text of a `.pex` file, to compare the output of two compilers.
//!
//! Two compilations of the same source differ in ways that do not matter to
//! the game: the order of tables (the official compiler uses hash order), the
//! spelling case of names (string table entries are shared ignoring case),
//! the numbering of temporaries, and the header. [`normalize`] removes these
//! differences: tables are sorted, everything is lowercase, temporaries are
//! renumbered by first use in each function and left out of the local table,
//! and the header and line numbers are dropped.
//!
//! Every table is rendered as one `String` per entry, which are then sorted:
//! those are the only allocations besides the output.

use std::{
    borrow::Cow,
    fmt::{self, Write as _},
};

use crate::{disasm::jump_target, pex::*};

/// Renders the canonical text of `pex`.
pub fn normalize(pex: &PexFile<'_>) -> String {
    normalize_with(pex, false)
}

/// Like [`normalize`]; with `merge_temps`, all temporaries have the same
/// name, which hides differences in how temporaries are reused.
pub fn normalize_with(pex: &PexFile<'_>, merge_temps: bool) -> String {
    let n = Normalizer { pex, merge_temps };
    let mut out = String::new();
    for obj in &pex.objects {
        n.object(&mut out, obj);
    }
    out
}

struct Normalizer<'p, 'a> {
    pex: &'p PexFile<'a>,
    merge_temps: bool,
}

impl<'p> Normalizer<'p, '_> {
    /// A name, lowercase.
    fn name(&self, id: StringId) -> impl fmt::Display + 'p {
        lowercase(self.pex.str(id))
    }

    /// A free text (documentation, state name, string literal), lowercase
    /// and quoted.
    fn text(&self, id: StringId) -> String {
        format!("{:?}", self.pex.str(id).to_lowercase())
    }

    fn object(&self, out: &mut String, obj: &Object) {
        let _ = writeln!(
            out,
            "object {} extends {} flags {} auto {} doc {}",
            self.name(obj.name),
            self.name(obj.parent_class_name),
            obj.user_flags.bits(),
            self.name(obj.auto_state_name),
            self.text(obj.docstring)
        );
        sorted_lines(out, obj.variables.iter().map(|v| self.variable(v)));
        sorted_lines(out, obj.properties.iter().map(|p| self.property(p)));
        sorted_lines(out, obj.states.iter().map(|s| self.state(s)));
    }

    fn variable(&self, v: &Variable) -> String {
        let mut text = String::new();
        let _ = write!(
            text,
            "  var {} {} flags {} = ",
            self.name(v.name),
            self.name(v.type_name),
            v.user_flags
        );
        self.value(&mut text, &v.value, &mut Vec::new());
        text
    }

    fn property(&self, p: &Property) -> String {
        let mut text = String::new();
        let _ = write!(
            text,
            "  property {} {} flags {} uflags {} doc {}",
            self.name(p.name),
            self.name(p.ty),
            p.flags.bits(),
            p.user_flags,
            self.text(p.docstring)
        );
        if let Some(var) = p.auto_var_name {
            let _ = write!(text, " auto {}", self.name(var));
        }
        if let Some(f) = &p.read_handler {
            text.push('\n');
            self.function(&mut text, "get", f);
        }
        if let Some(f) = &p.write_handler {
            text.push('\n');
            self.function(&mut text, "set", f);
        }
        text
    }

    fn state(&self, st: &State) -> String {
        let mut functions: Vec<String> = st
            .functions
            .iter()
            .map(|f| {
                let mut text = String::new();
                self.function(&mut text, &self.pex.str(f.name), &f.info);
                text
            })
            .collect();
        functions.sort();
        let mut text = format!("  state {}", self.text(st.name));
        for f in functions {
            text.push('\n');
            text.push_str(&f);
        }
        text
    }

    fn function(&self, out: &mut String, name: &str, f: &FunctionInfo) {
        let _ = write!(
            out,
            "    function {} -> {} flags {} uflags {} doc {}",
            lowercase(name),
            self.name(f.return_type),
            f.flags.bits(),
            f.user_flags,
            self.text(f.docstring)
        );
        for p in &f.params {
            let _ = write!(out, "\n      param {} {}", self.name(p.name), self.name(p.ty));
        }
        // Temporaries are renumbered below: only the named locals.
        let mut locals: Vec<String> = f
            .locals
            .iter()
            .filter(|l| !is_temp(&self.pex.str(l.name)))
            .map(|l| format!("\n      local {} {}", self.name(l.name), self.name(l.ty)))
            .collect();
        locals.sort();
        for local in locals {
            out.push_str(&local);
        }

        let mut targets: Vec<usize> = f
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(i, inst)| jump_target(i, inst))
            .collect();
        targets.sort_unstable();
        targets.dedup();
        // The temporaries in order of first use: `::t0`, `::t1`...
        let mut temps = Vec::new();
        for (i, inst) in f.instructions.iter().enumerate() {
            if targets.binary_search(&i).is_ok() {
                let _ = write!(out, "\n      @{i}:");
            }
            let _ = write!(out, "\n        {}", inst.opcode.mnemonic());
            let jump = matches!(inst.opcode, OpCode::Jmp | OpCode::JmpT | OpCode::JmpF);
            for (k, arg) in inst.args.iter().enumerate() {
                out.push(' ');
                if jump && k + 1 == inst.args.len() {
                    let _ = write!(out, "@{}", jump_target(i, inst).unwrap_or(usize::MAX));
                } else {
                    self.value(out, arg, &mut temps);
                }
            }
        }
    }

    /// An operand; temporaries by their number in `temps`, where the first
    /// use adds them.
    fn value(&self, out: &mut String, v: &VariableValue, temps: &mut Vec<Cow<'p, str>>) {
        let _ = match *v {
            VariableValue::Null => write!(out, "none"),
            VariableValue::Identifier(id) => {
                let name = self.pex.str(id);
                if !is_temp(&name) {
                    write!(out, "{}", lowercase(name))
                } else if self.merge_temps {
                    write!(out, "::t")
                } else {
                    let k = match temps.iter().position(|t| t.eq_ignore_ascii_case(&name)) {
                        Some(k) => k,
                        None => {
                            temps.push(name);
                            temps.len() - 1
                        }
                    };
                    write!(out, "::t{k}")
                }
            }
            VariableValue::String(id) => write!(out, "{}", self.text(id)),
            VariableValue::Integer(i) => write!(out, "{i}"),
            VariableValue::Float(f) => write!(out, "{f:?}"),
            VariableValue::Boolean(b) => write!(out, "{b}"),
        };
    }
}

/// Sorts `items`, then writes one per line.
fn sorted_lines(out: &mut String, items: impl Iterator<Item = String>) {
    let mut items: Vec<String> = items.collect();
    items.sort();
    for item in items {
        out.push_str(&item);
        out.push('\n');
    }
}

/// `::tempN`, ignoring case.
fn is_temp(name: &str) -> bool {
    name.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("::temp"))
        && name[6..].bytes().all(|b| b.is_ascii_digit())
}

/// `text` written lowercase, without a lowercase copy.
fn lowercase<'t>(text: impl AsRef<str> + 't) -> impl fmt::Display + 't {
    fmt::from_fn(move |f| {
        for c in text.as_ref().chars() {
            for lower in c.to_lowercase() {
                f.write_char(lower)?;
            }
        }
        Ok(())
    })
}
