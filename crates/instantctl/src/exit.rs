use instantctl_api::ErrorKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Success,
    Incomplete,
    Error(ErrorKind),
}

impl ExitStatus {
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Incomplete => 4,
            Self::Error(ErrorKind::General) => 1,
            Self::Error(ErrorKind::Config | ErrorKind::Usage | ErrorKind::ConfirmationRequired) => {
                2
            }
            Self::Error(ErrorKind::Auth) => 3,
            Self::Error(ErrorKind::NotFound | ErrorKind::Unsupported) => 4,
            Self::Error(ErrorKind::ClientError | ErrorKind::RetryLater) => 5,
            Self::Error(ErrorKind::Unverified) => 6,
            Self::Error(ErrorKind::BrokenPipe) => 141,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_kind_has_its_documented_exit_code() {
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::Incomplete.code(), 4);
        for (kind, expected) in [
            (ErrorKind::General, 1),
            (ErrorKind::Config, 2),
            (ErrorKind::Usage, 2),
            (ErrorKind::ConfirmationRequired, 2),
            (ErrorKind::Auth, 3),
            (ErrorKind::NotFound, 4),
            (ErrorKind::Unsupported, 4),
            (ErrorKind::ClientError, 5),
            (ErrorKind::RetryLater, 5),
            (ErrorKind::Unverified, 6),
            (ErrorKind::BrokenPipe, 141),
        ] {
            assert_eq!(ExitStatus::Error(kind).code(), expected, "{kind:?}");
        }
    }
}
