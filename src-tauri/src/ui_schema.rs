/// `https://icons.duckduckgo.com/ip3/<host>.ico` for any allowed host.
pub fn favicon_for_host(host: &str) -> Option<String> {
    let host = host.trim().trim_start_matches("www.").to_lowercase();
    if host.is_empty() || host.contains([' ', '/', '?', '#']) || !host.contains('.') {
        return None;
    }
    Some(format!("https://icons.duckduckgo.com/ip3/{host}.ico"))
}

pub fn host_of_url(url: &str) -> Option<String> {
    url.split_once("://").and_then(|(_, rest)| {
        let authority = rest.split('/').next().unwrap_or(rest);
        let hostname = authority.split('@').next_back().unwrap_or(authority);
        let hostname = hostname.split(':').next().unwrap_or(hostname);
        let hostname = hostname.trim().trim_end_matches('.').to_lowercase();
        if hostname.is_empty() || !hostname.contains('.') {
            return None;
        }
        Some(hostname)
    })
}

pub fn favicon_for_url(url: &str) -> Option<String> {
    host_of_url(url).and_then(|host| favicon_for_host(&host))
}
