//! UTF-8-safe display truncation shared by presentation and interchange layers.

/// Return the longest prefix of `input` that is at most `max` bytes and ends
/// on a UTF-8 character boundary.
///
/// Inputs already within the bound are returned unchanged. A byte offset that
/// falls inside a multi-byte character is rounded down to the start of that
/// character, so callers can slice or append without ever panicking. The
/// returned slice is always valid UTF-8 and always a prefix of `input`.
#[must_use]
pub fn truncate_utf8_boundary(input: &str, max: usize) -> &str {
    if input.len() <= max {
        return input;
    }
    let mut end = max;
    // At most three bytes are skipped: no UTF-8 character is wider than four
    // bytes, and byte zero is always a boundary.
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_prefix_and_valid(input: &str, max: usize) -> &str {
        let truncated = truncate_utf8_boundary(input, max);
        assert!(input.starts_with(truncated), "result must be a prefix");
        assert!(truncated.len() <= max, "result must respect the bound");
        // Compile-time proof of valid UTF-8: the value is a `&str`.
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
        truncated
    }

    #[test]
    fn shorter_input_is_returned_unchanged() {
        for input in ["", "ascii", "é", "€", "😀", "mixed é€😀 text"] {
            assert_eq!(truncate_utf8_boundary(input, 64), input);
        }
        assert_eq!(truncate_utf8_boundary("é", 2), "é");
    }

    #[test]
    fn single_characters_at_max_minus_one_and_max_plus_one() {
        // 2-byte `é`: `max == len` keeps it, `max - 1` slices it away, and
        // `max + 1` is beyond the input.
        assert_eq!(assert_prefix_and_valid("é", 2), "é");
        assert_eq!(assert_prefix_and_valid("é", 1), "");
        assert_eq!(assert_prefix_and_valid("é", 3), "é");
        // 3-byte `€`.
        assert_eq!(assert_prefix_and_valid("€", 3), "€");
        assert_eq!(assert_prefix_and_valid("€", 2), "");
        assert_eq!(assert_prefix_and_valid("€", 4), "€");
        // 4-byte `😀`.
        assert_eq!(assert_prefix_and_valid("😀", 4), "😀");
        assert_eq!(assert_prefix_and_valid("😀", 3), "");
        assert_eq!(assert_prefix_and_valid("😀", 5), "😀");
    }

    #[test]
    fn two_byte_character_boundaries_are_respected() {
        let input = "aébc";
        assert_eq!(assert_prefix_and_valid(input, 3), "aé");
        assert_eq!(assert_prefix_and_valid(input, 2), "a");
        assert_eq!(assert_prefix_and_valid(input, 5), "aébc");
    }

    #[test]
    fn three_byte_character_boundaries_are_respected() {
        let input = "a€bc";
        assert_eq!(assert_prefix_and_valid(input, 4), "a€");
        assert_eq!(assert_prefix_and_valid(input, 3), "a");
        assert_eq!(assert_prefix_and_valid(input, 6), "a€bc");
    }

    #[test]
    fn four_byte_character_boundaries_are_respected() {
        let input = "a😀bc";
        assert_eq!(assert_prefix_and_valid(input, 5), "a😀");
        assert_eq!(assert_prefix_and_valid(input, 4), "a");
        assert_eq!(assert_prefix_and_valid(input, 6), "a😀b");
        assert_eq!(assert_prefix_and_valid(input, 7), "a😀bc");
    }

    #[test]
    fn byte_512_inside_a_character_rounds_down() {
        let input = "€".repeat(300);
        assert_eq!(input.len(), 900);
        let truncated = assert_prefix_and_valid(&input, 512);
        assert_eq!(truncated.len(), 510, "512 falls inside the 171st '€'");
    }

    #[test]
    fn boundary_exactly_at_max_is_kept() {
        let input = "€€€";
        assert_eq!(assert_prefix_and_valid(input, 6), "€€");
        assert_eq!(assert_prefix_and_valid(input, 9), input);
    }
}
