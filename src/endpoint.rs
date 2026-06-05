//! Target endpoint: structured host and port parsed from `--addr` / `--port`.

/// A resolved target to dial: a host (for SNI/authority) and a port.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    host: String,
    port: u16,
}

impl std::fmt::Display for Target {
    /// Renders `host:port`, bracketing IPv6 literals.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Why an `--addr` value could not be parsed into a host and port.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AddrError {
    #[error("address {0:?} must be host:port (the port is required)")]
    MissingPort(String),
    #[error("address {0:?} has an empty host")]
    EmptyHost(String),
    #[error("address {0:?} has an invalid port: {1}")]
    InvalidPort(String, std::num::ParseIntError),
    #[error("address {0:?} has an unterminated IPv6 literal (missing ']')")]
    UnterminatedIpv6(String),
    #[error("address {0:?} looks like an IPv6 literal; wrap it in brackets: [host]:port")]
    UnbracketedIpv6(String),
}

impl Target {
    /// Parses an `--addr` value of the form `host:port`.
    /// IPv6 literals must be wrapped in brackets, e.g. `[::1]:50051`.
    pub(crate) fn parse_addr(addr: &str) -> Result<Target, AddrError> {
        // IPv6 literals carry their own colons,
        // so the bracketed form is split on the closing ']' before looking for the port separator.
        let (host, port_str) = if let Some(rest) = addr.strip_prefix('[') {
            let end = rest
                .find(']')
                .ok_or_else(|| AddrError::UnterminatedIpv6(addr.to_string()))?;
            let port_str = rest[end + 1..]
                .strip_prefix(':')
                .ok_or_else(|| AddrError::MissingPort(addr.to_string()))?;
            (&rest[..end], port_str)
        } else {
            let (host, port_str) = addr
                .rsplit_once(':')
                .ok_or_else(|| AddrError::MissingPort(addr.to_string()))?;
            // A leftover ':' in the host means an unbracketed IPv6 literal,
            // which rsplit_once would silently mis-split. Reject it outright.
            if host.contains(':') {
                return Err(AddrError::UnbracketedIpv6(addr.to_string()));
            }
            (host, port_str)
        };

        if host.is_empty() {
            return Err(AddrError::EmptyHost(addr.to_string()));
        }
        let port = port_str
            .parse::<u16>()
            .map_err(|e| AddrError::InvalidPort(addr.to_string(), e))?;

        Ok(Target {
            host: host.to_string(),
            port,
        })
    }

    /// Builds the implicit target for `--port N`: `localhost:N`.
    pub(crate) fn localhost(port: u16) -> Target {
        Target {
            host: "localhost".to_string(),
            port,
        }
    }

    /// Host used for the connection authority and TLS SNI.
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// The URI to dial: `http://host:port`, or `https://` when TLS is on.
    /// IPv6 hosts are bracketed.
    pub(crate) fn uri(&self, tls: bool) -> String {
        let scheme = if tls { "https" } else { "http" };
        // A host containing ':' is an IPv6 literal and must be bracketed in a URI.
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port() {
        let t = Target::parse_addr("example.test:1234").unwrap();
        assert_eq!(t.host(), "example.test");
        assert_eq!(t.to_string(), "example.test:1234");
    }

    #[test]
    fn rejects_addr_without_port() {
        assert!(matches!(
            Target::parse_addr("example.test"),
            Err(AddrError::MissingPort(_))
        ));
    }

    #[test]
    fn rejects_unbracketed_ipv6() {
        // A bare IPv6 literal is ambiguous:
        // rsplit on ':' would silently take the last group as the port.
        // It must be bracketed instead.
        assert!(matches!(
            Target::parse_addr("2001:db8::1:50051"),
            Err(AddrError::UnbracketedIpv6(_))
        ));
        assert!(matches!(
            Target::parse_addr("::1:50051"),
            Err(AddrError::UnbracketedIpv6(_))
        ));
    }

    #[test]
    fn accepts_ipv6_literal_in_brackets() {
        let t = Target::parse_addr("[::1]:50051").unwrap();
        assert_eq!(t.host(), "::1");
        assert_eq!(t.to_string(), "[::1]:50051");
    }

    #[test]
    fn uri_uses_http_without_tls() {
        assert_eq!(
            Target::localhost(50051).uri(false),
            "http://localhost:50051"
        );
    }

    #[test]
    fn uri_uses_https_with_tls() {
        assert_eq!(
            Target::localhost(50051).uri(true),
            "https://localhost:50051"
        );
    }

    #[test]
    fn uri_brackets_ipv6_host() {
        let t = Target::parse_addr("[::1]:50051").unwrap();
        assert_eq!(t.uri(false), "http://[::1]:50051");
    }
}
