use super::*;
use crate::{
    credentials::{CommandTokenSource, CredentialSource},
    output::Format,
};
use instantctl_api::StaticToken;
use std::time::Duration;

fn context(source: CredentialSource) -> CommandContext {
    CommandContext {
        profile: "profile-command-test".into(),
        site: None,
        timeout: Duration::from_secs(2),
        format: Format::Json,
        token_source: source,
        prepared_token: Some(CommandTokenSource::Static(
            StaticToken::new("profile-command-test-token").unwrap(),
        )),
        protected_ports: Vec::new(),
    }
}

#[test]
fn show_projection_contains_profile_metadata_only() {
    let data = show_data(
        "lab",
        ProfileMetadata {
            default_site: Some("123e4567-e89b-12d3-a456-426614174000".into()),
            protected_ports: vec!["aa:bb:cc:dd:ee:ff:7".into(), "Closet:Switch:3".into()],
        },
    );
    assert_eq!(
        data,
        json!({
            "profile": "lab",
            "default_site": "123e4567-e89b-12d3-a456-426614174000",
            "protected_ports": ["aa:bb:cc:dd:ee:ff:7", "Closet:Switch:3"],
        })
    );
    assert!(data.get("access_token").is_none());
    assert!(data.get("refresh_token").is_none());
}

#[test]
fn show_projection_preserves_missing_metadata_as_null_and_empty_list() {
    let data = show_data("default", ProfileMetadata::default());
    assert_eq!(
        data,
        json!({
            "profile": "default",
            "default_site": null,
            "protected_ports": [],
        })
    );
}

#[test]
fn cli_parses_profile_command_shapes() {
    use clap::Parser;

    for argv in [
        vec!["instantctl", "profile", "show"],
        vec![
            "instantctl",
            "profile",
            "default-site",
            "set",
            "123e4567-e89b-12d3-a456-426614174000",
        ],
        vec!["instantctl", "profile", "protected-ports", "list"],
        vec![
            "instantctl",
            "profile",
            "protected-ports",
            "add",
            "Closet:Switch:7",
        ],
        vec![
            "instantctl",
            "profile",
            "protected-ports",
            "remove",
            "aa:bb:cc:dd:ee:ff:7",
        ],
    ] {
        crate::cli::Cli::try_parse_from(argv).expect("profile command should parse");
    }
}

#[test]
fn protected_port_list_projection_has_only_plain_identity_fields() {
    let rows = protected_port_list_data(vec![
        instantctl_api::client::profile::ProtectedPortSummary {
            entry: "aa:bb:cc:dd:ee:ff:7".into(),
            switch_mac: Some("aa:bb:cc:dd:ee:ff".into()),
            switch_name: Some("Closet".into()),
            faceplate: 7,
        },
    ]);
    assert_eq!(
        rows,
        json!([{
            "entry": "aa:bb:cc:dd:ee:ff:7",
            "switch_mac": "aa:bb:cc:dd:ee:ff",
            "switch_name": "Closet",
            "faceplate": 7,
        }])
    );
}

#[test]
fn preflight_reports_bad_input_before_static_credential_refusal() {
    let context = context(CredentialSource::Stdin);
    let invalid_site = Args {
        command: Command::DefaultSite {
            command: DefaultSiteCommand::Set {
                site_id: "not-a-uuid".into(),
            },
        },
    };
    let error = invalid_site.preflight(&context).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(error.message.contains("UUID"));

    let invalid_entry = Args {
        command: Command::ProtectedPorts {
            command: ProtectedPortsCommand::Add {
                entry: "aa:bb:cc:dd:ee:gg:7".into(),
            },
        },
    };
    let error = invalid_entry.preflight(&context).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Usage);
    assert!(!error.message.contains("saved-profile"));
}

#[test]
fn valid_profile_commands_refuse_static_sources_as_usage() {
    let context = context(CredentialSource::Stdin);
    for args in [
        Args {
            command: Command::Show,
        },
        Args {
            command: Command::DefaultSite {
                command: DefaultSiteCommand::Set {
                    site_id: "123e4567-e89b-12d3-a456-426614174000".into(),
                },
            },
        },
        Args {
            command: Command::ProtectedPorts {
                command: ProtectedPortsCommand::List,
            },
        },
        Args {
            command: Command::ProtectedPorts {
                command: ProtectedPortsCommand::Add {
                    entry: "Closet:Switch:7".into(),
                },
            },
        },
        Args {
            command: Command::ProtectedPorts {
                command: ProtectedPortsCommand::Remove {
                    entry: "aa:bb:cc:dd:ee:ff:7".into(),
                },
            },
        },
    ] {
        let error = args.preflight(&context).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Usage);
        assert!(error.message.contains("saved-profile"));
        assert_eq!(crate::exit::ExitStatus::Error(error.kind).code(), 2);
    }
}

#[test]
fn profile_input_validators_accept_uuid_and_exact_name_forms() {
    validate_default_site("123e4567-e89b-12d3-a456-426614174000").unwrap();
    for entry in ["aa:bb:cc:dd:ee:ff:7", "Closet:Switch:7", "機器室🚪:2"] {
        validate_protected_port_entry(entry).unwrap();
    }
}
