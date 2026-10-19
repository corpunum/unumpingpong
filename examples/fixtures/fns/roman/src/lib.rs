// TODO: implement `pub fn to_roman(n: u32) -> String` for 1..=3999.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerals() {
        assert_eq!(to_roman(1), "I");
        assert_eq!(to_roman(4), "IV");
        assert_eq!(to_roman(9), "IX");
        assert_eq!(to_roman(14), "XIV");
        assert_eq!(to_roman(1994), "MCMXCIV");
        assert_eq!(to_roman(3999), "MMMCMXCIX");
    }
}
