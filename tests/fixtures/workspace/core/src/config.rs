pub struct Limit(pub u32);

impl Default for Limit {
    fn default() -> Self {
        Limit(3)
    }
}

/// `Default` is derived: the impl that calls `Limit::default` is written by
/// a macro and appears in no source file.
#[derive(Default)]
pub struct Config {
    pub limit: Limit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_a_limit() {
        assert_eq!(Config::default().limit.0, 3);
    }
}
