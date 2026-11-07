//! The custom events of SKSE, named by strings:
//!
//! ```papyrus
//! RegisterForModEvent("MyEvent", "OnMyEvent")   ; the name, and what to call
//! SendModEvent("MyEvent")                       ; or ModEvent.Create("MyEvent")
//!
//! Event OnMyEvent(String asName, String asArg, Float afArg, Form akSender)
//! EndEvent
//! ```
//!
//! The callback is a reference like an identifier is: hover, go to definition,
//! references and rename work on it, through [`callback_def_at`]. The object
//! registering may be of any script extending its type, so another object only
//! finds the callbacks of its type and parents.
//!
//! The name of the event belongs to no declaration: it is what the strings of
//! [`sites`], in every known script, have in common. A mod may also send it from
//! outside of the scripts (a Flash menu, a DLL). Other string expressions are not
//! followed.

use papyrus_intern::Name;
use papyrus_parser::{
    SyntaxKind, SyntaxToken, TextRange, TextSize,
    ast::{self, AstNode},
};

use crate::{
    FileId, Workspace,
    index::{Flags, MemberKind},
    sema::{Def, FileSource, Sema, Ty},
};

/// How a string is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModEventUse {
    /// `RegisterForModEvent("MyEvent", ...)`
    Register,
    /// `UnregisterForModEvent("MyEvent")`
    Unregister,
    /// `SendModEvent("MyEvent")`, `ModEvent.Create("MyEvent")`
    Send,
    /// `RegisterForModEvent(..., "OnMyEvent")`
    Callback,
}

/// A string literal naming a custom event or its callback.
#[derive(Clone, Copy, Debug)]
pub struct ModEventRef<'a> {
    /// The string token, with its quotes.
    pub token: SyntaxToken<'a>,
    /// The text between the quotes.
    pub name: &'a str,
    /// The range of `name`.
    pub name_range: TextRange,
    /// How it is used.
    pub usage: ModEventUse,
    /// For a callback: the script of the object registering.
    pub script: Option<Name>,
    /// The object is `Self`: the scripts extending `script` count too.
    pub on_self: bool,
}

/// The use of the string `token`, as an argument of the functions of SKSE.
pub fn mod_event_ref<'a>(sema: &Sema<'a>, token: SyntaxToken<'a>) -> Option<ModEventRef<'a>> {
    if token.kind() != SyntaxKind::STRING {
        return None;
    }
    let literal = token.parent()?;
    let arg = ast::Arg::cast(literal.parent()?)?;
    if arg.value()?.syntax() != literal {
        return None;
    }
    let list = arg.syntax().parent()?;
    let call = ast::CallExpr::cast(list.parent()?)?;
    let callee = call.callee_name()?;
    let func = callee.text();
    let is = |name: &str| func.eq_ignore_ascii_case(name);
    if !(is("RegisterForModEvent")
        || is("UnregisterForModEvent")
        || is("SendModEvent")
        || is("Create"))
    {
        return None;
    }
    let Def::Member { file, index } = sema.resolve_name_ref(callee)? else { return None };
    let member = sema.member(file, index);
    if !member.flags.contains(Flags::NATIVE) {
        return None;
    }
    let position = match arg.name() {
        Some(name) => {
            member.params.iter().position(|p| p.name.as_str().eq_ignore_ascii_case(name.text()))?
        }
        None => ast::ArgList::cast(list)?.args().position(|a| a.syntax() == arg.syntax())?,
    };
    let usage = match position {
        0 if is("RegisterForModEvent") => ModEventUse::Register,
        1 if is("RegisterForModEvent") => ModEventUse::Callback,
        0 if is("UnregisterForModEvent") => ModEventUse::Unregister,
        0 if is("SendModEvent") => ModEventUse::Send,
        0 if sema.ws.file(file).index.name_str().eq_ignore_ascii_case("ModEvent") => {
            ModEventUse::Send
        }
        _ => return None,
    };
    let (script, on_self) = match call.callee()? {
        ast::Expr::Field(field) if usage == ModEventUse::Callback => {
            let receiver = field.receiver()?;
            let on_self = matches!(receiver, ast::Expr::NameRef(n) if n.is_self());
            match sema.infer(receiver)? {
                Ty::Value(ty) => (ty.object(), on_self),
                _ => (None, false),
            }
        }
        ast::Expr::Field(_) => (None, false),
        _ => (sema.script_name(), true),
    };
    let text = token.text();
    let inner = text.strip_prefix('"')?;
    // Without closing quote while it is being typed.
    let name = inner.strip_suffix('"').unwrap_or(inner);
    if name.contains('\\') {
        return None;
    }
    let start = token.text_range().start() + TextSize::new(1);
    let name_range = TextRange::at(start, TextSize::of(name));
    Some(ModEventRef { token, name, name_range, usage, script, on_self })
}

/// The string around `offset`, between its quotes.
pub fn mod_event_at<'a>(sema: &Sema<'a>, offset: TextSize) -> Option<ModEventRef<'a>> {
    let token = sema.root.token_at_offset(offset).max_by_key(|t| t.kind() == SyntaxKind::STRING)?;
    mod_event_ref(sema, token).filter(|r| r.name_range.contains_inclusive(offset))
}

/// An event a callback may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackDecl {
    /// The name, as declared.
    pub name: Name,
    /// The file declaring it.
    pub file: FileId,
    /// Its index in the members of the file.
    pub index: u32,
    /// How far from the object: 0 its script, 1 a parent, 2 a script extending it.
    pub distance: u8,
}

/// The functions and events the callback `callback` may name, the nearest first:
/// of the script and its parents, then of the scripts extending it, when the
/// object is `Self`.
pub fn callables(sema: &Sema<'_>, callback: &ModEventRef<'_>) -> Vec<CallbackDecl> {
    let Some(script) = callback.script else { return Vec::new() };
    let of = |file: FileId, distance: u8| {
        let members = sema.ws.file(file).index.members.iter().enumerate();
        members
            .filter(|(_, m)| m.is_callable() && m.property.is_none())
            .map(move |(i, m)| CallbackDecl { name: m.name, file, index: i as u32, distance })
    };
    let mut out: Vec<CallbackDecl> = sema
        .chain(script)
        .enumerate()
        .flat_map(|(i, (file, _))| of(file, u8::from(i > 0)))
        .collect();
    if callback.on_self {
        for (name, file) in sema.ws.all_scripts_from(sema.file) {
            if name != script && sema.chain(name).any(|(_, i)| i.name == Some(script)) {
                out.extend(of(file, 2));
            }
        }
    }
    out
}

/// The declaration named by the callback `callback`, the nearest one.
fn callback_decl(sema: &Sema<'_>, callback: &ModEventRef<'_>) -> Option<CallbackDecl> {
    let name = Name::new(callback.name);
    let script = callback.script?;
    let own = sema.chain(script).enumerate().find_map(|(distance, (file, index))| {
        let i = index.find(name, |m| m.is_callable())?;
        Some(CallbackDecl { name, file, index: i as u32, distance: u8::from(distance > 0) })
    });
    if own.is_some() || !callback.on_self {
        return own;
    }
    sema.ws
        .files_from(sema.file)
        .filter(|(file, data)| {
            data.index.mentions(name)
                && data.index.name.is_some_and(|other| {
                    other != script
                        && sema.ws.script(other) == Some(*file)
                        && sema.chain(other).any(|(_, i)| i.name == Some(script))
                })
        })
        .find_map(|(file, data)| {
            let i = data.index.find(name, |m| m.is_callable())?;
            Some(CallbackDecl { name, file, index: i as u32, distance: 2 })
        })
}

/// Whether the callback `callback` names a declaration that can be looked up.
pub fn is_declared(sema: &Sema<'_>, callback: &ModEventRef<'_>) -> bool {
    callback_decl(sema, callback).is_some()
}

/// The declaration named by the callback at `offset`, with the range of the name.
pub(crate) fn callback_def_at(sema: &Sema<'_>, offset: TextSize) -> Option<(TextRange, Def)> {
    let callback = mod_event_at(sema, offset).filter(|r| r.usage == ModEventUse::Callback)?;
    let decl = callback_decl(sema, &callback)?;
    Some((callback.name_range, Def::Member { file: decl.file, index: decl.index }))
}

/// Whether the string `callback`, of the same name, names `def` or something
/// overriding it.
pub(crate) fn names_callable(sema: &Sema<'_>, callback: &ModEventRef<'_>, def: Def) -> bool {
    callback.usage == ModEventUse::Callback
        && callback_decl(sema, callback)
            .is_some_and(|d| Def::Member { file: d.file, index: d.index } == def)
}

/// A string naming a custom event in a known script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModEventSite {
    /// The file.
    pub file: FileId,
    /// The range of the name.
    pub range: TextRange,
    /// The name.
    pub name: String,
    /// How it is used, not [`ModEventUse::Callback`].
    pub usage: ModEventUse,
}

/// Where the scripts `from` may know name custom events. Only the few scripts
/// using these functions are read.
pub fn sites(ws: &Workspace, from: FileId) -> Vec<ModEventSite> {
    let names = [
        Name::new("RegisterForModEvent"),
        Name::new("UnregisterForModEvent"),
        Name::new("SendModEvent"),
        Name::new("ModEvent"),
    ];
    let mut out = Vec::new();
    for (file, data) in ws.files_from(from) {
        if !names.iter().any(|&n| data.index.mentions(n)) {
            continue;
        }
        let Some(src) = FileSource::load(ws, file) else { continue };
        let sema = src.sema(ws, file);
        out.extend(sema.root.descendant_tokens().filter_map(|t| {
            let r = mod_event_ref(&sema, t)?;
            (r.usage != ModEventUse::Callback && !r.name.is_empty()).then(|| ModEventSite {
                file,
                range: r.name_range,
                name: r.name.to_owned(),
                usage: r.usage,
            })
        }));
    }
    out
}

/// Whether `decl` is an event, not a function.
pub fn is_event(sema: &Sema<'_>, decl: &CallbackDecl) -> bool {
    sema.member(decl.file, decl.index).kind == MemberKind::Event
}
