// SPDX-FileCopyrightText: (C) 2025 russo-2025
// SPDX-License-Identifier: MIT
//! Writing `.pex` files.
//!
//! The mirror of `read.rs`: the same methods in the same order, each writing
//! the fields its reader reads (the layout is described there).

use crate::{Error, pex::*};

/// Appends big-endian values to a buffer.
#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}

/// Primitive values.
impl Writer {
    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    /// A count or a length, which the format stores as u16.
    fn len(&mut self, len: usize) -> Result<(), Error> {
        let len = u16::try_from(len).map_err(|_| Error::OverFlowVecLen { len })?;
        self.u16(len);
        Ok(())
    }

    /// A string of the header or of the string table: u16 length, then the
    /// bytes.
    fn string(&mut self, value: &PexStr<'_>) -> Result<(), Error> {
        self.len(value.as_bytes().len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    /// An index into the string table.
    fn string_id(&mut self, value: StringId) {
        self.u16(value.0);
    }

    /// A u16 count, then every item.
    fn list<T>(
        &mut self,
        items: &[T],
        mut write: impl FnMut(&mut Self, &T) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.len(items.len())?;
        items.iter().try_for_each(|item| write(self, item))
    }

    /// A type tag (u8), then the value.
    fn value(&mut self, value: &VariableValue) -> Result<(), Error> {
        match *value {
            VariableValue::Null => self.u8(0),
            VariableValue::Identifier(id) => {
                self.u8(1);
                self.string_id(id);
            }
            VariableValue::String(id) => {
                self.u8(2);
                self.string_id(id);
            }
            VariableValue::Integer(v) => {
                self.u8(3);
                self.i32(v);
            }
            VariableValue::Float(v) => {
                self.u8(4);
                self.f32(v);
            }
            VariableValue::Boolean(v) => {
                self.u8(5);
                self.u8(u8::from(v));
            }
        }
        Ok(())
    }
}

/// Records, from the leaves to the root.
impl Writer {
    fn variable_type(&mut self, v: &VariableType) -> Result<(), Error> {
        self.string_id(v.name);
        self.string_id(v.ty);
        Ok(())
    }

    fn instruction(&mut self, instruction: &Instruction) -> Result<(), Error> {
        self.u8(instruction.opcode as u8);
        // The count of the arguments of calls is one of the `args`.
        instruction.args.iter().try_for_each(|arg| self.value(arg))
    }

    fn function_info(&mut self, info: &FunctionInfo) -> Result<(), Error> {
        self.string_id(info.return_type);
        self.string_id(info.docstring);
        self.u32(info.user_flags);
        self.u8(info.flags.bits());
        self.list(&info.params, Self::variable_type)?;
        self.list(&info.locals, Self::variable_type)?;
        self.list(&info.instructions, Self::instruction)
    }

    fn function(&mut self, function: &Function) -> Result<(), Error> {
        self.string_id(function.name);
        self.function_info(&function.info)
    }

    fn state(&mut self, state: &State) -> Result<(), Error> {
        self.string_id(state.name);
        self.list(&state.functions, Self::function)
    }

    fn property(&mut self, property: &Property) -> Result<(), Error> {
        self.string_id(property.name);
        self.string_id(property.ty);
        self.string_id(property.docstring);
        self.u32(property.user_flags);
        self.u8(property.flags.bits());
        // Auto properties name their variable; the others have accessors.
        if property.is_auto_var() {
            let name = property.auto_var_name.ok_or_else(|| Error::MissingAutoVerInProperty {
                name: property.name.to_string(),
            })?;
            self.string_id(name);
            return Ok(());
        }
        if let Some(handler) = &property.read_handler {
            self.function_info(handler)?;
        }
        if let Some(handler) = &property.write_handler {
            self.function_info(handler)?;
        }
        Ok(())
    }

    fn variable(&mut self, variable: &Variable) -> Result<(), Error> {
        self.string_id(variable.name);
        self.string_id(variable.type_name);
        self.u32(variable.user_flags);
        self.value(&variable.value)
    }

    fn object(&mut self, object: &Object) -> Result<(), Error> {
        self.string_id(object.name);
        // The size counts itself and everything after it in the object:
        // written once the rest is.
        let size_offset = self.bytes.len();
        self.u32(0);
        self.string_id(object.parent_class_name);
        self.string_id(object.docstring);
        self.u32(object.user_flags.bits());
        self.string_id(object.auto_state_name);
        self.list(&object.variables, Self::variable)?;
        self.list(&object.properties, Self::property)?;
        self.list(&object.states, Self::state)?;
        let size = (self.bytes.len() - size_offset) as u32;
        self.bytes[size_offset..size_offset + 4].copy_from_slice(&size.to_be_bytes());
        Ok(())
    }

    fn user_flag(&mut self, flag: &UserFlag) -> Result<(), Error> {
        self.string_id(flag.name);
        self.u8(flag.flag_index);
        Ok(())
    }

    fn debug_function(&mut self, f: &DebugFunction) -> Result<(), Error> {
        self.string_id(f.object_name);
        self.string_id(f.state_name);
        self.string_id(f.function_name);
        self.u8(f.function_type as u8);
        self.list(&f.instruction_line_numbers, |w, &line| {
            w.u16(line);
            Ok(())
        })
    }

    fn debug_info(&mut self, debug: &DebugInfo) -> Result<(), Error> {
        self.i64(debug.modification_time);
        self.list(&debug.functions, Self::debug_function)
    }
}

impl PexFile<'_> {
    /// Writes the file.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::default();
        w.u32(self.magic_number);
        w.u8(self.major_version);
        w.u8(self.minor_version);
        w.u16(self.game_id.to_u16());
        w.i64(self.compilation_time);
        w.string(&self.src_file_name)?;
        w.string(&self.user_name)?;
        w.string(&self.machine_name)?;
        w.list(&self.string_table, Writer::string)?;
        w.u8(u8::from(self.debug_info.is_some()));
        if let Some(debug) = &self.debug_info {
            w.debug_info(debug)?;
        }
        w.list(&self.user_flags, Writer::user_flag)?;
        w.list(&self.objects, Writer::object)?;
        Ok(w.bytes)
    }
}
