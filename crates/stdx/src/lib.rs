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

#[cfg(test)]
mod tests {
    use super::*;

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
