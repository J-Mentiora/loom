/// Pluralization helper per D-28: "1 request" / "2 requests".
/// Doesn't pluralize uncountables — caller chooses whether to use this.
pub fn plural(n: u64, singular: &str) -> String {
    if n == 1 {
        format!("{} {}", n, singular)
    } else {
        format!("{} {}s", n, singular)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singular() {
        assert_eq!(plural(1, "error"), "1 error");
    }

    #[test]
    fn plural_zero_uses_s() {
        assert_eq!(plural(0, "error"), "0 errors");
    }

    #[test]
    fn plural_many() {
        assert_eq!(plural(42, "request"), "42 requests");
    }
}
