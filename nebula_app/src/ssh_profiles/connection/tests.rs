use super::{SshConnectionOptions, SshHostJumpMode, SshHostProxyMode, validate_ssh_destination};
use crate::ssh_profiles::{SshAuthMode, SshProfiles};

fn proxy() -> SshConnectionOptions {
    SshConnectionOptions {
        proxy_mode: SshHostProxyMode::Socks5,
        proxy_host: "proxy.internal".to_owned(),
        proxy_username: "alice".to_owned(),
        ..Default::default()
    }
}

#[test]
fn old_profiles_inherit_both_connection_settings_without_serialized_noise() {
    let profiles: SshProfiles = serde_json::from_str(
        r#"{"version":1,"profiles":[{"destination":"root@host","auth":"password"}]}"#,
    )
    .unwrap();
    assert!(profiles.for_destination("root@host").connection.is_default());
    let serialized = serde_json::to_string(&profiles).unwrap();
    assert!(!serialized.contains("connection"));
    assert!(!serialized.contains("proxy_password"));
}

#[test]
fn explicit_direct_and_disabled_jump_survive_profile_round_trip() {
    let mut profiles = SshProfiles::default();
    let mut profile = profiles.for_destination("root@host");
    profile.connection.proxy_mode = SshHostProxyMode::Direct;
    profile.connection.jump_mode = SshHostJumpMode::None;
    profiles.upsert(profile.clone());
    let serialized = serde_json::to_string(&profiles).unwrap();
    let restored: SshProfiles = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored.for_destination("root@host"), profile);
}

#[test]
fn proxy_defaults_are_protocol_specific() {
    let mut options = proxy();
    assert_eq!(options.effective_proxy_port(), 1080);
    options.proxy_mode = SshHostProxyMode::Http;
    assert_eq!(options.effective_proxy_port(), 8080);
    options.proxy_port = Some(3128);
    assert_eq!(options.effective_proxy_port(), 3128);
    options.proxy_port = Some(0);
    assert!(options.validate("root@host").is_err());
}

#[test]
fn invalid_proxy_endpoints_and_authentication_are_rejected() {
    for host in ["", "http://proxy", "user:password@proxy", "proxy:8080", "proxy\r\nHeader"] {
        let options = SshConnectionOptions { proxy_host: host.to_owned(), ..proxy() };
        assert!(options.validate("root@host").is_err(), "{host:?}");
    }
    let mut options = proxy();
    options.proxy_username = "字".repeat(86);
    assert!(options.validate("root@host").is_err());
    options.proxy_mode = SshHostProxyMode::Http;
    options.proxy_username = "user:password".to_owned();
    assert!(options.validate("root@host").is_err());
}

#[test]
fn proxy_credential_keys_are_stable_and_scoped_to_endpoint_and_target() {
    let options = proxy();
    let key = options.proxy_credential_target("root@host").unwrap();
    assert!(key.starts_with("Nebula/SSH/Proxy/"));
    assert!(!key.contains("alice"));
    assert!(!key.contains("proxy.internal"));
    assert_eq!(key, options.proxy_credential_target("root@host").unwrap());
    assert_ne!(key, options.proxy_credential_target("root@other").unwrap());
    for changed in [
        SshConnectionOptions { proxy_host: "other-proxy".into(), ..options.clone() },
        SshConnectionOptions { proxy_port: Some(9000), ..options.clone() },
        SshConnectionOptions { proxy_username: "bob".into(), ..options.clone() },
        SshConnectionOptions { proxy_mode: SshHostProxyMode::Http, ..options.clone() },
    ] {
        assert_ne!(key, changed.proxy_credential_target("root@host").unwrap());
    }
    let disabled = SshConnectionOptions { proxy_mode: SshHostProxyMode::Direct, ..options };
    assert!(disabled.proxy_credential_target("root@host").is_none());
}

#[test]
fn ssh_specs_allow_aliases_and_ipv6_but_not_lists_commands_or_passwords() {
    for spec in ["office", "dev@office:2200", "ssh://dev@[::1]:2222", "dev@2001:db8::1"] {
        assert!(validate_ssh_destination(spec).is_ok(), "{spec}");
    }
    for spec in [
        "",
        "host1,host2",
        "-oProxyCommand=exec",
        "user:secret@host",
        "bad host",
        "host:0",
        "host:65536",
        "[::1]junk",
        "host;echo",
        "user@@host",
        "https://host",
    ] {
        assert!(validate_ssh_destination(spec).is_err(), "{spec}");
    }
}

#[test]
fn self_jump_is_rejected_before_resolution() {
    let options = SshConnectionOptions {
        jump_mode: SshHostJumpMode::Host,
        jump_host: "ssh://root@HOST:22".to_owned(),
        ..Default::default()
    };
    assert!(options.validate("root@host").unwrap_err().contains("自身"));
}

#[test]
fn unknown_connection_modes_do_not_fall_back_to_direct_or_inherit() {
    assert!(serde_json::from_str::<SshConnectionOptions>(r#"{"proxy_mode":"socks4"}"#).is_err());
    assert!(serde_json::from_str::<SshConnectionOptions>(r#"{"jump_mode":"missing"}"#).is_err());
}

#[test]
fn renaming_saved_jump_host_updates_all_enabled_exact_references() {
    let mut profiles = SshProfiles::default();
    let mut bastion = profiles.for_destination("jump@old-bastion");
    bastion.auth = SshAuthMode::PublicKey;
    bastion.private_keys = vec!["bastion-identity".into()];
    profiles.upsert(bastion);
    for (destination, jump_host) in
        [("root@first", "jump@old-bastion"), ("root@second", "  jump@old-bastion  ")]
    {
        let mut profile = profiles.for_destination(destination);
        profile.connection.jump_mode = SshHostJumpMode::Host;
        profile.connection.jump_host = jump_host.to_owned();
        profiles.upsert(profile);
    }

    profiles.rename("jump@old-bastion", "jump@new-bastion");

    for destination in ["root@first", "root@second"] {
        assert_eq!(profiles.for_destination(destination).connection.jump_host, "jump@new-bastion",);
    }
    assert_eq!(profiles.for_destination("jump@new-bastion").auth, SshAuthMode::PublicKey);
    assert_eq!(
        profiles.for_destination("jump@new-bastion").private_keys,
        vec![std::path::PathBuf::from("bastion-identity")],
    );
}

#[test]
fn renaming_jump_host_keeps_different_aliases_and_inactive_drafts_unchanged() {
    let mut profiles = SshProfiles::default();
    let mut unchanged = Vec::new();
    for (destination, mode, jump_host) in [
        ("root@alias", SshHostJumpMode::Host, "bastion-alias"),
        ("root@uri", SshHostJumpMode::Host, "ssh://jump@old-bastion"),
        ("root@case", SshHostJumpMode::Host, "jump@OLD-bastion"),
        ("root@disabled", SshHostJumpMode::None, "jump@old-bastion"),
        ("root@inherited", SshHostJumpMode::Inherit, "jump@old-bastion"),
    ] {
        let mut profile = profiles.for_destination(destination);
        profile.connection.jump_mode = mode;
        profile.connection.jump_host = jump_host.to_owned();
        unchanged.push(profile.clone());
        profiles.upsert(profile);
    }

    profiles.rename("jump@old-bastion", "jump@new-bastion");

    for profile in unchanged {
        assert_eq!(profiles.for_destination(&profile.destination), profile);
    }
}

#[test]
fn renaming_imported_jump_without_profile_still_updates_exact_references() {
    let mut profiles = SshProfiles::default();
    let mut target = profiles.for_destination("root@target");
    target.connection.jump_mode = SshHostJumpMode::Host;
    target.connection.jump_host = "imported-bastion".to_owned();
    profiles.upsert(target);

    profiles.rename("imported-bastion", "renamed-bastion");

    assert_eq!(profiles.for_destination("root@target").connection.jump_host, "renamed-bastion",);
    let serialized = serde_json::to_value(&profiles).unwrap();
    assert_eq!(serialized["profiles"].as_array().unwrap().len(), 1);
}

#[test]
fn jump_dependents_preserves_order_and_excludes_self_aliases_and_inactive_drafts() {
    let mut profiles = SshProfiles::default();
    for (destination, mode, jump_host) in [
        ("root@second", SshHostJumpMode::Host, "bastion"),
        ("bastion", SshHostJumpMode::Host, "bastion"),
        ("root@alias", SshHostJumpMode::Host, "another-alias"),
        ("root@first", SshHostJumpMode::Host, "  bastion  "),
        ("root@disabled", SshHostJumpMode::None, "bastion"),
        ("root@inherited", SshHostJumpMode::Inherit, "bastion"),
    ] {
        let mut profile = profiles.for_destination(destination);
        profile.connection.jump_mode = mode;
        profile.connection.jump_host = jump_host.to_owned();
        profiles.upsert(profile);
    }

    assert_eq!(profiles.jump_dependents("bastion"), vec!["root@second", "root@first"]);
    assert!(profiles.jump_dependents("missing").is_empty());
}
