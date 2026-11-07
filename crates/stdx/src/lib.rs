//! Helpers missing from the standard library, without dependencies and
//! without any knowledge of Papyrus.

/// The Levenshtein distance of `a` and `b`, in characters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = cost.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// The `candidates` closest to `word`, ignoring ASCII case, without
/// duplicates: likely what was meant by a typo. Empty when none is close: at
/// most a third of the length of `word` differs (one character at least).
pub fn closest_matches<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let word = word.to_ascii_lowercase();
    let limit = (word.chars().count() / 3).max(1);
    let mut best = Vec::new();
    let mut best_distance = limit + 1;
    for candidate in candidates {
        let distance = edit_distance(&word, &candidate.to_ascii_lowercase());
        if distance < best_distance {
            best_distance = distance;
            best.clear();
        }
        if distance == best_distance && !best.contains(&candidate) {
            best.push(candidate);
        }
    }
    best
}

/// Paths left out by patterns, each relative to the directory of its file:
///
/// - `*` is any text of a name, `?` one character, `**` any directories;
/// - a pattern with a `/`, except at the end, starts at that directory; without,
///   it matches at any depth;
/// - a directory leaves out what is below it; a `/` at the end matches directories
///   only.
///
/// ASCII case is ignored, as on Windows, where the scripts of the game are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathExcludes {
    patterns: Vec<(Vec<String>, String)>,
}

impl PathExcludes {
    /// Adds `pattern`, relative to the directory `base`.
    pub fn add(&mut self, base: &std::path::Path, pattern: &str) {
        self.patterns.push((components(base), pattern.replace('\\', "/").to_ascii_lowercase()));
    }

    /// Whether `path`, of a file, is left out.
    pub fn matches(&self, path: &std::path::Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let path = components(path);
        self.patterns.iter().any(|(base, pattern)| {
            path.strip_prefix(base.as_slice()).is_some_and(|rest| pattern_matches(pattern, rest))
        })
    }
}

/// The lowercase components of `path`, made absolute.
fn components(path: &std::path::Path) -> Vec<String> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    path.components().map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase()).collect()
}

fn pattern_matches(pattern: &str, path: &[String]) -> bool {
    let directories_only = pattern.ends_with('/');
    let pattern = pattern.trim_end_matches('/');
    let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
    let anchored = pattern.contains('/');
    let mut segments: Vec<&str> = pattern.trim_start_matches('/').split('/').collect();
    if !anchored {
        segments.insert(0, "**");
    }
    let last = path.len().saturating_sub(usize::from(directories_only));
    (1..=last).any(|n| segments_match(&segments, &path[..n]))
}

fn segments_match(segments: &[&str], path: &[String]) -> bool {
    match segments.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(name, tail)| name_matches(first, name) && segments_match(rest, tail)),
    }
}

fn name_matches(glob: &str, name: &str) -> bool {
    fn go(glob: &[char], name: &[char]) -> bool {
        match glob.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (0..=name.len()).any(|skip| go(rest, &name[skip..])),
            Some(('?', rest)) => !name.is_empty() && go(rest, &name[1..]),
            Some((c, rest)) => name.first() == Some(c) && go(rest, &name[1..]),
        }
    }
    go(&glob.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn excluded(pattern: &str, path: &str) -> bool {
        let base = std::env::temp_dir().join("excludes");
        let mut excludes = PathExcludes::default();
        excludes.add(&base, pattern);
        excludes.matches(&base.join(path))
    }

    #[test]
    fn exclude_patterns() {
        // Names match at any depth.
        assert!(excluded("Gen.psc", "Gen.psc"));
        assert!(excluded("Gen.psc", "a/b/gen.PSC"));
        assert!(excluded("*_gen.psc", "a/x_gen.psc"));
        assert!(excluded("vendor", "a/vendor/b/X.psc"));
        assert!(!excluded("Gen.psc", "Gen2.psc"));

        // With a `/`: from the directory of the file.
        assert!(excluded("src/vendor", "src/vendor/X.psc"));
        assert!(excluded("./src/vendor", "src/vendor/X.psc"));
        assert!(excluded("/X.psc", "X.psc"));
        assert!(!excluded("/X.psc", "a/X.psc"));
        assert!(!excluded("src/vendor", "a/src/vendor/X.psc"));

        // Wildcards.
        assert!(excluded("src/**", "src/a/b/X.psc"));
        assert!(excluded("src/**/X.psc", "src/X.psc"));
        assert!(excluded("src/**/X.psc", "src/a/b/X.psc"));
        assert!(excluded("**/gen/*.psc", "a/gen/X.psc"));
        assert!(excluded("src/?.psc", "src/X.psc"));
        assert!(!excluded("src/*.psc", "src/a/X.psc"));
        assert!(!excluded("src/?.psc", "src/XY.psc"));

        // A `/` at the end: directories only. Windows separators.
        assert!(excluded("vendor/", "vendor/X.psc"));
        assert!(!excluded("X.psc/", "X.psc"));
        assert!(excluded("src\\vendor", "src/vendor/X.psc"));
    }

    #[test]
    fn excludes_are_relative_to_their_directory() {
        let base = std::env::temp_dir().join("excludes");
        let mut excludes = PathExcludes::default();
        assert!(!excludes.matches(&base.join("X.psc")));
        excludes.add(&base.join("a"), "X.psc");
        assert!(excludes.matches(&base.join("a/b/X.psc")));
        assert!(!excludes.matches(&base.join("c/X.psc")));

        // Relative paths are of the current directory.
        let mut here = PathExcludes::default();
        here.add(&std::env::current_dir().unwrap(), "X.psc");
        assert!(here.matches(Path::new("./a/X.psc")));
    }

    #[test]
    fn distances() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("busy", "busy"), 0);
        assert_eq!(edit_distance("bussy", "busy"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("あい", "あう"), 1);
    }

    #[test]
    fn closest() {
        let states = ["Busy", "Idle", "Waiting", "Busy"];
        assert_eq!(closest_matches("bussy", states), ["Busy"]);
        assert_eq!(closest_matches("IDEL", states), ["Idle"]);
        assert_eq!(closest_matches("Waitng", states), ["Waiting"]);
        assert_eq!(closest_matches("Nothing Like It", states), Vec::<&str>::new());
        // Ties are all kept; only the closest count.
        assert_eq!(closest_matches("bat", ["bad", "bar", "bats!"]), ["bad", "bar"]);
    }
}
