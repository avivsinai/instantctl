//! Secret input follows the login command: stdin has one bounded line, and
//! interactive input uses a hidden terminal prompt. No secret is an argument.
use std::io::{self, IsTerminal, Read};

use instantctl_api::{Error, ErrorKind, client::wlan::Patch, secret::SecretString};

use crate::credentials::CredentialSource;

const MAX_LINE_BYTES: usize = 63;

pub(super) fn validate_input(from_stdin: bool, source: CredentialSource) -> Result<(), Error> {
    validate_mode(from_stdin, io::stdin().is_terminal(), source)
}

fn validate_mode(from_stdin: bool, terminal: bool, source: CredentialSource) -> Result<(), Error> {
    if matches!(source, CredentialSource::Stdin) {
        return Err(usage(
            "passphrase input cannot share stdin with --token-stdin",
        ));
    }
    if from_stdin && terminal {
        return Err(usage(
            "omit --passphrase-stdin at a terminal to use a hidden prompt",
        ));
    }
    if !from_stdin && !terminal {
        return Err(usage(
            "passphrase input requires a terminal or --passphrase-stdin",
        ));
    }
    Ok(())
}

pub(super) fn collect(from_stdin: bool) -> Result<SecretString, Error> {
    let secret = if from_stdin {
        read_line(&mut io::stdin().lock())?
    } else {
        rpassword::prompt_password("WiFi passphrase: ")
            .map_err(|_| Error::new(ErrorKind::General, "could not read passphrase input"))?
    };
    let secret = SecretString::new(secret);
    // Reuse the library's PSK length/charset rules and fixed diagnostics.
    Patch {
        passphrase: Some(secret.clone()),
        ..Patch::default()
    }
    .validate()?;
    Ok(secret)
}

fn read_line(reader: &mut impl Read) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(MAX_LINE_BYTES + 1);
    let mut byte = [0; 1];
    let mut ended_with_lf = false;
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => {
                ended_with_lf = true;
                break;
            }
            Ok(_) if bytes.len() < MAX_LINE_BYTES + 1 => bytes.push(byte[0]),
            Ok(_) => return Err(usage("passphrase input exceeds the 63-byte limit")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(Error::new(
                    ErrorKind::General,
                    "could not read passphrase input",
                ));
            }
        }
    }
    if ended_with_lf && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_LINE_BYTES {
        return Err(usage("passphrase input exceeds the 63-byte limit"));
    }
    String::from_utf8(bytes).map_err(|_| usage("passphrase input is not valid UTF-8"))
}

fn usage(message: &'static str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn one_line_reader_accepts_crlf_and_eof_without_consuming_another_line() {
        let mut input = Cursor::new(b"first-key!\r\nsecond-key!\n".to_vec());
        assert_eq!(read_line(&mut input).unwrap(), "first-key!");
        assert_eq!(read_line(&mut input).unwrap(), "second-key!");
        assert_eq!(
            read_line(&mut Cursor::new(b"at-eof-key!".to_vec())).unwrap(),
            "at-eof-key!"
        );
        assert_eq!(
            read_line(&mut Cursor::new(
                format!("{}\r\n", "x".repeat(63)).into_bytes()
            ))
            .unwrap()
            .len(),
            63
        );
    }

    #[test]
    fn malformed_and_oversized_lines_have_fixed_secret_free_errors() {
        for bytes in [
            vec![b'x'; 65_536],
            vec![b'x'; 64],
            b"invalid-utf8-\xff".to_vec(),
        ] {
            let mut input = Cursor::new(bytes.clone());
            let error = read_line(&mut input).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(
                !format!("{error:?} {error}")
                    .contains(&String::from_utf8_lossy(&bytes).to_string())
            );
            assert!(input.position() <= 65);
        }
    }

    #[test]
    fn input_modes_require_hidden_terminal_input_or_explicit_stdin_and_refuse_shared_stdin() {
        assert!(validate_mode(false, true, CredentialSource::Environment).is_ok());
        assert!(validate_mode(true, false, CredentialSource::Environment).is_ok());
        for (from_stdin, tty, source) in [
            (false, false, CredentialSource::Environment),
            (true, true, CredentialSource::Environment),
            (true, false, CredentialSource::Stdin),
            (false, true, CredentialSource::Stdin),
        ] {
            assert_eq!(
                validate_mode(from_stdin, tty, source).unwrap_err().kind,
                ErrorKind::Usage
            );
        }
    }
}
