// SPDX-FileCopyrightText: (C) 2025 russo-2025
// SPDX-License-Identifier: MIT
//! Reading `.pex` files.
//!
//! The file is a tree of records, big-endian, each written field after field
//! (see [`crate::pex`] for what each field means):
//!
//! ```text
//! file      = header, string table, debug info?, user flags, objects
//! object    = name, size, parent, doc, flags, auto state, variables, properties, states
//! property  = name, type, doc, flags, (auto variable | getter? setter?)
//! state     = name, functions
//! function  = name, function info
//! info      = return type, doc, flags, params, locals, instructions
//! list of T = u16 count, then count times T
//! ```
//!
//! [`Reader`] has one method per record, in this order; `write.rs` has the
//! same methods in the same order, so the two can be read side by side.

use std::borrow::Cow;

use crate::{Error, pex::*};

/// A cursor over the bytes of a file, big-endian.
struct Reader<'a> {
    original: &'a [u8],
    remaining: &'a [u8],
}

/// Primitive values.
impl<'a> Reader<'a> {
    fn offset(&self) -> usize {
        self.original.len() - self.remaining.len()
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let Some((head, tail)) = self.remaining.split_at_checked(len) else {
            return Err(Error::UnexpectedEof {
                offset: self.offset(),
                expected: len,
                remaining: self.remaining.len(),
            });
        };
        self.remaining = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_bits(self.u32()?))
    }

    /// A string of the header or of the string table: u16 length, then the
    /// bytes, borrowed from the input.
    fn string(&mut self) -> Result<PexStr<'a>, Error> {
        let len = self.u16()? as usize;
        Ok(PexStr(Cow::Borrowed(self.take(len)?)))
    }

    /// An index into the string table.
    fn string_id(&mut self) -> Result<StringId, Error> {
        Ok(StringId(self.u16()?))
    }

    /// A u16 count, then that many `T`.
    fn list<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let len = self.u16()? as usize;
        (0..len).map(|_| read(self)).collect()
    }

    /// `T` when `present`, which a flag read before tells.
    fn optional<T>(
        &mut self,
        present: bool,
        read: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        if present { read(self).map(Some) } else { Ok(None) }
    }

    /// A type tag (u8), then the value.
    fn value(&mut self) -> Result<VariableValue, Error> {
        let offset = self.offset();
        Ok(match self.u8()? {
            0 => VariableValue::Null,
            1 => VariableValue::Identifier(self.string_id()?),
            2 => VariableValue::String(self.string_id()?),
            3 => VariableValue::Integer(self.i32()?),
            4 => VariableValue::Float(self.f32()?),
            5 => VariableValue::Boolean(self.u8()? != 0),
            value => return Err(Error::InvalidVariableValue { offset, value }),
        })
    }
}

/// Records, from the leaves to the root.
impl Reader<'_> {
    fn variable_type(&mut self) -> Result<VariableType, Error> {
        Ok(VariableType { name: self.string_id()?, ty: self.string_id()? })
    }

    fn instruction(&mut self) -> Result<Instruction, Error> {
        let offset = self.offset();
        let opcode = OpCode::try_from(self.u8()?)
            .map_err(|source| Error::InvalidOpCode { offset, source })?;
        let mut args = Vec::with_capacity(opcode.argument_count() + 1);
        for _ in 0..opcode.argument_count() {
            args.push(self.value()?);
        }
        // Calls: an integer count, then that many arguments.
        if opcode.is_variadic() {
            let count = self.value()?;
            args.push(count);
            if let VariableValue::Integer(n) = count {
                for _ in 0..n {
                    args.push(self.value()?);
                }
            }
        }
        Ok(Instruction { opcode, args })
    }

    fn function_info(&mut self) -> Result<FunctionInfo, Error> {
        Ok(FunctionInfo {
            return_type: self.string_id()?,
            docstring: self.string_id()?,
            user_flags: self.u32()?,
            flags: FunctionFlags::from_bits_retain(self.u8()?),
            params: self.list(Self::variable_type)?,
            locals: self.list(Self::variable_type)?,
            instructions: self.list(Self::instruction)?,
        })
    }

    fn function(&mut self) -> Result<Function, Error> {
        Ok(Function { name: self.string_id()?, info: self.function_info()? })
    }

    fn state(&mut self) -> Result<State, Error> {
        Ok(State { name: self.string_id()?, functions: self.list(Self::function)? })
    }

    fn property(&mut self) -> Result<Property, Error> {
        let name = self.string_id()?;
        let ty = self.string_id()?;
        let docstring = self.string_id()?;
        let user_flags = self.u32()?;
        let flags = PropertyFlags::from_bits_retain(self.u8()?);
        // Auto properties name their variable; the others have accessors.
        let auto = flags.contains(PropertyFlags::AUTO_VAR);
        let read = !auto && flags.contains(PropertyFlags::READ);
        let write = !auto && flags.contains(PropertyFlags::WRITE);
        Ok(Property {
            name,
            ty,
            docstring,
            user_flags,
            flags,
            auto_var_name: self.optional(auto, Self::string_id)?,
            read_handler: self.optional(read, Self::function_info)?,
            write_handler: self.optional(write, Self::function_info)?,
        })
    }

    fn variable(&mut self) -> Result<Variable, Error> {
        Ok(Variable {
            name: self.string_id()?,
            type_name: self.string_id()?,
            user_flags: self.u32()?,
            value: self.value()?,
        })
    }

    fn object(&mut self) -> Result<Object, Error> {
        Ok(Object {
            name: self.string_id()?,
            size: self.u32()?,
            parent_class_name: self.string_id()?,
            docstring: self.string_id()?,
            user_flags: ObjectFlags::from_bits_retain(self.u32()?),
            auto_state_name: self.string_id()?,
            variables: self.list(Self::variable)?,
            properties: self.list(Self::property)?,
            states: self.list(Self::state)?,
        })
    }

    fn user_flag(&mut self) -> Result<UserFlag, Error> {
        Ok(UserFlag { name: self.string_id()?, flag_index: self.u8()? })
    }

    fn debug_function(&mut self) -> Result<DebugFunction, Error> {
        let object_name = self.string_id()?;
        let state_name = self.string_id()?;
        let function_name = self.string_id()?;
        let offset = self.offset();
        let function_type = match self.u8()? {
            0 => DebugFunctionType::Method,
            1 => DebugFunctionType::Getter,
            2 => DebugFunctionType::Setter,
            value => return Err(Error::InvalidDebugFunctionType { offset, value }),
        };
        Ok(DebugFunction {
            object_name,
            state_name,
            function_name,
            function_type,
            instruction_line_numbers: self.list(Self::u16)?,
        })
    }

    fn debug_info(&mut self) -> Result<DebugInfo, Error> {
        Ok(DebugInfo {
            modification_time: self.i64()?,
            functions: self.list(Self::debug_function)?,
        })
    }
}

impl<'a> PexFile<'a> {
    /// Reads a file. Strings borrow `bytes`; see [`PexFile::into_owned`].
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut r = Reader { original: bytes, remaining: bytes };
        let magic_number = r.u32()?;
        if magic_number != LE_MAGIC_NUMBER {
            return Err(Error::InvalidMagicNumber { offset: 0, value: magic_number });
        }
        let major_version = r.u8()?;
        let minor_version = r.u8()?;
        let offset = r.offset();
        let game_id = match r.u16()? {
            1 => GameType::Skyrim,
            value => return Err(Error::InvalidGameType { offset, value }),
        };
        let compilation_time = r.i64()?;
        let src_file_name = r.string()?;
        let user_name = r.string()?;
        let machine_name = r.string()?;
        let string_table = r.list(Reader::string)?;
        let has_debug_info = r.u8()? != 0;
        let debug_info = r.optional(has_debug_info, Reader::debug_info)?;
        let user_flags = r.list(Reader::user_flag)?;
        let objects = r.list(Reader::object)?;
        if !r.remaining.is_empty() {
            return Err(Error::TrailingBytes { offset: r.offset(), len: r.remaining.len() });
        }
        Ok(PexFile {
            magic_number,
            major_version,
            minor_version,
            game_id,
            compilation_time,
            src_file_name,
            user_name,
            machine_name,
            string_table,
            debug_info,
            user_flags,
            objects,
        })
    }
}
