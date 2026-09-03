//! How `e` tells providers who is calling.
//!
//! Every outbound API request carries two headers:
//!
//! * `User-Agent` names the product, version and platform. A request without
//!   one is indistinguishable from a bare `curl`, and a gateway that routes,
//!   optimises or gatekeeps per client cannot tell us from traffic it would
//!   rather drop.
//! * [`SESSION_HEADER`] carries one stable id per conversation. Some gateways
//!   (OpenCode Go among them) require it and error requests that lack it. The
//!   header name is theirs, so it is kept verbatim even though the id is `e`'s
//!   own chat id.
//!
//! Both are computed once per process and read everywhere else through a
//! `OnceLock`, so the user agent cannot drift between call sites and the
//! install id survives restarts without new plumbing.

/// The header a gateway expects the conversation id under.
pub const SESSION_HEADER: &str = "x-opencode-session";

/// Product, version and platform in one line. Compile-time constants only, so
/// a build identifies itself the same way wherever it runs.
fn user_agent() -> String {
    format!(
        "e/{} ({}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// A stable-per-install id for requests that belong to no conversation — the
/// settings' model listing, for one. Stored next to the rest of `e`'s state so
/// restarts keep it; a first run mints one from the clock and the process id,
/// which is unique enough for one machine without pulling in a rand crate.
fn install_id() -> String {
    let file = dirs::home_dir()
        .unwrap_or_default()
        .join(".e")
        .join("install_id");
    if let Ok(id) = std::fs::read_to_string(&file) {
        let id = id.trim();
        if !id.is_empty() {
            return id.to_string();
        }
    }
    let id = format!(
        "e-{:x}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        std::process::id()
    );
    let parent = file.parent().unwrap_or(std::path::Path::new(""));
    if std::fs::create_dir_all(parent).is_ok() {
        let _ = std::fs::write(&file, &id);
    }
    id
}

static USER_AGENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static INSTALL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The `User-Agent` every request carries.
pub fn agent() -> &'static str {
    USER_AGENT.get_or_init(user_agent)
}

/// An install-stable id for requests made outside any conversation.
pub fn anonymous_session() -> &'static str {
    INSTALL.get_or_init(install_id)
}

/// The id to send for a conversation. Falls back to [`anonymous_session`] when
/// the caller has none, so the header is always present and never empty.
pub fn session_id(chat: &str) -> &str {
    let chat = chat.trim();
    if chat.is_empty() {
        anonymous_session()
    } else {
        chat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_names_the_product_platform_and_version() {
        let ua = agent();
        assert!(ua.starts_with("e/"), "{ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")), "{ua}");
        assert!(ua.contains(std::env::consts::OS), "{ua}");
        assert!(ua.contains(std::env::consts::ARCH), "{ua}");
    }

    #[test]
    fn a_known_chat_id_is_sent_verbatim_and_trimmed() {
        assert_eq!(session_id(" s123 "), "s123");
    }

    #[test]
    fn a_missing_chat_id_falls_back_to_the_install_id() {
        // The header must never go out empty: gateways error on a missing
        // id, and an empty value is no better than an absent one.
        assert_eq!(session_id(""), anonymous_session());
        assert_eq!(session_id("   "), anonymous_session());
    }

    #[test]
    fn the_install_id_is_nonempty_and_stable_within_the_process() {
        let first = anonymous_session();
        assert!(!first.is_empty());
        assert_eq!(anonymous_session(), first, "the id must not wander mid-session");
    }
}
