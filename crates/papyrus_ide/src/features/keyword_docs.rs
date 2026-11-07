//! Documentation of the Papyrus keywords, shown on hover like the keyword
//! documentation of rust-analyzer: a description followed by examples.
//!
//! Each keyword is documented by a Markdown file of the `keyword_docs`
//! directory, `<keyword>.md` for scripts and `flags/<keyword>.md` for flags
//! files. A file holds the description, then a `# Examples` section with a
//! ```` ```papyrus ```` code block. The tests check that every keyword has a
//! file and that the examples parse.

use papyrus_parser::SyntaxKind;

/// Documentation of one keyword.
pub(crate) struct KeywordDoc {
    /// The canonical spelling.
    pub(crate) name: &'static str,
    /// The Markdown documentation: description and examples.
    pub(crate) body: &'static str,
}

impl KeywordDoc {
    /// Renders the documentation as hover Markdown.
    pub(crate) fn markdown(&self) -> String {
        // Checkouts with CRLF line breaks give CRLF bodies.
        let body = crate::index::hide_test_lines(&self.body.trim_end().replace("\r\n", "\n"));
        format!("```papyrus\n{}\n```\n*keyword*\n\n---\n\n{body}", self.name)
    }
}

macro_rules! docs {
    ($($name:literal => $file:literal),* $(,)?) => {
        &[$(KeywordDoc { name: $name, body: include_str!(concat!("keyword_docs/", $file)) }),*]
    };
}

/// The documentation of a keyword token, if it is one.
pub(crate) fn keyword_doc(kind: SyntaxKind) -> Option<&'static KeywordDoc> {
    if !kind.is_keyword() {
        return None;
    }
    // Keyword token kinds are named after the keyword: `WHILE_KW` is `While`.
    let name = format!("{kind:?}");
    let name = name.strip_suffix("_KW")?;
    SCRIPT_DOCS.iter().find(|d| d.name.eq_ignore_ascii_case(name))
}

/// The documentation of the keywords of scripts.
pub(crate) fn script_keyword_docs() -> &'static [KeywordDoc] {
    SCRIPT_DOCS
}

const SCRIPT_DOCS: &[KeywordDoc] = docs![
    "As" => "as.md",
    "Auto" => "auto.md",
    "AutoReadOnly" => "autoreadonly.md",
    "Bool" => "bool.md",
    "Else" => "else.md",
    "ElseIf" => "elseif.md",
    "EndEvent" => "endevent.md",
    "EndFunction" => "endfunction.md",
    "EndIf" => "endif.md",
    "EndProperty" => "endproperty.md",
    "EndState" => "endstate.md",
    "EndWhile" => "endwhile.md",
    "Event" => "event.md",
    "Extends" => "extends.md",
    "False" => "false.md",
    "Float" => "float.md",
    "Function" => "function.md",
    "Global" => "global.md",
    "If" => "if.md",
    "Import" => "import.md",
    "Int" => "int.md",
    "Native" => "native.md",
    "New" => "new.md",
    "None" => "none.md",
    "Parent" => "parent.md",
    "Property" => "property.md",
    "Return" => "return.md",
    "ScriptName" => "scriptname.md",
    "Self" => "self.md",
    "State" => "state.md",
    "String" => "string.md",
    "True" => "true.md",
    "While" => "while.md",
];

/// The documentation of a keyword of the flags file (`Flag` and the targets).
pub(crate) fn flag_file_keyword_doc(word: &str) -> Option<&'static KeywordDoc> {
    const FLAG_DOCS: &[KeywordDoc] = docs![
        "Flag" => "flags/flag.md",
        "Script" => "flags/script.md",
        "Property" => "flags/property.md",
        "Variable" => "flags/variable.md",
        "Function" => "flags/function.md",
    ];
    FLAG_DOCS.iter().find(|d| d.name.eq_ignore_ascii_case(word))
}

#[cfg(test)]
mod tests {
    use papyrus_parser::SyntaxKind::*;

    use super::*;

    const KEYWORDS: [SyntaxKind; 33] = [
        AS_KW,
        AUTO_KW,
        AUTOREADONLY_KW,
        BOOL_KW,
        ELSE_KW,
        ELSEIF_KW,
        ENDEVENT_KW,
        ENDFUNCTION_KW,
        ENDIF_KW,
        ENDPROPERTY_KW,
        ENDSTATE_KW,
        ENDWHILE_KW,
        EVENT_KW,
        EXTENDS_KW,
        FALSE_KW,
        FLOAT_KW,
        FUNCTION_KW,
        GLOBAL_KW,
        IF_KW,
        IMPORT_KW,
        INT_KW,
        NATIVE_KW,
        NEW_KW,
        NONE_KW,
        PARENT_KW,
        PROPERTY_KW,
        RETURN_KW,
        SCRIPTNAME_KW,
        SELF_KW,
        STATE_KW,
        STRING_KW,
        TRUE_KW,
        WHILE_KW,
    ];

    /// The code of the `papyrus` block of the examples section.
    fn example(doc: &KeywordDoc) -> &'static str {
        let section = doc.body.split("# Examples").nth(1).unwrap_or_else(|| {
            panic!("{}: missing `# Examples` section", doc.name);
        });
        // After the opening fence line, `\n` or `\r\n`.
        let fence = section.find("```papyrus").expect("papyrus code block");
        let start = fence + section[fence..].find('\n').expect("fence line") + 1;
        let end = section[start..].find("```").expect("closing fence");
        &section[start..start + end]
    }

    /// The code of a doc test: the hidden lines shown.
    fn unhidden(code: &str) -> String {
        let lines = code.lines().map(|l| {
            let t = l.trim_start();
            if t == "#" { "" } else { t.strip_prefix("# ").unwrap_or(l) }
        });
        lines.map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn every_keyword_is_documented() {
        let missing: Vec<_> = KEYWORDS.into_iter().filter(|k| keyword_doc(*k).is_none()).collect();
        assert!(missing.is_empty(), "undocumented: {missing:?}");
        for word in ["Flag", "script", "Property", "Variable", "Function"] {
            assert!(flag_file_keyword_doc(word).is_some(), "{word}");
        }
    }

    #[test]
    fn script_examples_parse() {
        for kind in KEYWORDS {
            let doc = keyword_doc(kind).unwrap();
            let example = &unhidden(example(doc));
            let src = if example.starts_with("ScriptName") {
                example.to_owned()
            } else if example.contains("Function ")
                || example.contains("Property ")
                || example.contains("State ")
                || example.starts_with("Event ")
            {
                format!("ScriptName T\n{example}")
            } else {
                format!("ScriptName T\nFunction F()\n{example}EndFunction\n")
            };
            let parse = papyrus_parser::parse(&src);
            assert!(parse.errors().is_empty(), "{}: {:?}\n{src}", doc.name, parse.errors());
        }
    }

    #[test]
    fn flag_file_examples_parse() {
        let (defs, errors) =
            crate::flags::parse_flags(example(flag_file_keyword_doc("Flag").unwrap()));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(defs.len(), 2);
    }
}
