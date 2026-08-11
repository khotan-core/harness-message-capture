use regex::Regex;
use std::sync::OnceLock;

static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

/// Credential/secret patterns scrubbed from every line before it leaves the
/// machine. Adapted from the Paxel data-handling list. Fail-closed: anything
/// that matches becomes `[REDACTED]`.
fn patterns() -> &'static Vec<Regex> {
    PATTERNS.get_or_init(|| {
        let raw = [
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
            r#"(?i)(api[_-]?key|secret[_-]?key|access[_-]?token|password|passwd)\s*[:=]\s*[^\s"']+"#, // KEY=VALUE
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
