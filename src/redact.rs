use regex::Regex;
use std::sync::OnceLock;

static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

/// Credential/secret patterns scrubbed from every line before it leaves the
/// machine. Adapted from the Paxel data-handling list. Fail-closed: anything
/// that matches becomes `[REDACTED]`.
fn patterns() -> &'static Vec<Regex> {
    PATTERNS.get_or_init(|| {
        let raw = [
            // Environment/JSON assignments, including single, double, and
            // JSON-escaped quotes. Match the complete assignment so the value
            // cannot survive because of quote formatting.
            r#"(?i)\\?["']?[A-Z0-9_.-]*(?:api[_-]?key|secret(?:[_-]?key)?|access[_-]?token|auth[_-]?token|password|passwd)[A-Z0-9_.-]*\\?["']?\s*[:=]\s*\\?["'][^"'\r\n]*\\?["']"#,
            r#"(?i)\\?["']?[A-Z0-9_.-]*(?:api[_-]?key|secret(?:[_-]?key)?|access[_-]?token|auth[_-]?token|password|passwd)[A-Z0-9_.-]*\\?["']?\s*[:=]\s*[^\s"',}\\]+"#,
            r"mk_(live|test)_[A-Za-z0-9]+",         // Khotan organization keys
            r"sk-ant-[A-Za-z0-9_\-]+",             // Anthropic keys
            r"sk-[A-Za-z0-9]{20,}",                 // OpenAI keys
            r"(sk|rk)_live_[A-Za-z0-9]+",           // Stripe secret keys
            r"AKIA[0-9A-Z]{16}",                    // AWS access keys
            r"gh[pousr]_[A-Za-z0-9]{20,}",          // GitHub tokens
            r"xox[baprs]-[A-Za-z0-9\-]+",           // Slack tokens
            r"hf_[A-Za-z0-9]+",                     // HuggingFace tokens
            r"npm_[A-Za-z0-9]+",                    // npm tokens
            r"pypi-[A-Za-z0-9_\-]+",                // PyPI tokens
            r"AIza[0-9A-Za-z_\-]{35}",              // Google API keys
            r"eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+", // JWTs
            r"[Bb]earer\s+[A-Za-z0-9._\-]+",        // Bearer tokens
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",  // PEM private keys
            r#"(postgres|postgresql|redis|mongodb|mysql|amqp)://[^\s"']*@[^\s"']+"#, // conn strings w/ creds
        ];
        raw.iter().filter_map(|p| Regex::new(p).ok()).collect()
    })
}

pub fn scrub(line: &str) -> String {
    let mut out = line.to_string();
    for re in patterns() {
        if re.is_match(&out) {
            out = re.replace_all(&out, "[REDACTED]").into_owned();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::scrub;

    #[test]
    fn redacts_quoted_khotan_assignment() {
        let input = "KHOTAN_API_KEY='mk_live_FAKEVALUE123456789'";
        let output = scrub(input);
        assert!(!output.contains("FAKEVALUE"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_json_escaped_assignment() {
        let input = r#"message says KHOTAN_API_KEY=\"mk_live_FAKEJSON123456789\" next"#;
        let output = scrub(input);
        assert!(!output.contains("FAKEJSON"));
        assert!(output.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_complete_env_block_without_removing_url_or_org() {
        let input = "KHOTAN_API_URL='https://customer.example'\\nKHOTAN_API_KEY='mk_live_FAKEBLOCK123456789'\\nKHOTAN_ORG_ID='org_fake'";
        let output = scrub(input);
        assert!(output.contains("KHOTAN_API_URL"));
        assert!(output.contains("KHOTAN_ORG_ID"));
        assert!(!output.contains("FAKEBLOCK"));
    }

    #[test]
    fn leaves_ordinary_prose_intact() {
        let input = "Document how the API key is loaded from the customer repository.";
        assert_eq!(scrub(input), input);
    }
}
