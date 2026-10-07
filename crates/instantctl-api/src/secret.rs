use std::fmt;

/// A string that is safe to include in debug output.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Access the secret explicitly when constructing an authenticated request.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(redacted)")
    }
}

#[cfg(test)]
mod tests {
    use super::SecretString;

    #[test]
    fn debug_is_redacted() {
        let secret = SecretString::new("portal-token-value");

        assert_eq!(format!("{secret:?}"), "(redacted)");
        assert!(!format!("{secret:?}").contains("portal-token-value"));
    }
}
