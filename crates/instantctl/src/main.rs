use std::{
    any::TypeId,
    error::Error as _,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    process::ExitCode,
    time::Duration,
};

use clap::{
    Arg, Command, CommandFactory, Parser, ValueEnum,
    error::{ContextKind, ContextValue, ErrorKind as ClapErrorKind},
};
use instantctl::{
    cli::Cli,
    commands,
    context::CommandContext,
    credentials::CredentialSource,
    exit::ExitStatus,
    output::{self, Format},
};
use instantctl_api::client::radio::{Band, BandMapping, Power, Width};
use instantctl_api::{Error, ErrorKind, validate_site_id};
use serde::Serialize;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let terminal = io::stdout().is_terminal();
    let error_format = requested_format(&args, terminal);
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion
            ) =>
        {
            return match output::write_bytes(&mut io::stdout().lock(), error.to_string().as_bytes())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => report(error.into(), error_format),
            };
        }
        Err(error) => {
            let error = KindFormatter::format(&error);
            write_error(error_format, &error, &error.message);
            return ExitCode::from(ExitStatus::Error(error.kind).code());
        }
    };
    let format = Format::resolve(cli.format, terminal);
    if let Some(site) = cli.site.as_deref()
        && let Err(error) = validate_site_id(site)
    {
        return report(error.into(), format);
    }
    let context = CommandContext {
        profile: cli.profile.unwrap_or_else(|| "default".into()),
        site: cli.site.map(|site| site.to_ascii_lowercase()),
        timeout: Duration::from_secs(cli.timeout),
        format,
        token_source: if cli.token_stdin {
            CredentialSource::Stdin
        } else {
            CredentialSource::Environment
        },
        prepared_token: None,
        protected_ports: Vec::new(),
    };
    let result = {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        commands::run(cli.command, &context, &mut out).await
    };
    match result {
        Ok(status) => ExitCode::from(status.code()),
        Err(error) => report(error, format),
    }
}

#[derive(Serialize)]
struct UsageError {
    kind: ErrorKind,
    message: String,
    fields: UsageFields,
}

#[derive(Default, Serialize)]
struct UsageFields {
    argument: Option<String>,
    value: Option<String>,
    allowed_values: Vec<String>,
    expected: Option<String>,
}

struct KindFormatter;

impl KindFormatter {
    fn format(error: &clap::Error) -> UsageError {
        let mut command = Cli::command();
        command.build();
        Self::with_command(error, &command)
    }

    fn with_command(error: &clap::Error, command: &Command) -> UsageError {
        // Unrecognized tokens can be a misplaced password or an obsolete secret
        // positional argument. Only a declared argument can supply value detail;
        // never render clap's usage, suggestions, or the original argv.
        let mut fields = UsageFields::default();
        let mut message = error
            .kind()
            .as_str()
            .unwrap_or("invalid arguments")
            .to_owned();
        if let Some(ContextValue::String(label)) = error.get(ContextKind::InvalidArg) {
            let arguments = declared_arguments(command, label);
            if let Some(argument) = arguments.first() {
                let secret = arguments.iter().any(|argument| secret_argument(argument));
                fields.argument = Some(label.clone());
                fields.value = match error.get(ContextKind::InvalidValue) {
                    Some(ContextValue::String(value)) => Some(if secret {
                        "[REDACTED]".to_owned()
                    } else {
                        value.clone()
                    }),
                    _ => None,
                };
                if fields.value.is_some() || error.get(ContextKind::ValidValue).is_some() {
                    (fields.allowed_values, fields.expected) = expectation(argument, error, secret);
                }
                message = if let Some(value) = &fields.value {
                    format!("invalid value {value:?} for {label:?}")
                } else {
                    format!("{message}: {label}")
                };
                if let Some(expected) = &fields.expected {
                    message.push_str("; expected: ");
                    message.push_str(expected);
                }
            }
        }
        UsageError {
            kind: ErrorKind::Usage,
            message,
            fields,
        }
    }
}

fn declared_arguments<'a>(command: &'a Command, label: &str) -> Vec<&'a Arg> {
    command
        .get_arguments()
        .filter(|argument| argument.to_string() == label)
        .chain(
            command
                .get_subcommands()
                .flat_map(|command| declared_arguments(command, label)),
        )
        .collect()
}

fn secret_argument(argument: &Arg) -> bool {
    argument.get_id().as_str().split(['-', '_']).any(|part| {
        matches!(
            part,
            "secret"
                | "password"
                | "passphrase"
                | "psk"
                | "token"
                | "otp"
                | "field"
                | "fields"
                | "header"
                | "headers"
        )
    })
}

fn expectation(argument: &Arg, error: &clap::Error, secret: bool) -> (Vec<String>, Option<String>) {
    if argument
        .get_num_args()
        .is_some_and(|range| range.max_values() == 0)
    {
        return (Vec::new(), Some("a flag without a value".to_owned()));
    }
    if secret {
        // Parser diagnostics and possible values can themselves contain secrets.
        return (Vec::new(), Some("a secret value (redacted)".to_owned()));
    }
    if error.kind() == ClapErrorKind::InvalidValue
        && matches!(error.get(ContextKind::InvalidValue), Some(ContextValue::String(value)) if value.is_empty())
        && matches!(error.get(ContextKind::ValidValue), Some(ContextValue::Strings(values)) if values.is_empty())
    {
        // Clap represents a missing value this way, without a command path.
        // Shared labels such as --start can have different types across commands.
        return (Vec::new(), Some("a value is required".to_owned()));
    }
    let mut values = match error.get(ContextKind::ValidValue) {
        Some(ContextValue::Strings(values)) => values.clone(),
        _ => argument
            .get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .filter(|value| !value.is_hide_set())
            .map(|value| value.get_name().to_owned())
            .collect(),
    };
    // These API enums use FromStr, so clap has no possible-values iterator.
    // Serialize their existing variants rather than inventing wire strings.
    if values.is_empty() {
        values = radio_values(argument);
    }
    if !values.is_empty() {
        let expected = format!("one of: {}", values.join(", "));
        return (values, Some(expected));
    }
    let type_id = argument.get_value_parser().type_id();
    let format = if type_id == TypeId::of::<Ipv4Addr>() {
        "IPv4 address (four decimal octets, each 0..255)"
    } else if type_id == TypeId::of::<Ipv6Addr>() {
        "IPv6 address"
    } else if type_id == TypeId::of::<IpAddr>() {
        "IPv4 or IPv6 address"
    } else if type_id == TypeId::of::<url::Url>() {
        "URL with a scheme"
    } else if [
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
    ]
    .iter()
    .any(|id| type_id == *id)
    {
        "unsigned integer"
    } else {
        "value accepted by this argument's parser"
    };
    let mut expected = format.to_owned();
    if let Some(help) = argument.get_help() {
        expected.push_str("; ");
        expected.push_str(&help.to_string());
    }
    // A parser source concerns this non-sensitive value only.
    if let Some(source) = error.source() {
        expected.push_str("; ");
        expected.push_str(&source.to_string());
    }
    (Vec::new(), Some(expected))
}

fn radio_values(argument: &Arg) -> Vec<String> {
    let type_id = argument.get_value_parser().type_id();
    let values: Vec<&str> = if type_id == TypeId::of::<Band>() {
        [Band::Ghz24, Band::Ghz5, Band::Ghz6]
            .map(Band::api_id)
            .to_vec()
    } else if type_id == TypeId::of::<Width>() {
        [
            Width::Mhz20,
            Width::Mhz40,
            Width::Mhz80,
            Width::Mhz160,
            Width::Mhz320,
        ]
        .map(Width::api_id)
        .to_vec()
    } else if type_id == TypeId::of::<Power>() {
        [
            Power::Dbm6,
            Power::Dbm9,
            Power::Dbm12,
            Power::Dbm15,
            Power::Dbm18,
            Power::Dbm21,
            Power::Dbm24,
            Power::Dbm27,
            Power::Dbm30,
            Power::Dbm33,
            Power::RegulatoryMax,
        ]
        .map(Power::api_id)
        .to_vec()
    } else if type_id == TypeId::of::<BandMapping>() {
        [
            BandMapping::Ghz24And5,
            BandMapping::Ghz24And6,
            BandMapping::Ghz5And6,
        ]
        .map(BandMapping::api_id)
        .to_vec()
    } else {
        Vec::new()
    };
    values.into_iter().map(str::to_owned).collect()
}

fn requested_format(args: &[OsString], terminal: bool) -> Format {
    let mut requested = None;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let Some(arg) = arg.to_str() else { continue };
        let value = if arg == "--format" {
            args.next().and_then(|value| value.to_str())
        } else {
            arg.strip_prefix("--format=")
        };
        if let Some(value) = value {
            requested = Format::from_str(value, false).ok();
        }
    }
    Format::resolve(requested, terminal)
}

fn report(error: anyhow::Error, format: Format) -> ExitCode {
    if error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    {
        return ExitCode::from(ExitStatus::Error(ErrorKind::BrokenPipe).code());
    }
    let fallback;
    let error = match error.downcast_ref::<Error>() {
        Some(error) => error,
        None => {
            fallback = Error::new(ErrorKind::General, error.to_string());
            &fallback
        }
    };
    write_error(format, error, &error.message);
    ExitCode::from(ExitStatus::Error(error.kind).code())
}

fn write_error(format: Format, error: &impl Serialize, message: &str) {
    let mut stderr = io::stderr().lock();
    // Error reporting must not introduce a second failure or panic.
    let _ = match format {
        Format::Json | Format::Yaml => output::write_data(&mut stderr, format, error),
        Format::Table => writeln!(stderr, "error: {message}").map_err(Into::into),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_values_do_not_borrow_another_commands_parser_type() {
        for args in [
            vec!["instantctl", "wlan", "schedule", "example", "--start"],
            vec!["instantctl", "wlan", "schedule", "example", "--end"],
            vec!["instantctl", "schedule", "create", "example", "--start"],
            vec!["instantctl", "schedule", "create", "example", "--end"],
        ] {
            let error = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(error.kind(), ClapErrorKind::InvalidValue);
            let error = KindFormatter::format(&error);
            assert_eq!(error.fields.value.as_deref(), Some(""));
            assert_eq!(
                error.fields.expected.as_deref(),
                Some("a value is required")
            );
            assert!(error.fields.allowed_values.is_empty());
            assert!(!error.message.contains("IPv4"));
        }
    }

    #[test]
    fn duplicate_arguments_do_not_borrow_another_commands_value_choices() {
        let error = Cli::try_parse_from([
            "instantctl",
            "wlan",
            "bandwidth",
            "example",
            "--mode",
            "off",
            "--mode",
            "per-client",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), ClapErrorKind::ArgumentConflict);
        let error = KindFormatter::format(&error);
        assert_eq!(error.fields.argument.as_deref(), Some("--mode <MODE>"));
        assert_eq!(error.fields.value, None);
        assert!(error.fields.allowed_values.is_empty());
        assert_eq!(error.fields.expected, None);
        assert!(!error.message.contains("infrastructure"));
    }

    #[test]
    fn secret_parser_reasons_are_redacted() {
        let mut command =
            Command::new("test").arg(Arg::new("password").long("password").value_parser(
                |value: &str| -> Result<String, String> { Err(format!("refused secret {value}")) },
            ));
        command.build();
        let error = command
            .clone()
            .try_get_matches_from(["test", "--password", "parser-secret"])
            .unwrap_err();
        let error = KindFormatter::with_command(&error, &command);
        assert_eq!(error.fields.value.as_deref(), Some("[REDACTED]"));
        assert!(error.fields.allowed_values.is_empty());
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("parser-secret"));
        assert!(!serialized.contains("refused secret"));
    }

    #[test]
    fn secret_possible_values_are_redacted() {
        let mut command = Command::new("test").arg(
            Arg::new("primary_secret")
                .long("primary-secret")
                .value_parser(["allowed-secret"]),
        );
        command.build();
        let error = command
            .clone()
            .try_get_matches_from(["test", "--primary-secret", "rejected-secret"])
            .unwrap_err();
        let error = KindFormatter::with_command(&error, &command);
        assert_eq!(error.fields.value.as_deref(), Some("[REDACTED]"));
        assert!(error.fields.allowed_values.is_empty());
        let serialized = serde_json::to_string(&error).unwrap();
        for secret in ["allowed-secret", "rejected-secret"] {
            assert!(!serialized.contains(secret));
        }
    }
}
