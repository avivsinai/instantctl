use std::io::{self, IsTerminal, Read, Write};

use clap::Args as ClapArgs;
use instantctl_api::{Error, ErrorKind, client::reads::Site, secret::SecretString};

const MAX_LINE_BYTES: usize = 16_384;

#[derive(Debug, ClapArgs)]
pub struct LoginArgs {
    /// Portal account username.
    #[arg(long)]
    pub username: Option<String>,
    /// Read the password from one line on stdin.
    #[arg(long)]
    pub password_stdin: bool,
    /// Read an OTP from stdin after the server requests one.
    #[arg(long, requires = "password_stdin")]
    pub otp_stdin: bool,
}

pub(super) fn validate_input(args: &LoginArgs, token_stdin: bool) -> Result<(), Error> {
    if token_stdin {
        return Err(config("login cannot share stdin with --token-stdin"));
    }
    if args.otp_stdin && !args.password_stdin {
        return Err(config("--otp-stdin requires --password-stdin"));
    }
    if io::stdin().is_terminal() && args.password_stdin {
        return Err(config(
            "omit --password-stdin and --otp-stdin at a terminal to use hidden prompts",
        ));
    }
    if !io::stdin().is_terminal() && (args.username.is_none() || !args.password_stdin) {
        return Err(config(
            "login requires a terminal unless --username and --password-stdin are supplied",
        ));
    }
    Ok(())
}

pub(super) fn collect_username(
    args: &LoginArgs,
    stored_username: Option<&str>,
) -> Result<String, Error> {
    if let Some(username) = args.username.as_deref() {
        return checked_username(username);
    }
    if !io::stdin().is_terminal() {
        return Err(config("login requires a terminal or --username"));
    }

    let default = stored_username.filter(|value| !value.is_empty());
    let prompt = match default {
        Some(value) => format!("Username [{}]: ", terminal_safe(value)),
        None => "Username: ".to_owned(),
    };
    write_prompt(&prompt)?;
    let line = read_line(&mut io::stdin().lock())?;
    let username = if line.is_empty() {
        default.ok_or_else(|| config("username must not be empty"))?
    } else {
        &line
    };
    checked_username(username)
}

pub(super) fn password(args: &LoginArgs) -> Result<SecretString, Error> {
    if args.password_stdin {
        return checked_secret(read_line(&mut io::stdin().lock())?);
    }
    let password = rpassword::prompt_password("Password: ")
        .map_err(|_| Error::new(ErrorKind::General, "could not read password input"))?;
    checked_secret(password)
}

pub(super) fn otp(args: &LoginArgs) -> Result<SecretString, Error> {
    if args.otp_stdin {
        return checked_secret(read_line(&mut io::stdin().lock())?);
    }
    if !io::stdin().is_terminal() {
        return Err(config(
            "OTP required; retry with --otp-stdin and --password-stdin",
        ));
    }
    let otp = rpassword::prompt_password("Authenticator code: ")
        .map_err(|_| Error::new(ErrorKind::General, "could not read OTP input"))?;
    checked_secret(otp)
}

pub(super) fn choose_site(sites: &[Site], explicit: Option<&str>) -> Result<String, Error> {
    if let Some(explicit) = explicit {
        return sites
            .iter()
            .find(|site| site.id.eq_ignore_ascii_case(explicit))
            .map(|site| site.id.to_ascii_lowercase())
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "the selected site was not found"));
    }

    match sites {
        [] => Err(Error::new(ErrorKind::NotFound, "no sites are available")),
        [site] => Ok(site.id.to_ascii_lowercase()),
        _ if !io::stdin().is_terminal() => Err(config(
            "multiple sites are available; specify one with --site",
        )),
        _ => {
            let mut stderr = io::stderr().lock();
            for (index, site) in sites.iter().enumerate() {
                let name = site.name.as_deref().unwrap_or("(unnamed)");
                writeln!(
                    stderr,
                    "{}. {} ({})",
                    index + 1,
                    terminal_safe(name),
                    terminal_safe(&site.id)
                )
                .map_err(|_| Error::new(ErrorKind::General, "could not write site selection"))?;
            }
            drop(stderr);
            write_prompt(&format!("Select site [1-{}]: ", sites.len()))?;
            let selection = read_line(&mut io::stdin().lock())?;
            let index = selection
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=sites.len()).contains(index))
                .ok_or_else(|| config("site selection must be a listed number"))?;
            Ok(sites[index - 1].id.to_ascii_lowercase())
        }
    }
}

fn read_line(reader: &mut impl Read) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(128);
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
            Ok(_) => return Err(config("input line exceeds the 16384-byte limit")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Error::new(ErrorKind::General, "could not read input")),
        }
    }

    if ended_with_lf && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_LINE_BYTES {
        return Err(config("input line exceeds the 16384-byte limit"));
    }
    let line = String::from_utf8(bytes).map_err(|_| config("input line is not valid UTF-8"))?;
    if line.chars().any(char::is_control) {
        return Err(config("input line is malformed"));
    }
    Ok(line)
}

fn checked_secret(secret: String) -> Result<SecretString, Error> {
    if secret.is_empty() || secret.len() > MAX_LINE_BYTES || secret.chars().any(char::is_control) {
        return Err(config("secret input is empty or malformed"));
    }
    Ok(SecretString::new(secret))
}

fn checked_username(username: &str) -> Result<String, Error> {
    if username.is_empty()
        || username.len() > MAX_LINE_BYTES
        || username.chars().any(char::is_control)
    {
        return Err(config("username is empty or malformed"));
    }
    Ok(username.to_owned())
}

fn write_prompt(prompt: &str) -> Result<(), Error> {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    stderr
        .write_all(prompt.as_bytes())
        .and_then(|()| stderr.flush())
        .map_err(|_| Error::new(ErrorKind::General, "could not write input prompt"))
}

fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

fn config(message: &'static str) -> Error {
    Error::new(ErrorKind::Config, message)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn bounded_line_reader_strips_crlf_and_leaves_following_otp_unread() {
        let mut input = Cursor::new(b"password\r\n123456\n".to_vec());
        assert_eq!(read_line(&mut input).unwrap(), "password");
        assert_eq!(read_line(&mut input).unwrap(), "123456");
    }

    #[test]
    fn bounded_line_reader_rejects_oversized_and_empty_secrets() {
        let mut oversized = Cursor::new(vec![b'x'; MAX_LINE_BYTES + 1]);
        assert_eq!(
            read_line(&mut oversized).unwrap_err().kind,
            ErrorKind::Config
        );
        let mut empty = Cursor::new(b"\n".to_vec());
        assert_eq!(
            checked_secret(read_line(&mut empty).unwrap())
                .unwrap_err()
                .kind,
            ErrorKind::Config
        );
    }

    #[test]
    fn explicit_site_must_match_and_returns_canonical_lowercase_id() {
        let sites = [Site {
            id: "A1B2C3D4-0000-0000-0000-000000000000".to_owned(),
            name: Some("main".to_owned()),
            status: None,
            health: None,
        }];
        assert_eq!(
            choose_site(&sites, Some("a1b2c3d4-0000-0000-0000-000000000000")).unwrap(),
            "a1b2c3d4-0000-0000-0000-000000000000"
        );
        assert_eq!(
            choose_site(&sites, Some("11111111-2222-3333-4444-555555555555"))
                .unwrap_err()
                .kind,
            ErrorKind::NotFound
        );
    }
}
