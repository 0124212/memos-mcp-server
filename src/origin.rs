//! Origin checking, ported from `server/router/mcp/origin.go`.

/// Allow empty origin (non-browser clients). Otherwise the Origin host must
/// equal the request Host, or match the configured instance URL host.
pub fn is_allowed_origin(host: &str, origin: Option<&str>, instance_url: &str) -> bool {
    let Some(origin) = origin.filter(|o| !o.is_empty()) else {
        return true;
    };
    let Ok(origin_url) = url::Url::parse(origin) else {
        return false;
    };
    if origin_url.host_str().is_none() {
        return false;
    }
    if origin_url.host_str() == host_port_host(host) {
        return true;
    }
    if instance_url.is_empty() {
        return false;
    }
    let Ok(inst) = url::Url::parse(instance_url) else {
        return false;
    };
    origin_url.scheme() == inst.scheme() && origin_url.host_str() == inst.host_str()
}

fn host_port_host(host: &str) -> Option<&str> {
    // `host` may be `example.com:5230`; compare host part only.
    // url crate has no bare-host parser; split manually.
    let h = host.rsplit('@').next().unwrap_or(host);
    let h = h.strip_prefix('[').unwrap_or(h);
    let h = match h.rfind("]:") {
        Some(_) => h.split("]:").next().unwrap_or(h),
        None => h.rsplit(':').next().map_or(h, |port| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                &h[..h.len() - port.len() - 1]
            } else {
                h
            }
        }),
    };
    Some(h.trim_end_matches(']'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_origin_allowed() {
        assert!(is_allowed_origin("example.com", None, ""));
    }

    #[test]
    fn same_host_allowed() {
        assert!(is_allowed_origin(
            "example.com:5230",
            Some("https://example.com:5230"),
            ""
        ));
    }
}
