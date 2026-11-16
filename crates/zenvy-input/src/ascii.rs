//! US-layout ASCII → (key name, needs shift). This is the *partial* fallback only; the
//! preferred text path supports full Unicode.

pub fn keystroke(c: char) -> Option<(&'static str, bool)> {
    const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
    const NAMES: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    if let Some(i) = LETTERS
        .find(c.to_ascii_lowercase())
        .filter(|_| c.is_ascii_alphabetic())
    {
        return Some((NAMES[i], c.is_ascii_uppercase()));
    }
    Some(match c {
        '1' => ("1", false),
        '2' => ("2", false),
        '3' => ("3", false),
        '4' => ("4", false),
        '5' => ("5", false),
        '6' => ("6", false),
        '7' => ("7", false),
        '8' => ("8", false),
        '9' => ("9", false),
        '0' => ("0", false),
        '!' => ("1", true),
        '@' => ("2", true),
        '#' => ("3", true),
        '$' => ("4", true),
        '%' => ("5", true),
        '^' => ("6", true),
        '&' => ("7", true),
        '*' => ("8", true),
        '(' => ("9", true),
        ')' => ("0", true),
        ' ' => ("space", false),
        '-' => ("minus", false),
        '_' => ("minus", true),
        '=' => ("equal", false),
        '+' => ("equal", true),
        ',' => ("comma", false),
        '<' => ("comma", true),
        '.' => ("period", false),
        '>' => ("period", true),
        '/' => ("slash", false),
        '?' => ("slash", true),
        ';' => ("semicolon", false),
        ':' => ("semicolon", true),
        '\'' => ("apostrophe", false),
        '"' => ("apostrophe", true),
        '[' => ("leftbracket", false),
        '{' => ("leftbracket", true),
        ']' => ("rightbracket", false),
        '}' => ("rightbracket", true),
        '\\' => ("backslash", false),
        '|' => ("backslash", true),
        '`' => ("grave", false),
        '~' => ("grave", true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_ascii_character_has_a_us_keystroke() {
        for b in 0x20u8..=0x7e {
            assert!(keystroke(b as char).is_some(), "{:?}", b as char);
        }
        assert!(
            keystroke('\u{e9}').is_none() && keystroke('\n').is_none() && keystroke('\t').is_none()
        );
        assert_eq!(keystroke('~'), Some(("grave", true)));
        assert_eq!(keystroke('A'), Some(("a", true)));
        assert_eq!(keystroke(':'), Some(("semicolon", true)));
        assert_eq!(keystroke('|'), Some(("backslash", true)));
    }

    #[test]
    fn every_keystroke_names_a_known_key() {
        for b in 0x20u8..=0x7e {
            let (name, _) = keystroke(b as char).unwrap();
            assert!(
                zenvy_proto::keycode(name).is_some(),
                "{name} for {:?}",
                b as char
            );
        }
    }
}
