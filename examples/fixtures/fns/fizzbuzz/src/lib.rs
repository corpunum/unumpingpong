// TODO: implement `pub fn fizzbuzz(n: u32) -> String`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fizzbuzz_rules() {
        assert_eq!(fizzbuzz(1), "1");
        assert_eq!(fizzbuzz(3), "Fizz");
        assert_eq!(fizzbuzz(5), "Buzz");
        assert_eq!(fizzbuzz(15), "FizzBuzz");
        assert_eq!(fizzbuzz(98), "98");
    }
}
