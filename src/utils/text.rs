pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    let char_count = input.chars().count();
    if char_count <= max_chars {
        return input.to_owned();
    }
    let truncated: String = input.chars().take(max_chars).collect();
    format!("{}...", truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii() {
        let input = "Hello World";
        assert_eq!(truncate_chars(input, 5), "Hello...");
        assert_eq!(truncate_chars(input, 20), "Hello World");
    }

    #[test]
    fn test_truncate_vietnamese() {
        let input = "Tiếng Việt có dấu";
        assert_eq!(truncate_chars(input, 10), "Tiếng Việt...");
        assert_eq!(truncate_chars(input, 30), "Tiếng Việt có dấu");
    }

    #[test]
    fn test_truncate_thai() {
        let input = "เพลงไทยยาวมาก";
        assert_eq!(truncate_chars(input, 7), "เพลงไทย...");
    }

    #[test]
    fn test_truncate_japanese() {
        let input = "日本語のタイトルです";
        assert_eq!(truncate_chars(input, 4), "日本語の...");
    }

    #[test]
    fn test_truncate_emoji() {
        let input = "😊😂🤣😍😒😘";
        assert_eq!(truncate_chars(input, 3), "😊😂🤣...");
    }
}
