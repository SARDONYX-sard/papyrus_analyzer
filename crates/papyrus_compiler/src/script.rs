//! Assembly of a script into a [`PexFile`]: variables, properties, states,
//! the string table and the debug information.

use papyrus_ide::{
    LineIndex,
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
        GameType, Instruction, LE_MAGIC_NUMBER, Object, ObjectFlags, Property, PropertyFlags,
        State, UserFlag, Variable, VariableType, VariableValue, property_auto_var_name,
    },
};

use crate::{
    CompileOptions,
    codegen::{self, ScriptCx, convert_const, type_name},
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
        cx: ScriptCx { sema, lines: LineIndex::new(sema.text), temps: 0 },
        options,
        strings: StringTable::default(),
        debug: Vec::new(),
        variables: Vec::new(),
        properties: Vec::new(),
        states: vec![(String::new(), Vec::new())],
        auto_state: String::new(),
    };
    // Interned first so that `::State` and friends keep their spelling.
    let script_name = sema.index.name_str().to_owned();
    a.strings.id(&script_name);

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
            h.extends().map(|e| e.text().to_owned()).unwrap_or_default(),
            a.docstring(h.syntax()),
            a.user_flags(h.flags(), FlagTargets::SCRIPT),
        ),
        None => (String::new(), String::new(), 0),
    };

    let auto_state = std::mem::take(&mut a.auto_state);
    let states = std::mem::take(&mut a.states)
        .into_iter()
        .map(|(name, functions)| State { name: a.strings.id(&name), functions })
        .collect();
    let object = Object {
        name: a.strings.id(&script_name),
        size: 0,
        parent_class_name: a.strings.id(&parent),
        docstring: a.strings.id(&docstring),
        user_flags: ObjectFlags::from_bits_retain(user_flags),
        auto_state_name: a.strings.id(&auto_state),
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
        .map(|d| UserFlag { name: a.strings.id(d.name.as_str()), flag_index: d.index as u8 })
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

struct Assembler<'o, 'a> {
    cx: ScriptCx<'a>,
    options: &'o CompileOptions,
    strings: StringTable,
    debug: Vec<DebugFunction>,
    variables: Vec<Variable>,
    properties: Vec<Property>,
    /// States in source order, the empty state first.
    states: Vec<(String, Vec<Function>)>,
    auto_state: String,
}

impl Assembler<'_, '_> {
    fn text(&self) -> &str {
        self.cx.sema.text
    }

    /// The `{ ... }` documentation of a declaration.
    fn docstring(&self, node: SyntaxNode<'_>) -> String {
        // Only after the header: a `{ ... }` above belongs to the previous
        // declaration.
        let start = node.text_range().start();
        // The text between the braces is kept as written, indentation
        // included, like the official compiler does.
        doc_ranges(node)
            .into_iter()
            .filter(|r| r.start() >= start)
            .find_map(|r| {
                let raw = &self.text()[usize::from(r.start())..usize::from(r.end())];
                let inner = raw.strip_prefix('{')?;
                Some(
                    inner
                        .strip_suffix('}')
                        .unwrap_or(inner)
                        .replace("\r\n", "\n")
                        .trim()
                        .to_owned(),
                )
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

    fn state_index(&mut self, state: Option<&str>) -> usize {
        let name = state.unwrap_or("");
        match self.states.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None => {
                self.states.push((name.to_owned(), Vec::new()));
                self.states.len() - 1
            }
        }
    }

    fn item(&mut self, item: ast::Item<'_>, state: Option<&str>) {
        match item {
            ast::Item::Header(_) | ast::Item::Import(_) => {}
            ast::Item::VarDecl(var) => {
                let Some(name) = var.name() else { return };
                let ty = var.ty().and_then(TypeRef::from_ast);
                let value = self.initial_value(var.initializer(), ty);
                let user_flags = self.user_flags(var.flags(), FlagTargets::VARIABLE);
                let variable = Variable {
                    name: self.strings.id(name.text()),
                    type_name: self.strings.id(&type_name(ty)),
                    user_flags,
                    value,
                };
                self.variables.push(variable);
            }
            ast::Item::Property(prop) => self.property(prop),
            ast::Item::State(st) => {
                let Some(name) = st.name() else { return };
                let name = name.text().to_owned();
                self.state_index(Some(&name));
                if st.is_auto() && self.auto_state.is_empty() {
                    self.auto_state = name.clone();
                }
                for inner in st.items() {
                    self.item(inner, Some(&name));
                }
            }
            ast::Item::Function(f) => {
                let Some(name) = f.name() else { return };
                let name = name.text().to_owned();
                let info = self.function_info(f.syntax());
                self.push_function(state, &name, info);
            }
            ast::Item::Event(e) => {
                let Some(name) = e.name() else { return };
                let name = name.text().to_owned();
                let info = self.function_info(e.syntax());
                self.push_function(state, &name, info);
            }
        }
    }

    fn push_function(
        &mut self,
        state: Option<&str>,
        name: &str,
        (info, lines): (FunctionInfo, Vec<u16>),
    ) {
        let has_code = !info.is_native();
        let index = self.state_index(state);
        let name_id = self.strings.id(name);
        self.states[index].1.push(Function { name: name_id, info });
        if has_code {
            self.debug_entry(state.unwrap_or(""), name, DebugFunctionType::Method, lines);
        }
    }

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

    fn initial_value(&mut self, init: Option<ast::Expr<'_>>, ty: Option<TypeRef>) -> VariableValue {
        let Some(init) = init else { return VariableValue::Null };
        let range = init.syntax().trimmed_range();
        let text = &self.text()[usize::from(range.start())..usize::from(range.end())];
        let Some(value) = literal::parse(text) else { return VariableValue::Null };
        let value = ty.and_then(|t| convert_const(&value, &t.to_string())).unwrap_or(value);
        self.strings.value(&value)
    }

    fn property(&mut self, prop: ast::Property<'_>) {
        let Some(name) = prop.name() else { return };
        let name = name.text().to_owned();
        let ty = prop.ty().and_then(TypeRef::from_ast);
        let ty_name = type_name(ty);
        let docstring = self.docstring(prop.syntax());
        let user_flags = self.user_flags(prop.flags(), FlagTargets::PROPERTY);
        let mut property = Property {
            name: self.strings.id(&name),
            ty: self.strings.id(&ty_name),
            docstring: self.strings.id(&docstring),
            user_flags,
            flags: PropertyFlags::empty(),
            auto_var_name: None,
            read_handler: None,
            write_handler: None,
        };
        if prop.is_read_only() {
            // A getter returning the value.
            let line = self.cx.line(prop.syntax().trimmed_range());
            let value = self.initial_value(prop.initializer(), ty);
            let info = FunctionInfo {
                return_type: self.strings.id(&ty_name),
                docstring: self.strings.id(""),
                user_flags: 0,
                flags: FunctionFlags::empty(),
                params: Vec::new(),
                locals: Vec::new(),
                instructions: vec![Instruction {
                    opcode: papyrus_pex::pex::OpCode::Ret,
                    args: vec![value],
                }],
            };
            property.flags = PropertyFlags::READ;
            property.read_handler = Some(info);
            self.debug_entry("", &name, DebugFunctionType::Getter, vec![line as u16]);
        } else if prop.is_auto() {
            let var = property_auto_var_name(&name);
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
                type_name: self.strings.id(&ty_name),
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
                    self.debug_entry("", &name, DebugFunctionType::Getter, lines);
                } else if fname.text().eq_ignore_ascii_case("Set") {
                    property.flags |= PropertyFlags::WRITE;
                    property.write_handler = Some(info);
                    self.debug_entry("", &name, DebugFunctionType::Setter, lines);
                }
            }
        }
        self.properties.push(property);
    }

    /// Compiles a `FUNCTION` or `EVENT` node.
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
            None => FunctionIr {
                params: params
                    .iter()
                    .flat_map(|l| l.params())
                    .filter_map(|p| {
                        Some(Slot {
                            name: p.name()?.text().to_owned(),
                            ty: type_name(p.ty().and_then(TypeRef::from_ast)),
                        })
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
        self.lower_function(&ir, &type_name(ret), &docstring, flags, fn_flags)
    }

    fn lower_function(
        &mut self,
        ir: &FunctionIr,
        ret: &str,
        docstring: &str,
        user_flags: u32,
        flags: FunctionFlags,
    ) -> (FunctionInfo, Vec<u16>) {
        let slots = |s: &[Slot], strings: &mut StringTable| -> Vec<VariableType> {
            s.iter()
                .map(|s| VariableType { name: strings.id(&s.name), ty: strings.id(&s.ty) })
                .collect()
        };
        let params = slots(&ir.params, &mut self.strings);
        let locals = slots(&ir.locals, &mut self.strings);
        let mut instructions = Vec::new();
        let mut lines = Vec::new();
        for inst in ir.lower() {
            let fixed = inst.opcode.argument_count();
            let mut args: Vec<VariableValue> = Vec::with_capacity(inst.args.len() + 1);
            for (i, arg) in inst.args.iter().enumerate() {
                if inst.opcode.is_variadic() && i == fixed {
                    args.push(VariableValue::Integer((inst.args.len() - fixed) as i32));
                }
                args.push(self.strings.value(arg));
            }
            if inst.opcode.is_variadic() && inst.args.len() == fixed {
                args.push(VariableValue::Integer(0));
            }
            instructions.push(Instruction { opcode: inst.opcode, args });
            lines.push(inst.line.min(u16::MAX as u32) as u16);
        }
        let info = FunctionInfo {
            return_type: self.strings.id(ret),
            docstring: self.strings.id(docstring),
            user_flags,
            flags,
            params,
            locals,
            instructions,
        };
        (info, lines)
    }

    /// The `GetState` and `GotoState` functions every script has.
    fn state_functions(&mut self) {
        use papyrus_pex::pex::OpCode;
        let defined =
            |a: &Self, name: &str| a.states[0].1.iter().any(|f| a.strings_eq(f.name, name));
        if !defined(self, "GotoState") {
            let ir = FunctionIr {
                params: vec![Slot { name: "newState".into(), ty: "String".into() }],
                locals: vec![Slot { name: "::NoneVar".into(), ty: "None".into() }],
                code: vec![
                    (
                        Inst::Op {
                            opcode: OpCode::CallMethod,
                            args: vec![
                                Operand::var("onEndState"),
                                Operand::var("self"),
                                Operand::var("::NoneVar"),
                            ],
                        },
                        0,
                    ),
                    (
                        Inst::Op {
                            opcode: OpCode::Assign,
                            args: vec![Operand::var("::State"), Operand::var("newState")],
                        },
                        0,
                    ),
                    (
                        Inst::Op {
                            opcode: OpCode::CallMethod,
                            args: vec![
                                Operand::var("onBeginState"),
                                Operand::var("self"),
                                Operand::var("::NoneVar"),
                            ],
                        },
                        0,
                    ),
                ],
            };
            let (info, _) = self.lower_function(
                &ir,
                "None",
                "Function that switches this object to the specified state",
                0,
                FunctionFlags::empty(),
            );
            let name = self.strings.id("GotoState");
            self.states[0].1.push(Function { name, info });
        }
        if !defined(self, "GetState") {
            let ir = FunctionIr {
                code: vec![(
                    Inst::Op { opcode: OpCode::Ret, args: vec![Operand::var("::State")] },
                    0,
                )],
                ..FunctionIr::default()
            };
            let (info, _) = self.lower_function(
                &ir,
                "String",
                "Function that returns the current state",
                0,
                FunctionFlags::empty(),
            );
            let name = self.strings.id("GetState");
            self.states[0].1.push(Function { name, info });
        }
    }

    fn strings_eq(&self, id: papyrus_pex::pex::StringId, name: &str) -> bool {
        self.strings.get(id).is_some_and(|s| s.eq_ignore_ascii_case(name))
    }
}
