// SPDX-FileCopyrightText: (C) 2025 russo-2025
// SPDX-License-Identifier: MIT
//! The data model of a compiled Papyrus script (`.pex`, Skyrim: version 3.2,
//! big-endian).
//!
//! Strings are kept as the raw bytes of the file ([`PexStr`]), so that reading
//! and writing a file gives back the same bytes, whatever its encoding (the
//! official compiler writes Windows-1252).

use std::borrow::Cow;

/// The name of the empty state, the one functions outside of any state live in.
pub const EMPTY_STATE_NAME: &str = "";

/// The hidden variable backing the auto property `name`.
#[inline]
pub fn property_auto_var_name(name: &str) -> String {
    format!("::{name}_var")
}

/// The magic number of Skyrim `.pex` files.
pub const LE_MAGIC_NUMBER: u32 = 0xFA57C0DE;

/// A string of a `.pex` file: raw bytes, usually Windows-1252.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct PexStr<'a>(pub Cow<'a, [u8]>);

impl<'a> PexStr<'a> {
    /// Encodes `text` as Windows-1252, keeping UTF-8 for characters that have
    /// no Windows-1252 byte.
    pub fn encode(text: &str) -> PexStr<'static> {
        if text.is_ascii() {
            return PexStr(Cow::Owned(text.as_bytes().to_vec()));
        }
        let mut out = Vec::with_capacity(text.len());
        for c in text.chars() {
            match encode_1252(c) {
                Some(b) => out.push(b),
                None => {
                    let mut buf = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        PexStr(Cow::Owned(out))
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Decodes the string: UTF-8 when valid, else Windows-1252.
    pub fn decode(&self) -> Cow<'_, str> {
        match std::str::from_utf8(&self.0) {
            Ok(s) => Cow::Borrowed(s),
            Err(_) => Cow::Owned(self.0.iter().map(|&b| decode_1252(b)).collect()),
        }
    }

    /// Whether the string is `text`, ignoring ASCII case like the game does.
    /// Compares the raw bytes: nothing is decoded.
    pub fn eq_ignore_ascii_case(&self, text: &str) -> bool {
        self.0.eq_ignore_ascii_case(text.as_bytes())
    }

    /// An owned copy, independent of the input bytes.
    pub fn into_owned(self) -> PexStr<'static> {
        PexStr(Cow::Owned(self.0.into_owned()))
    }
}

impl std::fmt::Debug for PexStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.decode(), f)
    }
}

impl std::fmt::Display for PexStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.decode())
    }
}

const HIGH_1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
];

fn decode_1252(b: u8) -> char {
    match b {
        0x80..=0x9F => HIGH_1252[(b - 0x80) as usize],
        _ => b as char,
    }
}

fn encode_1252(c: char) -> Option<u8> {
    match c as u32 {
        0..=0x7F | 0xA0..=0xFF => Some(c as u8),
        _ => HIGH_1252.iter().position(|&h| h == c).map(|i| 0x80 + i as u8),
    }
}

/// An index into the string table.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StringId(pub u16);

impl StringId {
    /// The index as `usize`.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Display for StringId {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The game a file is compiled for.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameType {
    /// Not set.
    Unknown = 0,
    /// Skyrim (Legendary and Special Edition).
    Skyrim = 1,
}

impl GameType {
    /// The value written in the header.
    #[inline]
    pub const fn to_u16(self) -> u16 {
        self as u16
    }
}

/// The instructions of the Papyrus virtual machine (Skyrim).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpCode {
    /// `nop`
    Nop = 0,
    /// `iadd dest, a, b`
    IAdd,
    /// `fadd dest, a, b`
    FAdd,
    /// `isub dest, a, b`
    ISub,
    /// `fsub dest, a, b`
    FSub,
    /// `imul dest, a, b`
    IMul,
    /// `fmul dest, a, b`
    FMul,
    /// `idiv dest, a, b`
    IDiv,
    /// `fdiv dest, a, b`
    FDiv,
    /// `imod dest, a, b`
    IMod,
    /// `not dest, a`
    Not,
    /// `ineg dest, a`
    INeg,
    /// `fneg dest, a`
    FNeg,
    /// `assign dest, a`
    Assign,
    /// `cast dest, a`
    Cast,
    /// `cmp_eq dest, a, b`
    CmpEq,
    /// `cmp_lt dest, a, b`
    CmpLt,
    /// `cmp_le dest, a, b`
    CmpLe,
    /// `cmp_gt dest, a, b`
    CmpGt,
    /// `cmp_ge dest, a, b`
    CmpGe,
    /// `jmp offset`
    Jmp,
    /// `jmpt cond, offset`
    JmpT,
    /// `jmpf cond, offset`
    JmpF,
    /// `callmethod name, object, dest, count, args...`
    CallMethod,
    /// `callparent name, dest, count, args...`
    CallParent,
    /// `callstatic object, name, dest, count, args...`
    CallStatic,
    /// `return value`
    Ret,
    /// `strcat dest, a, b`
    StrCat,
    /// `propget name, object, dest`
    PropGet,
    /// `propset name, object, value`
    PropSet,
    /// `array_create dest, size`
    ArrayCreate,
    /// `array_length dest, array`
    ArrayLength,
    /// `array_getelement dest, array, index`
    ArrayGetElement,
    /// `array_setelement array, index, value`
    ArraySetElement,
    /// `array_findelement array, dest, value, start`
    ArrayFindElement,
    /// `array_rfindelement array, dest, value, start`
    ArrayRFindElement,
}

impl OpCode {
    /// All opcodes, by value.
    pub const ALL: [OpCode; 36] = [
        Self::Nop,
        Self::IAdd,
        Self::FAdd,
        Self::ISub,
        Self::FSub,
        Self::IMul,
        Self::FMul,
        Self::IDiv,
        Self::FDiv,
        Self::IMod,
        Self::Not,
        Self::INeg,
        Self::FNeg,
        Self::Assign,
        Self::Cast,
        Self::CmpEq,
        Self::CmpLt,
        Self::CmpLe,
        Self::CmpGt,
        Self::CmpGe,
        Self::Jmp,
        Self::JmpT,
        Self::JmpF,
        Self::CallMethod,
        Self::CallParent,
        Self::CallStatic,
        Self::Ret,
        Self::StrCat,
        Self::PropGet,
        Self::PropSet,
        Self::ArrayCreate,
        Self::ArrayLength,
        Self::ArrayGetElement,
        Self::ArraySetElement,
        Self::ArrayFindElement,
        Self::ArrayRFindElement,
    ];

    /// The number of fixed arguments. Calls are followed by an integer count
    /// and that many variadic arguments.
    #[inline]
    pub const fn argument_count(self) -> usize {
        match self {
            Self::Nop => 0,
            Self::Jmp | Self::Ret => 1,
            Self::Not
            | Self::INeg
            | Self::FNeg
            | Self::Assign
            | Self::Cast
            | Self::JmpT
            | Self::JmpF
            | Self::ArrayCreate
            | Self::ArrayLength
            | Self::CallParent => 2,
            Self::IAdd
            | Self::FAdd
            | Self::ISub
            | Self::FSub
            | Self::IMul
            | Self::FMul
            | Self::IDiv
            | Self::FDiv
            | Self::IMod
            | Self::CmpEq
            | Self::CmpLt
            | Self::CmpLe
            | Self::CmpGt
            | Self::CmpGe
            | Self::StrCat
            | Self::PropGet
            | Self::PropSet
            | Self::ArrayGetElement
            | Self::ArraySetElement
            | Self::CallStatic
            | Self::CallMethod => 3,
            Self::ArrayFindElement | Self::ArrayRFindElement => 4,
        }
    }

    /// Whether variadic arguments follow the fixed ones.
    #[inline]
    pub const fn is_variadic(self) -> bool {
        matches!(self, Self::CallMethod | Self::CallParent | Self::CallStatic)
    }

    /// The mnemonic, as in the disassembly of the official tools.
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Nop => "NOP",
            Self::IAdd => "IADD",
            Self::FAdd => "FADD",
            Self::ISub => "ISUB",
            Self::FSub => "FSUB",
            Self::IMul => "IMUL",
            Self::FMul => "FMUL",
            Self::IDiv => "IDIV",
            Self::FDiv => "FDIV",
            Self::IMod => "IMOD",
            Self::Not => "NOT",
            Self::INeg => "INEG",
            Self::FNeg => "FNEG",
            Self::Assign => "ASSIGN",
            Self::Cast => "CAST",
            Self::CmpEq => "COMPAREEQ",
            Self::CmpLt => "COMPARELT",
            Self::CmpLe => "COMPARELTE",
            Self::CmpGt => "COMPAREGT",
            Self::CmpGe => "COMPAREGTE",
            Self::Jmp => "JUMP",
            Self::JmpT => "JUMPT",
            Self::JmpF => "JUMPF",
            Self::CallMethod => "CALLMETHOD",
            Self::CallParent => "CALLPARENT",
            Self::CallStatic => "CALLSTATIC",
            Self::Ret => "RETURN",
            Self::StrCat => "STRCAT",
            Self::PropGet => "PROPGET",
            Self::PropSet => "PROPSET",
            Self::ArrayCreate => "ARRAYCREATE",
            Self::ArrayLength => "ARRAYLENGTH",
            Self::ArrayGetElement => "ARRAYGETELEMENT",
            Self::ArraySetElement => "ARRAYSETELEMENT",
            Self::ArrayFindElement => "ARRAYFINDELEMENT",
            Self::ArrayRFindElement => "ARRAYRFINDELEMENT",
        }
    }
}

impl TryFrom<u8> for OpCode {
    type Error = OpcodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::ALL.get(value as usize).copied().ok_or(OpcodeError { value })
    }
}

/// An unknown opcode byte.
#[derive(Debug)]
pub struct OpcodeError {
    value: u8,
}

impl core::fmt::Display for OpcodeError {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "OpCodes range from 0 to 35, but got {}.", self.value)
    }
}

impl core::error::Error for OpcodeError {}

/// What a debug function entry describes.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugFunctionType {
    /// A function or event.
    Method = 0,
    /// A property getter.
    Getter = 1,
    /// A property setter.
    Setter = 2,
}

bitflags::bitflags! {
    /// User flags of an object (indices of the flags file).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ObjectFlags: u32 {
        /// `Hidden`
        const HIDDEN      = 0b0001;
        /// `Conditional`
        const CONDITIONAL = 0b0010;
    }
}

bitflags::bitflags! {
    /// Flags of a property.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PropertyFlags: u8 {
        /// Has a getter.
        const READ    = 0b0001;
        /// Has a setter.
        const WRITE   = 0b0010;
        /// Backed by an auto variable.
        const AUTO_VAR = 0b0100;
    }
}

bitflags::bitflags! {
    /// Flags of a function.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FunctionFlags: u8 {
        /// `Global`
        const GLOBAL = 0b0001;
        /// `Native`
        const NATIVE = 0b0010;
    }
}

/// An operand or a constant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VariableValue {
    /// `None`, or no value.
    Null,
    /// A variable, parameter, property or script name.
    Identifier(StringId),
    /// A string constant.
    String(StringId),
    /// An integer constant.
    Integer(i32),
    /// A float constant.
    Float(f32),
    /// A boolean constant.
    Boolean(bool),
}

/// A flag of the flags file.
#[derive(Debug, Clone, PartialEq)]
pub struct UserFlag {
    /// The flag name.
    pub name: StringId,
    /// The bit index.
    pub flag_index: u8,
}

/// A named and typed slot: a parameter or a local.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableType {
    /// The name.
    pub name: StringId,
    /// The type name.
    pub ty: StringId,
}

/// One instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    /// The opcode.
    pub opcode: OpCode,
    /// The fixed arguments, then (for calls) the count and the variadic ones.
    pub args: Vec<VariableValue>,
}

/// A script variable.
#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    /// The name.
    pub name: StringId,
    /// The type name.
    pub type_name: StringId,
    /// User flags.
    pub user_flags: u32,
    /// The initial value.
    pub value: VariableValue,
}

/// Line numbers of the instructions of one function.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugFunction {
    /// The object.
    pub object_name: StringId,
    /// The state.
    pub state_name: StringId,
    /// The function, or the property for accessors.
    pub function_name: StringId,
    /// What the entry describes.
    pub function_type: DebugFunctionType,
    /// One source line per instruction.
    pub instruction_line_numbers: Vec<u16>,
}

/// The body and signature of a function.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionInfo {
    /// The return type name (`None` for no value).
    pub return_type: StringId,
    /// The documentation.
    pub docstring: StringId,
    /// User flags.
    pub user_flags: u32,
    /// Flags.
    pub flags: FunctionFlags,
    /// Parameters.
    pub params: Vec<VariableType>,
    /// Locals, temporaries included.
    pub locals: Vec<VariableType>,
    /// The code.
    pub instructions: Vec<Instruction>,
}

impl FunctionInfo {
    /// `Global` function.
    #[inline]
    pub fn is_global(&self) -> bool {
        self.flags.contains(FunctionFlags::GLOBAL)
    }

    /// `Native` function.
    #[inline]
    pub fn is_native(&self) -> bool {
        self.flags.contains(FunctionFlags::NATIVE)
    }
}

/// A named function of a state.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The name.
    pub name: StringId,
    /// The body.
    pub info: FunctionInfo,
}

/// A state and its functions.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// The name; empty for the empty state.
    pub name: StringId,
    /// The functions and events.
    pub functions: Vec<Function>,
}

/// A property.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The name.
    pub name: StringId,
    /// The type name.
    pub ty: StringId,
    /// The documentation.
    pub docstring: StringId,
    /// User flags.
    pub user_flags: u32,
    /// Flags.
    pub flags: PropertyFlags,
    /// The backing variable of auto properties.
    pub auto_var_name: Option<StringId>,
    /// The getter of full properties.
    pub read_handler: Option<FunctionInfo>,
    /// The setter of full properties.
    pub write_handler: Option<FunctionInfo>,
}

impl Property {
    /// Has a getter.
    #[inline]
    pub fn is_read(&self) -> bool {
        self.flags.contains(PropertyFlags::READ)
    }

    /// Has a setter.
    #[inline]
    pub fn is_write(&self) -> bool {
        self.flags.contains(PropertyFlags::WRITE)
    }

    /// Auto property.
    #[inline]
    pub fn is_auto_var(&self) -> bool {
        self.flags.contains(PropertyFlags::AUTO_VAR)
    }
}

/// A script.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    /// The script name.
    pub name: StringId,
    /// The size written in the file, recomputed when writing.
    pub size: u32,
    /// The parent script, empty for none.
    pub parent_class_name: StringId,
    /// The documentation.
    pub docstring: StringId,
    /// User flags.
    pub user_flags: ObjectFlags,
    /// The `Auto` state, empty for none.
    pub auto_state_name: StringId,
    /// Variables, auto property variables included.
    pub variables: Vec<Variable>,
    /// Properties.
    pub properties: Vec<Property>,
    /// States, the empty state first.
    pub states: Vec<State>,
}

/// Debugging information.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugInfo {
    /// When the source was modified, in seconds since 1970.
    pub modification_time: i64,
    /// Line numbers of the functions.
    pub functions: Vec<DebugFunction>,
}

/// A `.pex` file.
#[derive(Debug, Clone, PartialEq)]
pub struct PexFile<'a> {
    /// [`LE_MAGIC_NUMBER`].
    pub magic_number: u32,
    /// 3 for Skyrim.
    pub major_version: u8,
    /// 2 for Skyrim.
    pub minor_version: u8,
    /// The game.
    pub game_id: GameType,
    /// When the file was compiled, in seconds since 1970.
    pub compilation_time: i64,
    /// The source file, as the compiler saw it.
    pub src_file_name: PexStr<'a>,
    /// Who compiled the file.
    pub user_name: PexStr<'a>,
    /// On which machine.
    pub machine_name: PexStr<'a>,
    /// All strings referenced by [`StringId`]s.
    pub string_table: Vec<PexStr<'a>>,
    /// Line numbers, when compiled with them.
    pub debug_info: Option<DebugInfo>,
    /// The flags of the flags file.
    pub user_flags: Vec<UserFlag>,
    /// The scripts; one per file.
    pub objects: Vec<Object>,
}

impl<'a> PexFile<'a> {
    /// The string of a string table entry.
    #[inline]
    pub fn string(&self, id: StringId) -> Option<Cow<'_, str>> {
        self.string_table.get(id.index()).map(|s| s.decode())
    }

    /// The string of an entry, or `?` when the index is out of range.
    pub fn str(&self, id: StringId) -> Cow<'_, str> {
        self.string(id).unwrap_or(Cow::Borrowed("?"))
    }

    /// Finds a string table entry, ignoring ASCII case like the game does.
    pub fn find_string(&self, text: &str) -> Option<StringId> {
        self.string_table
            .iter()
            .position(|s| s.eq_ignore_ascii_case(text))
            .map(|i| StringId(i as u16))
    }

    /// An object by name, ignoring ASCII case.
    pub fn object(&self, name: &str) -> Option<&Object> {
        self.objects.iter().find(|obj| {
            self.string_table.get(obj.name.index()).is_some_and(|s| s.eq_ignore_ascii_case(name))
        })
    }

    /// An independent copy that does not borrow the input bytes.
    pub fn into_owned(self) -> PexFile<'static> {
        PexFile {
            magic_number: self.magic_number,
            major_version: self.major_version,
            minor_version: self.minor_version,
            game_id: self.game_id,
            compilation_time: self.compilation_time,
            src_file_name: self.src_file_name.into_owned(),
            user_name: self.user_name.into_owned(),
            machine_name: self.machine_name.into_owned(),
            string_table: self.string_table.into_iter().map(PexStr::into_owned).collect(),
            debug_info: self.debug_info,
            user_flags: self.user_flags,
            objects: self.objects,
        }
    }
}
