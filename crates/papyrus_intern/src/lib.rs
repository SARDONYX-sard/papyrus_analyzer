//! A global, thread-safe string interner.
//!
//! Papyrus identifiers are case-insensitive, and a language server sees the same
//! handful of names (`Actor`, `Debug`, `akTarget`, ...) thousands of times across a
//! workspace. Interning stores every distinct string exactly once and replaces it
//! with a 4-byte [`Symbol`], which is `Copy`, cheap to hash and cheap to compare.
//!
//! Interned strings are never freed: the set of distinct identifiers in a workspace
//! is small and bounded, so leaking them gives `&'static str` access without any
//! lifetime juggling.
//!
//! # Example
//!
//! ```
//! use papyrus_intern::{Name, Symbol};
//!
//! let a = Symbol::intern("Actor");
//! assert_eq!(a, Symbol::intern("Actor"));
//! assert_eq!(a.as_str(), "Actor");
//!
//! // `Name` compares case-insensitively but remembers the original spelling.
//! let n1 = Name::new("GetActorBase");
//! let n2 = Name::new("getactorbase");
//! assert_eq!(n1, n2);
//! assert_eq!(n1.as_str(), "GetActorBase");
//! ```

use std::{
    fmt,
    hash::{Hash, Hasher},
    num::NonZeroU32,
    sync::{OnceLock, RwLock},
};

use rustc_hash::FxHashMap;

/// An interned string.
///
/// Two symbols are equal if and only if the strings they were created from are
/// byte-for-byte equal. Use [`Symbol::intern_folded`] or [`Name`] for
/// case-insensitive identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(NonZeroU32);

impl Symbol {
    /// Interns `s` exactly as written.
    pub fn intern(s: &str) -> Symbol {
        interner().intern(s)
    }

    /// Interns the ASCII-lowercased form of `s`.
    ///
    /// Papyrus identifiers are ASCII-only and case-insensitive, so this is the
    /// canonical key used for name lookups.
    pub fn intern_folded(s: &str) -> Symbol {
        if !s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Symbol::intern(s);
        }

        // Most identifiers are short; fold them on the stack to avoid allocating
        // for lookups of strings that are already interned.
        let mut buf = [0u8; 64];
        if s.len() <= buf.len() {
            let buf = &mut buf[..s.len()];
            buf.copy_from_slice(s.as_bytes());
            buf.make_ascii_lowercase();
            // ASCII lowercasing keeps the buffer valid UTF-8.
            let folded = std::str::from_utf8(buf).unwrap_or(s);
            Symbol::intern(folded)
        } else {
            Symbol::intern(&s.to_ascii_lowercase())
        }
    }

    /// Returns the string this symbol was created from.
    pub fn as_str(self) -> &'static str {
        interner().resolve(self)
    }

    /// Returns the symbol of the case-folded version of this string.
    pub fn folded(self) -> Symbol {
        Symbol::intern_folded(self.as_str())
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Symbol::intern(s)
    }
}

/// A case-insensitive identifier that keeps its original spelling.
///
/// Equality, ordering and hashing only consider [`Name::key`], the case-folded
/// symbol, so `Name::new("Foo") == Name::new("FOO")`. [`Name::as_str`] still
/// returns the spelling used at the declaration site, which is what users expect
/// to see in hovers and completions.
#[derive(Clone, Copy)]
pub struct Name {
    key: Symbol,
    text: Symbol,
}

impl Name {
    /// Creates a name from its source spelling.
    pub fn new(text: &str) -> Name {
        let text_sym = Symbol::intern(text);
        Name { key: Symbol::intern_folded(text), text: text_sym }
    }

    /// The case-folded symbol used for comparisons.
    pub fn key(self) -> Symbol {
        self.key
    }

    /// The original spelling.
    pub fn as_str(self) -> &'static str {
        self.text.as_str()
    }
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Name {}

impl PartialEq<Symbol> for Name {
    fn eq(&self, other: &Symbol) -> bool {
        self.key == *other
    }
}

impl Hash for Name {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Default)]
struct Inner {
    map: FxHashMap<&'static str, Symbol>,
    /// How many strings [`Interner::strings`] holds.
    len: usize,
}

struct Interner {
    inner: RwLock<Inner>,
    /// The strings by symbol, read without the lock: [`Symbol::as_str`] runs
    /// all the time, on every thread compiling, and a shared lock taken that
    /// often makes the threads wait for each other.
    strings: Strings,
}

fn interner() -> &'static Interner {
    static INTERNER: OnceLock<Interner> = OnceLock::new();
    INTERNER.get_or_init(|| Interner {
        inner: RwLock::new(Inner::default()),
        strings: Strings::default(),
    })
}

/// An append-only array readable without lock: chunk `k` holds the
/// `2^k` strings from index `2^k - 1`, allocated when the first one is
/// added; added strings never move.
struct Strings {
    chunks: [OnceLock<Box<[OnceLock<&'static str>]>>; 32],
}

impl Default for Strings {
    fn default() -> Strings {
        Strings { chunks: std::array::from_fn(|_| OnceLock::new()) }
    }
}

impl Strings {
    /// The chunk and the position in it of the string `index`.
    fn place(index: usize) -> (usize, usize) {
        let chunk = (usize::BITS - 1 - (index + 1).leading_zeros()) as usize;
        (chunk, index + 1 - (1 << chunk))
    }

    /// Adds the string `index`; under the write lock, one at a time.
    fn set(&self, index: usize, s: &'static str) {
        let (chunk, at) = Strings::place(index);
        let slots = self.chunks[chunk]
            .get_or_init(|| (0..1usize << chunk).map(|_| OnceLock::new()).collect());
        let _ = slots[at].set(s);
    }

    fn get(&self, index: usize) -> &'static str {
        let (chunk, at) = Strings::place(index);
        self.chunks[chunk].get().and_then(|slots| slots[at].get()).expect("interned symbol")
    }
}

impl Interner {
    /// The symbol of `s`: from the cache of the thread when it has seen `s`,
    /// else from the shared map, under its lock.
    fn intern(&self, s: &str) -> Symbol {
        thread_local! {
            static SEEN: std::cell::RefCell<FxHashMap<&'static str, Symbol>> =
                std::cell::RefCell::default();
        }
        if let Some(sym) = SEEN.with_borrow(|seen| seen.get(s).copied()) {
            return sym;
        }
        let sym = self.intern_shared(s);
        SEEN.with_borrow_mut(|seen| seen.insert(self.resolve(sym), sym));
        sym
    }

    fn intern_shared(&self, s: &str) -> Symbol {
        if let Some(&sym) = self.read().map.get(s) {
            return sym;
        }

        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        // Another thread may have interned it between the two locks.
        if let Some(&sym) = inner.map.get(s) {
            return sym;
        }

        let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
        // Stored before the symbol exists: whoever gets it can read it.
        self.strings.set(inner.len, leaked);
        inner.len += 1;
        let index = u32::try_from(inner.len).expect("interner overflow");
        let sym = Symbol(NonZeroU32::new(index).expect("index starts at 1"));
        inner.map.insert(leaked, sym);
        sym
    }

    fn resolve(&self, sym: Symbol) -> &'static str {
        self.strings.get(sym.0.get() as usize - 1)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_roundtrip() {
        let a = Symbol::intern("Hello");
        let b = Symbol::intern("Hello");
        let c = Symbol::intern("hello");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str(), "Hello");
        assert_eq!(a.folded(), c);
    }

    #[test]
    fn folded_long_string() {
        let long = "A".repeat(100);
        assert_eq!(Symbol::intern_folded(&long).as_str(), "a".repeat(100));
    }

    #[test]
    fn name_is_case_insensitive() {
        let a = Name::new("OnInit");
        let b = Name::new("ONINIT");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "OnInit");
        assert_eq!(b.as_str(), "ONINIT");
        assert_eq!(a, Symbol::intern("oninit"));
    }

    #[test]
    fn concurrent_interning() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..1000).map(|i| Symbol::intern(&i.to_string())).collect::<Vec<_>>()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for r in &results[1..] {
            assert_eq!(r, &results[0]);
        }
    }
}
