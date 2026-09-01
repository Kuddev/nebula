//! Shared model for the manual SSH proxy controls.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualProxyProtocol {
    #[default]
    Socks5,
    Http,
}

pub(crate) const MANUAL_PROXY_PROTOCOL_OPTIONS: [ManualProxyProtocol; 2] =
    [ManualProxyProtocol::Socks5, ManualProxyProtocol::Http];

/// State of the network test for the settings currently persisted on disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum ProxyTestStatus {
    #[default]
    Idle,
    Running,
    Complete {
        outcome: crate::proxy_test::ProxyTestOutcome,
        elapsed_ms: u64,
    },
}

/// Split a persisted proxy URL into the protocol control and editable address.
pub(crate) fn manual_proxy_parts(value: &str) -> (ManualProxyProtocol, &str) {
    let value = value.trim();
    for (prefix, protocol) in [
        ("socks5://", ManualProxyProtocol::Socks5),
        ("socks5h://", ManualProxyProtocol::Socks5),
        ("socks://", ManualProxyProtocol::Socks5),
        ("http://", ManualProxyProtocol::Http),
    ] {
        if let Some(address) = crate::ssh_proxy::strip_prefix_ignore_case(value, prefix) {
            return (protocol, address);
        }
    }
    (ManualProxyProtocol::Socks5, value)
}

pub(crate) fn manual_proxy_value(protocol: ManualProxyProtocol, address: &str) -> String {
    let address = address.trim();
    if address.is_empty() {
        return String::new();
    }
    match protocol {
        ManualProxyProtocol::Socks5 => format!("socks5://{address}"),
        ManualProxyProtocol::Http => format!("http://{address}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{ManualProxyProtocol, manual_proxy_parts, manual_proxy_value};

    #[test]
    fn protocol_and_address_round_trip_without_duplicate_prefixes() {
        assert_eq!(
            manual_proxy_parts("SOCKS5://127.0.0.1:1080"),
            (ManualProxyProtocol::Socks5, "127.0.0.1:1080")
        );
        assert_eq!(
            manual_proxy_parts("HTTP://proxy.lan:8080"),
            (ManualProxyProtocol::Http, "proxy.lan:8080")
        );
        assert_eq!(
            manual_proxy_parts("127.0.0.1:7890"),
            (ManualProxyProtocol::Socks5, "127.0.0.1:7890")
        );
        assert_eq!(
            manual_proxy_value(ManualProxyProtocol::Socks5, "127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(manual_proxy_value(ManualProxyProtocol::Http, ""), "");
    }

    #[test]
    fn parts_accept_non_ascii_without_slicing_inside_utf8() {
        for value in ["127.0.0.1：", "：127.0.0.1", "127.0：0.1", "："] {
            assert_eq!(manual_proxy_parts(value), (ManualProxyProtocol::Socks5, value));
        }
    }
}
