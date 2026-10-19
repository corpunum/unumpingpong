// TODO: implement `pub fn is_palindrome(s: &str) -> bool`, ignoring case, spaces and punctuation.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palindromes() {
        assert!(is_palindrome("A man, a plan, a canal: Panama"));
        assert!(is_palindrome("Was it a car or a cat I saw?"));
        assert!(is_palindrome(""));
        assert!(!is_palindrome("ping pong"));
    }
}
