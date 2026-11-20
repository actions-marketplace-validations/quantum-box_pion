use crate::LIMIT;

pub struct Token {
    pub age: u32,
}

impl Token {
    pub fn is_expired(&self) -> bool {
        self.age > LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_roundtrip() {
        assert!(!Token { age: 1 }.is_expired());
    }
}
