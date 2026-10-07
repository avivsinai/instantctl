use crate::{context::CommandContext, credentials::CredentialSource};
use clap::Args;
use instantctl_api::{
    Error, ErrorKind,
    client::access::radius::{self, ServerPatch},
    secret::SecretString,
};
use std::io::{self, IsTerminal, Read};

pub(super) async fn context(context: &CommandContext) -> Result<CommandContext, Error> {
    super::site::read::check_site(context)?;
    if context.site.is_none() && context.token_source.uses_profile() {
        context.resolve_profile_default().await
    } else {
        Ok(context.clone())
    }
}
pub(super) fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

#[derive(Clone, Debug, Default, Args)]
pub struct ServersArgs {
    #[arg(long)]
    pub primary_host: Option<String>,
    #[arg(long)]
    pub primary_timeout: Option<u8>,
    #[arg(long)]
    pub primary_retries: Option<u8>,
    #[arg(long)]
    pub primary_auth_port: Option<u16>,
    #[arg(long)]
    pub primary_accounting_port: Option<u16>,
    /// Read the primary shared secret from one bounded line on stdin.
    #[arg(long, conflicts_with = "prompt_primary_secret")]
    pub primary_secret_stdin: bool,
    /// Replace the primary secret using a hidden terminal prompt.
    #[arg(long)]
    pub prompt_primary_secret: bool,
    #[arg(long)]
    pub secondary_host: Option<String>,
    #[arg(long)]
    pub secondary_timeout: Option<u8>,
    #[arg(long)]
    pub secondary_retries: Option<u8>,
    #[arg(long)]
    pub secondary_auth_port: Option<u16>,
    #[arg(long)]
    pub secondary_accounting_port: Option<u16>,
    /// Read the secondary shared secret from the next bounded line on stdin.
    #[arg(long, conflicts_with = "prompt_secondary_secret")]
    pub secondary_secret_stdin: bool,
    /// Replace the secondary secret using a hidden terminal prompt.
    #[arg(long)]
    pub prompt_secondary_secret: bool,
}
impl ServersArgs {
    pub fn metadata(&self) -> (ServerPatch, ServerPatch) {
        (
            ServerPatch {
                host: self.primary_host.clone(),
                timeout: self.primary_timeout,
                retries: self.primary_retries,
                auth_port: self.primary_auth_port,
                accounting_port: self.primary_accounting_port,
                ..ServerPatch::default()
            },
            ServerPatch {
                host: self.secondary_host.clone(),
                timeout: self.secondary_timeout,
                retries: self.secondary_retries,
                auth_port: self.secondary_auth_port,
                accounting_port: self.secondary_accounting_port,
                ..ServerPatch::default()
            },
        )
    }
    pub fn has_secret(&self) -> bool {
        self.primary_secret_stdin
            || self.prompt_primary_secret
            || self.secondary_secret_stdin
            || self.prompt_secondary_secret
    }
    pub fn collect(
        &self,
        source: CredentialSource,
        create: bool,
    ) -> Result<(ServerPatch, ServerPatch), Error> {
        let (mut primary, mut secondary) = self.metadata();
        primary.validate()?;
        secondary.validate()?;
        let primary_needed = create || self.primary_secret_stdin || self.prompt_primary_secret;
        let secondary_needed = create && self.secondary_host.is_some()
            || self.secondary_secret_stdin
            || self.prompt_secondary_secret;
        for (needed, stdin) in [
            (primary_needed, self.primary_secret_stdin),
            (secondary_needed, self.secondary_secret_stdin),
        ] {
            if needed {
                validate_mode(stdin, io::stdin().is_terminal(), source)?;
            }
        }
        if primary_needed {
            primary.secret = Some(collect(
                self.primary_secret_stdin,
                "Primary RADIUS shared secret: ",
            )?);
        }
        if secondary_needed {
            secondary.secret = Some(collect(
                self.secondary_secret_stdin,
                "Secondary RADIUS shared secret: ",
            )?);
        }
        Ok((primary, secondary))
    }
}
fn validate_mode(stdin: bool, terminal: bool, source: CredentialSource) -> Result<(), Error> {
    if matches!(source, CredentialSource::Stdin) {
        return Err(usage(
            "RADIUS secret input cannot share stdin with --token-stdin",
        ));
    }
    if stdin && terminal {
        return Err(usage(
            "omit the secret-stdin flag at a terminal to use a hidden prompt",
        ));
    }
    if !stdin && !terminal {
        return Err(usage(
            "RADIUS secret input requires a terminal or an explicit secret-stdin flag",
        ));
    }
    Ok(())
}
fn collect(stdin: bool, prompt: &str) -> Result<SecretString, Error> {
    let value = if stdin {
        read_line(&mut io::stdin().lock())?
    } else {
        rpassword::prompt_password(prompt)
            .map_err(|_| Error::new(ErrorKind::General, "could not read RADIUS secret input"))?
    };
    radius::validate_secret(&value)?;
    Ok(SecretString::new(value))
}
fn read_line(reader: &mut impl Read) -> Result<String, Error> {
    const MAX: usize = 256;
    let mut bytes = Vec::with_capacity(MAX + 1);
    let mut byte = [0];
    let mut lf = false;
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => {
                lf = true;
                break;
            }
            Ok(_) if bytes.len() < MAX + 1 => bytes.push(byte[0]),
            Ok(_) => {
                return Err(usage(
                    "RADIUS secret input exceeds the 256-byte input limit",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(Error::new(
                    ErrorKind::General,
                    "could not read RADIUS secret input",
                ));
            }
        }
    }
    if lf && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX {
        return Err(usage(
            "RADIUS secret input exceeds the 256-byte input limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| usage("RADIUS secret input is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn secret_reader_is_bounded_and_leaves_secondary_line_unread() {
        let mut input = Cursor::new(b"primary-value\r\nsecondary-value\n".to_vec());
        assert_eq!(read_line(&mut input).unwrap(), "primary-value");
        assert_eq!(read_line(&mut input).unwrap(), "secondary-value");
        assert_eq!(
            read_line(&mut Cursor::new(
                format!("{}\r\n", "x".repeat(64)).into_bytes()
            ))
            .unwrap()
            .len(),
            64
        );
        let unicode = "א".repeat(64);
        assert_eq!(
            read_line(&mut Cursor::new(format!("{unicode}\n").into_bytes())).unwrap(),
            unicode
        );
        assert!(radius::validate_secret(&unicode).is_ok());
        assert!(radius::validate_secret(&"א".repeat(65)).is_err());
        for bytes in [vec![b'x'; 65536], vec![b'x'; 257], b"utf8-\xff".to_vec()] {
            let mut input = Cursor::new(bytes.clone());
            let error = read_line(&mut input).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(input.position() <= 258);
            assert!(
                !format!("{error:?} {error}")
                    .contains(&String::from_utf8_lossy(&bytes).to_string())
            );
        }
    }
    #[test]
    fn secret_modes_refuse_non_tty_and_shared_token_stdin() {
        assert!(validate_mode(false, true, CredentialSource::Environment).is_ok());
        assert!(validate_mode(true, false, CredentialSource::Environment).is_ok());
        for (stdin, tty, source) in [
            (false, false, CredentialSource::Environment),
            (true, true, CredentialSource::Environment),
            (true, false, CredentialSource::Stdin),
            (false, true, CredentialSource::Stdin),
        ] {
            assert_eq!(
                validate_mode(stdin, tty, source).unwrap_err().kind,
                ErrorKind::Usage
            );
        }
    }
    #[test]
    fn radius_secret_arguments_are_flags_only_and_cannot_accept_values() {
        use clap::{ArgAction, CommandFactory};
        let cli = crate::cli::Cli::command();
        for (noun, verb) in [
            ("radius", "create"),
            ("radius", "update"),
            ("port-access-control", "update"),
        ] {
            let command = cli
                .find_subcommand(noun)
                .unwrap()
                .find_subcommand(verb)
                .unwrap();
            let secrets = command
                .get_arguments()
                .filter(|arg| arg.get_id().as_str().contains("secret"))
                .collect::<Vec<_>>();
            assert_eq!(secrets.len(), 4);
            for arg in secrets {
                assert!(matches!(arg.get_action(), ArgAction::SetTrue));
                assert!(!arg.is_allow_hyphen_values_set());
            }
        }
    }
}
