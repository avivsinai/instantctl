use std::future::{Future, ready};

use crate::secret::SecretString;
use crate::{Error, ErrorKind};

const MAX_TOKEN_BYTES: usize = 16_384;

/// Supplies a session token without exposing its storage mechanism.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> impl Future<Output = Result<SecretString, Error>> + Send;
}

impl<T: TokenSource + ?Sized> TokenSource for std::sync::Arc<T> {
    async fn token(&self) -> Result<SecretString, Error> {
        (**self).token().await
    }
}

/// A token supplied directly by the caller.
#[derive(Clone, Debug)]
pub struct StaticToken(SecretString);

impl StaticToken {
    pub fn new(token: impl Into<String>) -> Result<Self, Error> {
        let token = token.into();
        let bytes = token.as_bytes();

        if bytes.is_empty() {
            return Err(config_error("session token must not be empty"));
        }
        if bytes.len() > MAX_TOKEN_BYTES {
            return Err(config_error("session token exceeds 16384 bytes"));
        }
        if !bytes.iter().all(|byte| (33..=126).contains(byte)) {
            return Err(config_error(
                "session token must contain visible ASCII without whitespace",
            ));
        }

        Ok(Self(SecretString::new(token)))
    }
}

impl TokenSource for StaticToken {
    fn token(&self) -> impl Future<Output = Result<SecretString, Error>> + Send {
        ready(Ok(self.0.clone()))
    }
}

fn config_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Config, message)
}

#[cfg(test)]
mod tests {
    use super::StaticToken;
    use crate::ErrorKind;

    fn error_for(token: &str) -> crate::Error {
        StaticToken::new(token).expect_err("token should be rejected")
    }

    #[test]
    fn accepts_visible_ascii_and_inclusive_byte_limit() {
        assert!(StaticToken::new("!~").is_ok());
        assert!(StaticToken::new("A".repeat(16_384)).is_ok());
    }

    #[test]
    fn rejects_empty_control_unicode_and_oversized_tokens_safely() {
        for (token, expected_message) in [
            ("", "session token must not be empty"),
            (
                "has space",
                "session token must contain visible ASCII without whitespace",
            ),
            (
                "line\nbreak",
                "session token must contain visible ASCII without whitespace",
            ),
            (
                "unicode-☃",
                "session token must contain visible ASCII without whitespace",
            ),
            (&"x".repeat(16_385), "session token exceeds 16384 bytes"),
        ] {
            let error = error_for(token);
            assert_eq!(error.kind, ErrorKind::Config);
            assert_eq!(error.message, expected_message);
            if !token.is_empty() {
                assert!(!error.message.contains(token));
            }
        }
    }
}
