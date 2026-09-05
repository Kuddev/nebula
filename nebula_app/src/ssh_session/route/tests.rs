use std::collections::HashMap;

use super::{ResolvedRoute, RouteTransport, resolve_route_with};
use crate::ssh_profiles::{
    SshAuthMode, SshHostJumpMode, SshHostProxyMode, SshProfileAuth, SshProfiles,
};
use crate::ssh_proxy::{ProxyMode, ProxyScheme, SshProxyConfig};
use crate::ssh_session::{SshDestination, SshTestRequest};

mod loopback;

fn host(spec: &str) -> (SshDestination, SshProfileAuth) {
    (SshDestination::parse(spec).unwrap(), SshProfiles::default().for_destination(spec))
}

fn global_proxy() -> SshProxyConfig {
    SshProxyConfig {
        mode: ProxyMode::Custom,
        url: "socks5://global-proxy:1080".to_owned(),
        no_proxy: Vec::new(),
    }
}

fn resolve(
    destination: SshDestination,
    profile: SshProfileAuth,
    global: &SshProxyConfig,
    hosts: &HashMap<String, (SshDestination, SshProfileAuth)>,
) -> Result<ResolvedRoute, String> {
    resolve_route_with(
        destination,
        profile,
        global,
        0,
        None,
        &mut |spec| hosts.get(spec).cloned().ok_or_else(|| "missing host".to_owned()),
        &mut |_| Ok(None),
    )
}

fn custom_proxy(profile: &mut SshProfileAuth, mode: SshHostProxyMode, host: &str) {
    profile.connection.proxy_mode = mode;
    profile.connection.proxy_host = host.to_owned();
}

fn jump(profile: &mut SshProfileAuth, target: &str) {
    profile.connection.jump_mode = SshHostJumpMode::Host;
    profile.connection.jump_host = target.to_owned();
}

#[test]
fn defaults_follow_global_proxy_and_openssh_jump() {
    let (mut destination, profile) = host("root@target");
    let global = global_proxy();
    let route = resolve(destination.clone(), profile.clone(), &global, &HashMap::new()).unwrap();
    assert!(matches!(route.transport, RouteTransport::Server(_)));
    destination.proxy_jump = Some("jump-alias".to_owned());
    let hosts = HashMap::from([("jump-alias".to_owned(), host("bastion@first-hop"))]);
    let route = resolve(destination, profile, &global, &hosts).unwrap();
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    assert_eq!(jump.destination.user, "bastion");
    let RouteTransport::Server(server) = jump.transport else { panic!("expected proxy") };
    assert_eq!(server.host, "global-proxy");
}

#[test]
fn explicit_network_proxy_overrides_jump_proxy_only_at_first_network_hop() {
    let (destination, mut profile) = host("root@target");
    custom_proxy(&mut profile, SshHostProxyMode::Http, "target-proxy");
    jump(&mut profile, "bastion@hop");
    let (jump_destination, mut jump_profile) = host("bastion@hop");
    custom_proxy(&mut jump_profile, SshHostProxyMode::Socks5, "jump-proxy");
    jump_profile.auth = SshAuthMode::PublicKey;
    jump_profile.private_keys = vec!["bastion-key".into()];
    let hosts = HashMap::from([("bastion@hop".to_owned(), (jump_destination, jump_profile))]);
    let route = resolve(destination, profile, &global_proxy(), &hosts).unwrap();
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    assert_eq!(jump.profile.auth, SshAuthMode::PublicKey);
    assert_eq!(jump.profile.private_keys, vec![std::path::PathBuf::from("bastion-key")]);
    let RouteTransport::Server(server) = jump.transport else { panic!("expected proxy") };
    assert_eq!(server.scheme, ProxyScheme::HttpConnect);
    assert_eq!(server.host, "target-proxy");
}

#[test]
fn direct_disables_network_proxy_but_keeps_jump() {
    let (destination, mut profile) = host("root@target");
    profile.connection.proxy_mode = SshHostProxyMode::Direct;
    jump(&mut profile, "bastion@hop");
    let (jump_destination, mut jump_profile) = host("bastion@hop");
    custom_proxy(&mut jump_profile, SshHostProxyMode::Http, "ignored-proxy");
    let hosts = HashMap::from([("bastion@hop".to_owned(), (jump_destination, jump_profile))]);
    let route = resolve(destination, profile, &global_proxy(), &hosts).unwrap();
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    assert!(matches!(jump.transport, RouteTransport::Direct));
}

#[test]
fn inherited_network_uses_jump_profiles_own_proxy() {
    let (destination, mut profile) = host("root@target");
    jump(&mut profile, "bastion@hop");
    let (jump_destination, mut jump_profile) = host("bastion@hop");
    custom_proxy(&mut jump_profile, SshHostProxyMode::Http, "jump-proxy");
    let hosts = HashMap::from([("bastion@hop".to_owned(), (jump_destination, jump_profile))]);
    let route = resolve(destination, profile, &global_proxy(), &hosts).unwrap();
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    let RouteTransport::Server(server) = jump.transport else { panic!("expected proxy") };
    assert_eq!(server.host, "jump-proxy");
}

#[test]
fn disabled_jump_suppresses_ssh_config_without_disabling_network_proxy() {
    let (mut destination, mut profile) = host("root@target");
    destination.proxy_jump = Some("bad,chain".to_owned());
    profile.connection.jump_mode = SshHostJumpMode::None;
    let route = resolve(destination, profile, &global_proxy(), &HashMap::new()).unwrap();
    assert!(matches!(route.transport, RouteTransport::Server(_)));
}

#[test]
fn global_jump_is_applied_once_instead_of_recursing_into_itself() {
    let (destination, profile) = host("root@target");
    let global = SshProxyConfig {
        mode: ProxyMode::Custom,
        url: "jump:bastion@hop".into(),
        no_proxy: Vec::new(),
    };
    let hosts = HashMap::from([("bastion@hop".to_owned(), host("bastion@hop"))]);
    let route = resolve(destination, profile, &global, &hosts).unwrap();
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    assert!(matches!(jump.transport, RouteTransport::Direct));
}

#[test]
fn invalid_proxy_or_unresolved_jump_never_falls_back_to_direct() {
    let (destination, mut profile) = host("root@target");
    custom_proxy(&mut profile, SshHostProxyMode::Socks5, "http://invalid");
    assert!(
        resolve(destination.clone(), profile.clone(), &global_proxy(), &HashMap::new()).is_err()
    );
    profile.connection.proxy_mode = SshHostProxyMode::Direct;
    jump(&mut profile, "bastion@missing");
    assert!(resolve(destination, profile, &global_proxy(), &HashMap::new()).is_err());
}

#[test]
fn resolved_alias_cycles_are_rejected_before_opening_any_transport() {
    let (destination, mut profile) = host("root@target");
    jump(&mut profile, "target-alias");
    let hosts = HashMap::from([("target-alias".to_owned(), host("another-user@TARGET"))]);
    let error = resolve(destination, profile, &global_proxy(), &hosts).err().unwrap();
    assert!(error.contains("循环"));
}

#[test]
fn two_hops_are_supported_and_a_third_is_rejected() {
    let (destination, mut profile) = host("root@target");
    jump(&mut profile, "jump@near-target");
    let (first_destination, mut first_profile) = host("jump@near-target");
    jump(&mut first_profile, "jump@near-client");
    let (second_destination, second_profile) = host("jump@near-client");
    let mut hosts = HashMap::from([
        ("jump@near-target".to_owned(), (first_destination, first_profile)),
        ("jump@near-client".to_owned(), (second_destination, second_profile)),
    ]);
    assert!(resolve(destination.clone(), profile.clone(), &global_proxy(), &hosts).is_ok());
    jump(&mut hosts.get_mut("jump@near-client").unwrap().1, "jump@third");
    let error = resolve(destination, profile, &global_proxy(), &hosts).err().unwrap();
    assert!(error.contains("2 级"));
}

#[test]
fn pool_identity_changes_when_any_hop_network_path_or_auth_changes() {
    let (destination, mut profile) = host("root@target");
    jump(&mut profile, "bastion@hop");
    let mut hosts = HashMap::from([("bastion@hop".to_owned(), host("bastion@hop"))]);
    let initial = resolve(destination.clone(), profile.clone(), &global_proxy(), &hosts).unwrap();
    let initial_key = initial.pool_key();
    custom_proxy(
        &mut hosts.get_mut("bastion@hop").unwrap().1,
        SshHostProxyMode::Http,
        "other-proxy",
    );
    let changed = resolve(destination.clone(), profile.clone(), &global_proxy(), &hosts).unwrap();
    assert_ne!(initial_key, changed.pool_key());
    hosts.get_mut("bastion@hop").unwrap().1.auth = SshAuthMode::Password;
    let changed_auth = resolve(destination, profile, &global_proxy(), &hosts).unwrap();
    assert_ne!(changed.pool_key(), changed_auth.pool_key());
}

#[test]
fn proxy_draft_secret_wins_and_is_not_used_as_jump_ssh_password() {
    let (destination, mut profile) = host("root@target");
    custom_proxy(&mut profile, SshHostProxyMode::Socks5, "draft-proxy");
    profile.connection.proxy_username = "proxy-user".to_owned();
    jump(&mut profile, "bastion@hop");
    let route = resolve_route_with(
        destination,
        profile,
        &global_proxy(),
        0,
        Some("draft-secret"),
        &mut |_| Ok(host("bastion@hop")),
        &mut |_| panic!("must not read an older credential when a draft was supplied"),
    )
    .unwrap();
    assert!(!route.pool_key().contains("draft-secret"));
    let RouteTransport::Jump(jump) = route.transport else { panic!("expected jump") };
    assert_eq!(jump.profile.destination, "bastion@hop");
    let RouteTransport::Server(server) = jump.transport else { panic!("expected proxy") };
    assert_eq!(server.password.as_deref(), Some("draft-secret"));
    assert!(!format!("{server:?}").contains("draft-secret"));
}

#[test]
fn proxy_password_load_uses_its_own_destination_scoped_credential_target() {
    let (destination, mut profile) = host("root@target");
    custom_proxy(&mut profile, SshHostProxyMode::Http, "stored-proxy");
    profile.connection.proxy_username = "proxy-user".to_owned();
    let expected = profile.connection.proxy_credential_target("root@target").unwrap();
    let route = resolve_route_with(
        destination,
        profile,
        &global_proxy(),
        0,
        None,
        &mut |_| panic!("no jump configured"),
        &mut |target| {
            assert_eq!(target, expected);
            Ok(Some(b"stored-secret".to_vec()))
        },
    )
    .unwrap();
    let RouteTransport::Server(server) = route.transport else { panic!("expected proxy") };
    assert_eq!(server.password.as_deref(), Some("stored-secret"));
}

#[test]
fn test_request_debug_redacts_both_password_fields() {
    let request = SshTestRequest {
        request_id: 1,
        destination: "root@target".to_owned(),
        auth: SshAuthMode::Password,
        private_keys: Vec::new(),
        password: Some("ssh-secret".to_owned()),
        connection: Default::default(),
        proxy_password: Some("proxy-secret".to_owned()),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains("ssh-secret"));
    assert!(!debug.contains("proxy-secret"));
    assert_eq!(debug.matches("[redacted]").count(), 2);
}
