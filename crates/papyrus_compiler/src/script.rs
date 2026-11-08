//! Assembly of a script into a [`PexFile`]: variables, properties, states,
//! the string table and the debug information.
//!
//! The source is walked once, in order. Names are kept as the `&str` of the
//! source text until they go into the string table: the order in which
//! strings are first interned is the order of the table in the file.

use std::borrow::Cow;

use papyrus_ide::{
    flags::FlagTargets,
    hir::Body,
    index::{TypeRef, doc_ranges},
    sema::Sema,
    typeck,
};
use papyrus_parser::{
    SyntaxNode,
    ast::{self, AstNode},
};
use papyrus_pex::{
    PexFile, PexStr,
    pex::{
        DebugFunction, DebugFunctionType, DebugInfo, Function, FunctionFlags, FunctionInfo,
        GameType, Instruction, LE_MAGIC_NUMBER, Object, ObjectFlags, OpCode, Property,
        PropertyFlags, State, StringId, UserFlag, Variable, VariableType, VariableValue,
        property_auto_var_name,
    },
};

use crate::{
    CompileOptions,
    codegen::{self, ScriptCx, convert_const},
    ir::{FunctionIr, Inst, Operand, Slot},
    literal, log, opt,
    strings::StringTable,
};

/// What the source file says about itself, for the header.
pub(crate) struct SourceInfo {
    /// The file name, e.g. `Foo.psc`.
    pub(crate) file_name: String,
    /// The modification time, in seconds since 1970.
    pub(crate) modified: i64,
}

pub(crate) fn assemble(
    sema: Sema<'_>,
    source: &SourceInfo,
    options: &CompileOptions,
) -> PexFile<'static> {
    let mut a = Assembler {
        cx: ScriptCx::new(sema),
        options,
        strings: StringTable::default(),
        debug: Vec::new(),
        variables: Vec::new(),
        properties: Vec::new(),
        states: vec![("", Vec::new())],
        auto_state: "",
    };
    // Interned first so that `::State` and friends keep their spelling.
    let script_name = sema.index.name_str();
    a.strings.id(script_name);

    let root = ast::SourceFile::from_root(sema.root);
    let mut header = None;
    for item in root.items() {
        match item {
            ast::Item::Header(h) => header = header.or(Some(h)),
            other => a.item(other, None),
        }
    }
    // Every script gets them, with or without states.
    a.state_functions();

    let (parent, docstring, user_flags) = match header {
        Some(h) => (
            h.extends().map_or("", |e| e.text()),
            a.docstring(h.syntax()),
            a.user_flags(h.flags(), FlagTargets::SCRIPT),
        ),
        None => ("", Cow::Borrowed(""), 0),
    };

    let states = std::mem::take(&mut a.states)
        .into_iter()
        .map(|(name, functions)| State { name: a.strings.id(name), functions })
        .collect();
    let object = Object {
        name: a.strings.id(script_name),
        size: 0,
        parent_class_name: a.strings.id(parent),
        docstring: a.strings.id(&docstring),
        user_flags: ObjectFlags::from_bits_retain(user_flags),
        auto_state_name: a.strings.id(a.auto_state),
        variables: std::mem::take(&mut a.variables),
        properties: std::mem::take(&mut a.properties),
        states,
    };
    log::debug!(
        "object {script_name}: {} variables, {} properties, {} states, {} temporaries",
        object.variables.len(),
        object.properties.len(),
        object.states.len(),
        a.cx.temps
    );

    let flags = sema.ws.flags_of(sema.file);
    let user_flags_table = flags
        .defs
        .iter()
        .map(|d| UserFlag { name: a.strings.name(d.name), flag_index: d.index as u8 })
        .collect();
    let debug_info = options.debug_info.then(|| DebugInfo {
        modification_time: source.modified,
        functions: std::mem::take(&mut a.debug),
    });
    let src = PexStr::encode(&source.file_name);
    let user = PexStr::encode(
        &std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default(),
    );
    let machine = PexStr::encode(
        &std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default(),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    PexFile {
        magic_number: LE_MAGIC_NUMBER,
        major_version: 3,
        minor_version: 2,
        game_id: GameType::Skyrim,
        compilation_time: now,
        src_file_name: src,
        user_name: user,
        machine_name: machine,
        string_table: a.strings.into_table(),
        debug_info,
        user_flags: user_flags_table,
        objects: vec![object],
    }
}

/// `'a` is the lifetime of the source text: names borrow it.
struct Assembler<'o, 'a> {
    cx: ScriptCx<'a>,
    options: &'o CompileOptions,
    strings: StringTable,
    debug: Vec<DebugFunction>,
    variables: Vec<Variable>,
    properties: Vec<Property>,
    /// States in source order, the empty state (`""`) first.
    states: Vec<(&'a str, Vec<Function>)>,
    /// The `Auto` state, `""` for none.
    auto_state: &'a str,
}

impl<'a> Assembler<'_, 'a> {
    fn text(&self) -> &'a str {
        self.cx.sema.text
    }

    /// The `{ ... }` documentation of a declaration.
    fn docstring(&self, node: SyntaxNode<'_>) -> Cow<'a, str> {
        // Only after the header: a `{ ... }` above belongs to the previous
        // declaration.
        let start = node.text_range().start();
        // The text between the braces is kept as written, indentation
        // included, like the official compiler does.
        let text = self.text();
        doc_ranges(node)
            .into_iter()
            .filter(|r| r.start() >= start)
            .find_map(|r| {
                let raw = &text[usize::from(r.start())..usize::from(r.end())];
                let inner = raw.strip_prefix('{')?;
                let inner = inner.strip_suffix('}').unwrap_or(inner);
                Some(if inner.contains("\r\n") {
                    Cow::Owned(inner.replace("\r\n", "\n").trim().to_owned())
                } else {
                    Cow::Borrowed(inner.trim())
                })
            })
            .unwrap_or_default()
    }

    /// The user flag bits of `flags` that apply to `target`.
    fn user_flags<'f>(
        &self,
        flags: impl Iterator<Item = ast::Flag<'f>>,
        target: FlagTargets,
    ) -> u32 {
        let table = self.cx.sema.ws.flags_of(self.cx.sema.file);
        let mut bits = 0;
        for flag in flags {
            if let Some((_, def)) = table.get(flag.text())
                && def.targets.intersects(target)
                && def.index < 32
            {
                bits |= 1 << def.index;
            }
        }
        bits
    }

    /// The index of state `name` in `states`, added when new.
    fn state_index(&mut self, name: &'a str) -> usize {
        match self.states.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None => {
                self.states.push((name, Vec::new()));
                self.states.len() - 1
            }
        }
    }

    /// A declaration of the script, or of the state `state` (`""`: the empty
    /// state).
    fn item(&mut self, item: ast::Item<'a>, state: Option<&'a str>) {
        match item {
            ast::Item::Header(_) | ast::Item::Import(_) => {}
            ast::Item::VarDecl(var) => {
                let Some(name) = var.name() else { return };
                let ty = var.ty().and_then(TypeRef::from_ast);
                let value = self.initial_value(var.initializer(), ty);
                let user_flags = self.user_flags(var.flags(), FlagTargets::VARIABLE);
                let variable = Variable {
                    name: self.strings.id(name.text()),
                    type_name: self.strings.type_name(ty),
                    user_flags,
                    value,
                };
                self.variables.push(variable);
            }
            ast::Item::Property(prop) => self.property(prop),
            ast::Item::State(st) => {
                let Some(name) = st.name() else { return };
                let name = name.text();
                self.state_index(name);
                if st.is_auto() && self.auto_state.is_empty() {
                    self.auto_state = name;
                }
                for inner in st.items() {
                    self.item(inner, Some(name));
                }
            }
            ast::Item::Function(f) => {
                let Some(name) = f.name() else { return };
                let info = self.function_info(f.syntax());
                self.push_function(state.unwrap_or(""), name.text(), info);
            }
            ast::Item::Event(e) => {
                let Some(name) = e.name() else { return };
                let info = self.function_info(e.syntax());
                self.push_function(state.unwrap_or(""), name.text(), info);
            }
        }
    }

    fn push_function(
        &mut self,
        state: &'a str,
        name: &str,
        (info, lines): (FunctionInfo, Vec<u16>),
    ) {
        let has_code = !info.is_native();
        let index = self.state_index(state);
        let name_id = self.strings.id(name);
        self.states[index].1.push(Function { name: name_id, info });
        if has_code {
            self.debug_entry(state, name, DebugFunctionType::Method, lines);
        }
    }

    /// The source line of every instruction of a function.
    fn debug_entry(&mut self, state: &str, name: &str, kind: DebugFunctionType, lines: Vec<u16>) {
        if !self.options.debug_info {
            return;
        }
        let entry = DebugFunction {
            object_name: self.strings.id(self.cx.sema.index.name_str()),
            state_name: self.strings.id(state),
            function_name: self.strings.id(name),
            function_type: kind,
            instruction_line_numbers: lines,
        };
        self.debug.push(entry);
    }

    /// The `= value` of a variable or property, converted to its type.
    fn initial_value(&mut self, init: Option<ast::Expr<'_>>, ty: Option<TypeRef>) -> VariableValue {
        let Some(init) = init else { return VariableValue::Null };
        let range = init.syntax().trimmed_range();
        let text = &self.text()[usize::from(range.start())..usize::from(range.end())];
        let Some(value) = literal::parse(text) else { return VariableValue::Null };
        let value = ty.and_then(|t| convert_const(&value, t)).unwrap_or(value);
        self.strings.value(&value)
    }

    /// A property: `AutoReadOnly` ones get a getter returning the value,
    /// `Auto` ones a hidden variable, full ones their `Get` and `Set`.
    fn property(&mut self, prop: ast::Property<'a>) {
        let Some(name) = prop.name() else { return };
        let name = name.text();
        let ty = prop.ty().and_then(TypeRef::from_ast);
        let docstring = self.docstring(prop.syntax());
        let user_flags = self.user_flags(prop.flags(), FlagTargets::PROPERTY);
        let mut property = Property {
            name: self.strings.id(name),
            ty: self.strings.type_name(ty),
            docstring: self.strings.id(&docstring),
            user_flags,
            flags: PropertyFlags::empty(),
            auto_var_name: None,
            read_handler: None,
            write_handler: None,
        };
        if prop.is_read_only() {
            let line = self.cx.line(prop.syntax().trimmed_range());
            let value = self.initial_value(prop.initializer(), ty);
            let info = FunctionInfo {
                return_type: self.strings.type_name(ty),
                docstring: self.strings.id(""),
                user_flags: 0,
                flags: FunctionFlags::empty(),
                params: Vec::new(),
                locals: Vec::new(),
                instructions: vec![Instruction { opcode: OpCode::Ret, args: vec![value] }],
            };
            property.flags = PropertyFlags::READ;
            property.read_handler = Some(info);
            self.debug_entry("", name, DebugFunctionType::Getter, vec![line as u16]);
        } else if prop.is_auto() {
            let var = property_auto_var_name(name);
            // Flags only allowed on variables (`Conditional`) go to the
            // hidden variable.
            let table = self.cx.sema.ws.flags_of(self.cx.sema.file);
            let var_flags = prop
                .flags()
                .filter_map(|f| table.get(f.text()))
                .filter(|(_, d)| {
                    !d.targets.intersects(FlagTargets::PROPERTY)
                        && d.targets.intersects(FlagTargets::VARIABLE)
                        && d.index < 32
                })
                .fold(0, |bits, (_, d)| bits | 1 << d.index);
            let value = self.initial_value(prop.initializer(), ty);
            let variable = Variable {
                name: self.strings.id(&var),
                type_name: self.strings.type_name(ty),
                user_flags: var_flags,
                value,
            };
            self.variables.push(variable);
            property.flags = PropertyFlags::READ | PropertyFlags::WRITE | PropertyFlags::AUTO_VAR;
            property.auto_var_name = Some(self.strings.id(&var));
        } else {
            for f in prop.functions() {
                let Some(fname) = f.name() else { continue };
                let (info, lines) = self.function_info(f.syntax());
                if fname.text().eq_ignore_ascii_case("Get") {
                    property.flags |= PropertyFlags::READ;
                    property.read_handler = Some(info);
                    self.debug_entry("", name, DebugFunctionType::Getter, lines);
                } else if fname.text().eq_ignore_ascii_case("Set") {
                    property.flags |= PropertyFlags::WRITE;
                    property.write_handler = Some(info);
                    self.debug_entry("", name, DebugFunctionType::Setter, lines);
                }
            }
        }
        self.properties.push(property);
    }

    /// Compiles a `FUNCTION` or `EVENT` node, and returns the source line of
    /// each instruction.
    fn function_info(&mut self, node: SyntaxNode<'_>) -> (FunctionInfo, Vec<u16>) {
        let (ret, global, native, flags, params) = if let Some(f) = ast::Function::cast(node) {
            let ret = f.return_type().and_then(TypeRef::from_ast);
            let user = self.user_flags(f.flags(), FlagTargets::FUNCTION);
            (ret, f.is_global(), f.is_native(), user, f.param_list())
        } else {
            let e = ast::Event::cast(node).expect("function or event");
            let user = self.user_flags(e.flags(), FlagTargets::FUNCTION);
            (None, false, e.is_native(), user, e.param_list())
        };
        let sema = self.cx.sema;
        let ir = match Body::lower(&sema, node) {
            Some(body) => {
                let types = typeck::check_body(&sema, &body);
                let mut ir = codegen::function(&mut self.cx, &body, &types);
                let name = node.children().find_map(ast::Name::cast).map_or("", |n| n.text());
                log::debug!(
                    "function {name}: {} instructions, {} locals",
                    ir.instruction_count(),
                    ir.locals.len()
                );
                if self.options.optimize {
                    opt::optimize(&mut ir);
                }
                log::trace!("function {name}:\n{ir}");
                ir
            }
            // Native functions: only the signature.
            None => FunctionIr {
                params: params
                    .iter()
                    .flat_map(|l| l.params())
                    .filter_map(|p| {
                        Some(Slot::new(p.name()?.text(), p.ty().and_then(TypeRef::from_ast)))
                    })
                    .collect(),
                ..FunctionIr::default()
            },
        };
        let mut fn_flags = FunctionFlags::empty();
        if global {
            fn_flags |= FunctionFlags::GLOBAL;
        }
        if native {
            fn_flags |= FunctionFlags::NATIVE;
        }
        let docstring = self.docstring(node);
        self.lower_function(&ir, ret, &docstring, flags, fn_flags)
    }

    /// The IR as `.pex` code: names to the string table, labels to relative
    /// jump offsets, and the argument count of calls.
    fn lower_function(
        &mut self,
        ir: &FunctionIr,
        ret: Option<TypeRef>,
        docstring: &str,
        user_flags: u32,
        flags: FunctionFlags,
    ) -> (FunctionInfo, Vec<u16>) {
        let params = self.slots(&ir.params);
        let locals = self.slots(&ir.locals);
        let positions = ir.label_positions();
        let count = ir.instruction_count();
        let mut instructions = Vec::with_capacity(count);
        let mut lines = Vec::with_capacity(count);
        for (inst, line) in &ir.code {
            let (opcode, args) = match inst {
                Inst::Label(_) => continue,
                Inst::Op { opcode, args } => (*opcode, self.arguments(*opcode, args)),
                Inst::Jump { opcode, cond, target } => {
                    // Offsets are relative to the jump.
                    let here = instructions.len() as i64;
                    let to = positions[target.0 as usize] as i64;
                    let mut args: Vec<VariableValue> =
                        cond.iter().map(|c| self.strings.value(c)).collect();
                    args.push(VariableValue::Integer((to - here) as i32));
                    (*opcode, args)
                }
            };
            instructions.push(Instruction { opcode, args });
            lines.push(u16::try_from(*line).unwrap_or(u16::MAX));
        }
        let info = FunctionInfo {
            return_type: self.strings.type_name(ret),
            docstring: self.strings.id(docstring),
            user_flags,
            flags,
            params,
            locals,
            instructions,
        };
        (info, lines)
    }

    fn slots(&mut self, slots: &[Slot]) -> Vec<VariableType> {
        slots
            .iter()
            .map(|s| VariableType {
                name: self.strings.name(s.name),
                ty: self.strings.type_name(s.ty),
            })
            .collect()
    }

    /// The arguments of an instruction; calls get the count of their
    /// variadic arguments after the fixed ones.
    fn arguments(&mut self, opcode: OpCode, args: &[Operand]) -> Vec<VariableValue> {
        let fixed = opcode.argument_count().min(args.len());
        let mut out = Vec::with_capacity(args.len() + 1);
        out.extend(args[..fixed].iter().map(|a| self.strings.value(a)));
        if opcode.is_variadic() {
            out.push(VariableValue::Integer((args.len() - fixed) as i32));
        }
        out.extend(args[fixed..].iter().map(|a| self.strings.value(a)));
        out
    }

    /// The `GetState` and `GotoState` functions every script has, unless it
    /// defines them.
    fn state_functions(&mut self) {
        let defined =
            |a: &Self, name: &str| a.states[0].1.iter().any(|f| a.strings_eq(f.name, name));
        if !defined(self, "GotoState") {
            let call = |name: &str| {
                let args =
                    vec![Operand::var(name), Operand::var("self"), Operand::var("::NoneVar")];
                (Inst::Op { opcode: OpCode::CallMethod, args }, 0)
            };
            let assign = Inst::Op {
                opcode: OpCode::Assign,
                args: vec![Operand::var("::State"), Operand::var("newState")],
            };
            let ir = FunctionIr {
                params: vec![Slot::new("newState", Some(TypeRef::STRING))],
                locals: vec![Slot::new("::NoneVar", None)],
                code: vec![call("onEndState"), (assign, 0), call("onBeginState")],
            };
            let (info, _) = self.lower_function(
                &ir,
                None,
                "Function that switches this object to the specified state",
                0,
                FunctionFlags::empty(),
            );
            let name = self.strings.id("GotoState");
            self.states[0].1.push(Function { name, info });
        }
        if !defined(self, "GetState") {
            let ret = Inst::Op { opcode: OpCode::Ret, args: vec![Operand::var("::State")] };
            let ir = FunctionIr { code: vec![(ret, 0)], ..FunctionIr::default() };
            let (info, _) = self.lower_function(
                &ir,
                Some(TypeRef::STRING),
                "Function that returns the current state",
                0,
                FunctionFlags::empty(),
            );
            let name = self.strings.id("GetState");
            self.states[0].1.push(Function { name, info });
        }
    }

    fn strings_eq(&self, id: StringId, name: &str) -> bool {
        self.strings.get(id).is_some_and(|s| s.eq_ignore_ascii_case(name))
    }
}
