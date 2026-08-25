use crate::workspace::primary_repo_for_worktree;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const ENV_FILE: &str = "env.khotan.local";
const DOTTED_ENV_FILE: &str = ".env.khotan.local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRef {
    pub id: String,
    /// The organization this route is bound to: declared in the destination
    /// file, or pinned into the queue the first time the endpoint named it.
    /// Absent until either happens — the organization is no longer required to
    /// enrol.
    #[serde(default)]
    pub org_id: Option<String>,
    pub api_url: String,
    /// FNV-1a of the API key that reaches this origin. `None` only for a queue
    /// written before this field existed; a route read from a file always has
    /// one. It identifies a queue without the network and is not the key.
    #[serde(default)]
    pub key_fingerprint: Option<u64>,
    pub credential_path: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: String,
}

impl RouteRef {
    fn new(
        api_url: String,
        org_id: Option<String>,
        api_key: &str,
        credential_path: PathBuf,
        label: String,
    ) -> RouteRef {
        let fingerprint = key_fingerprint(api_key);
        RouteRef {
            id: stable_route_id(&api_url, fingerprint),
            org_id,
            api_url,
            key_fingerprint: Some(fingerprint),
            credential_path,
            label,
        }
    }
}

/// FNV-1a of an API key. Not reversible to the key in any practical sense, and
/// the key itself is never written beside it. One definition serves discovery,
/// queue identity, and the upload path's verification cache.
pub fn key_fingerprint(api_key: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in api_key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// True when this workspace may be captured. An empty allowlist sends nothing.
/// A name matches the folder leaf exactly, ignoring case. `podium-automation`
/// does not match `podium-automation-mirror`. An absolute path matches only
/// that exact workspace.
pub fn workspace_allowed(workspace: &Path, allow: &[String]) -> bool {
    let allow = nonempty_allow(allow);
    if allow.is_empty() {
        return false;
    }
    if matches_allow(workspace, &allow) {
        return true;
    }
    match primary_repo_for_worktree(workspace) {
        Ok(Some(primary)) if primary != workspace => matches_allow(&primary, &allow),
        _ => false,
    }
}

fn nonempty_allow(allow: &[String]) -> Vec<&str> {
    allow
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn matches_allow(workspace: &Path, allow: &[&str]) -> bool {
    allow.iter().any(|entry| {
        let path = Path::new(entry);
        if path.is_absolute() {
            workspace == path
        } else {
            workspace
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name_matches(name, entry))
        }
    })
}

fn name_matches(leaf: &str, entry: &str) -> bool {
    let leaf = leaf.to_ascii_lowercase();
    let entry = entry.to_ascii_lowercase();
    leaf == entry
}

/// True when a queued route still belongs on the allowlist.
pub fn route_allowed(route: &RouteRef, allow: &[String]) -> bool {
    if nonempty_allow(allow).is_empty() {
        return false;
    }
    if let Some(parent) = route.credential_path.parent() {
        return workspace_allowed(parent, allow);
    }
    allow
        .iter()
        .any(|entry| name_matches(&route.label, entry.trim()))
}

/// Whether a repository could upload today, for the `configure` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// No destination file inside the repository boundary.
    NoFile,
    Ready,
    /// A destination file exists but cannot produce a route. Carries a short
    /// phrase for the list, such as `missing KHOTAN_ORG_ID`.
    Blocked(String),
}

/// `resolve` stays the only authority on whether a repository is usable. This
/// adds a reason on the failure path so a typo does not hide a repository from
/// the list without an explanation.
pub fn readiness(workspace: &Path) -> Readiness {
    match resolve(workspace) {
        Ok(Some(_)) => Readiness::Ready,
        Ok(None) => Readiness::NoFile,
        Err(_) => Readiness::Blocked(diagnose(workspace)),
    }
}

fn diagnose(workspace: &Path) -> String {
    let Some((path, both)) = nearest_env_file(workspace) else {
        return "destination file is unreadable".to_string();
    };
    if both {
        return "env.khotan.local and .env.khotan.local disagree".to_string();
    }
    let Ok(body) = fs::read_to_string(&path) else {
        return "destination file is unreadable".to_string();
    };
    let parsed = parse_env(&body);
    let missing: Vec<&str> = ["KHOTAN_API_URL", "KHOTAN_API_KEY"]
        .into_iter()
        .filter(|key| {
            parsed
                .get(*key)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        })
        .collect();
    if !missing.is_empty() {
        return format!("missing {}", missing.join(", "));
    }
    let url = parsed
        .get("KHOTAN_API_URL")
        .map(String::as_str)
        .unwrap_or("");
    match normalize_api_url(url) {
        Err(_) => "KHOTAN_API_URL must be a bare origin".to_string(),
        Ok(_) => "destination file is not usable".to_string(),
    }
}

/// Nearest destination file inside the repository boundary, tolerating the
/// conflict that `select_env_file` rejects. The flag reports that conflict.
fn nearest_env_file(start: &Path) -> Option<(PathBuf, bool)> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        let canonical = dir.join(ENV_FILE);
        let dotted = dir.join(DOTTED_ENV_FILE);
        let both = canonical.is_file() && dotted.is_file();
        if canonical.is_file() {
            return Some((canonical, both));
        }
        if dotted.is_file() {
            return Some((dotted, both));
        }
        if dir.join(".git").exists() {
            return None;
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// Find the authoritative repo-local route for a resolved harness workspace.
/// No ambient process environment or machine-global Khotan profile is used.
pub fn resolve(workspace: &Path) -> Result<Option<RouteRef>> {
    if let Some(path) = find_env_within_repo(workspace)? {
        return load_route(&path).map(Some);
    }
    if let Some(primary) = primary_repo_for_worktree(workspace)? {
        if primary != workspace {
            if let Some(path) = select_env_file(&primary)? {
                return load_route(&path).map(Some);
            }
        }
    }
    Ok(None)
}

fn find_env_within_repo(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent().unwrap_or(start).to_path_buf()
    };
    let mut nearest = None;
    loop {
        if nearest.is_none() {
            nearest = select_env_file(&dir)?;
        }
        if dir.join(".git").exists() {
            return Ok(nearest);
        }
        let Some(parent) = dir.parent() else {
            return Ok(None);
        };
        dir = parent.to_path_buf();
    }
}

/// The name a *new* queue directory takes: origin plus a fingerprint of the
/// key, never the organization, so an identity can be computed offline the
/// moment a destination file is read. A specified FNV-1a digest keeps the name
/// stable across Rust versions. Existing directories are matched by their
/// recorded metadata, not by recomputing this, so their names never change.
fn stable_route_id(api_url: &str, fingerprint: u64) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in api_url
        .bytes()
        .chain(std::iter::once(b'\n'))
        .chain(format!("{fingerprint:016x}").bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn select_env_file(dir: &Path) -> Result<Option<PathBuf>> {
    let canonical = dir.join(ENV_FILE);
    let dotted = dir.join(DOTTED_ENV_FILE);
    match (canonical.is_file(), dotted.is_file()) {
        (false, false) => Ok(None),
        (true, false) => Ok(Some(canonical)),
        (false, true) => Ok(Some(dotted)),
        (true, true) => {
            let left = parse_env(&fs::read_to_string(&canonical)?);
            let right = parse_env(&fs::read_to_string(&dotted)?);
            if route_values(&left) == route_values(&right) {
                Ok(Some(canonical))
            } else {
                bail!(
                    "conflicting {} and {} in {}",
                    ENV_FILE,
                    DOTTED_ENV_FILE,
                    dir.display()
                )
            }
        }
    }
}

fn route_values(map: &BTreeMap<String, String>) -> [Option<&str>; 3] {
    [
        map.get("KHOTAN_API_URL").map(String::as_str),
        map.get("KHOTAN_API_KEY").map(String::as_str),
        map.get("KHOTAN_ORG_ID").map(String::as_str),
    ]
}

fn load_route(path: &Path) -> Result<RouteRef> {
    let parsed =
        parse_env(&fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?);
    let api_url = required(&parsed, "KHOTAN_API_URL", path)?;
    let api_key = required(&parsed, "KHOTAN_API_KEY", path)?;
    let org_id = optional(&parsed, "KHOTAN_ORG_ID");
    let api_url = normalize_api_url(&api_url)?;
    let repo = path.parent().context("destination file has no parent")?;
    let label = repo
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "customer".to_string());
    Ok(RouteRef::new(
        api_url,
        org_id,
        &api_key,
        path.to_path_buf(),
        label,
    ))
}

/// The key that reaches a queue's origin. The organization is deliberately not
/// checked here — it is resolved and enforced against the endpoint at send time,
/// and the key may have been rotated to a new one for the same customer. Only
/// the origin must still agree, so a file repurposed to a different customer
/// stops serving this queue rather than delivering its records to the wrong one.
pub fn read_credentials(route: &RouteRef) -> Result<Credentials> {
    let parsed = parse_env(
        &fs::read_to_string(&route.credential_path)
            .with_context(|| format!("read {}", route.credential_path.display()))?,
    );
    let api_url = normalize_api_url(&required(
        &parsed,
        "KHOTAN_API_URL",
        &route.credential_path,
    )?)?;
    if api_url != route.api_url {
        bail!("destination origin changed since records were queued")
    }
    Ok(Credentials {
        api_key: required(&parsed, "KHOTAN_API_KEY", &route.credential_path)?,
    })
}

/// True when two routes name the same destination: same origin, same key. Used
/// to re-point a queue at a live file when the one it was pinned to stops
/// producing credentials. Identity cannot be confirmed without both
/// fingerprints, so a legacy queue that never recorded one does not match.
pub fn same_identity(a: &RouteRef, b: &RouteRef) -> bool {
    a.api_url == b.api_url
        && matches!(
            (a.key_fingerprint, b.key_fingerprint),
            (Some(x), Some(y)) if x == y
        )
}

fn required(map: &BTreeMap<String, String>, key: &str, path: &Path) -> Result<String> {
    map.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("{key} is missing in {}", path.display()))
}

/// A value that may be absent or blank. An empty declaration is the same as
/// none — the field is optional now, and a leftover `KHOTAN_ORG_ID=` says
/// nothing about the organization.
fn optional(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn normalize_api_url(raw: &str) -> Result<String> {
    let normalized = raw.trim().trim_end_matches('/').to_string();
    if !(normalized.starts_with("https://") || normalized.starts_with("http://")) {
        bail!("KHOTAN_API_URL must use http or https")
    }
    if normalized.chars().any(char::is_whitespace) {
        bail!("KHOTAN_API_URL must not contain whitespace")
    }
    let authority = normalized
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("");
    if authority.is_empty()
        || authority.starts_with('/')
        || authority.contains('@')
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        bail!("KHOTAN_API_URL must be an origin without userinfo or a path")
    }
    Ok(normalized)
}

pub fn parse_env(contents: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = raw_value.trim();
        if value.len() >= 2
            && ((value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"')))
        {
            value = &value[1..value.len() - 1];
        }
        map.insert(key.to_string(), value.to_string());
    }
    map
}

pub fn discover_routes(workspaces: &[PathBuf], allow: &[String]) -> Vec<RouteRef> {
    let mut seen = BTreeSet::new();
    let mut routes = Vec::new();
    for workspace in workspaces {
        if !workspace_allowed(workspace, allow) {
            continue;
        }
        if let Ok(Some(route)) = resolve(workspace) {
            if seen.insert((route.api_url.clone(), route.key_fingerprint)) {
                routes.push(route);
            }
        }
    }
    routes.sort_by(|left, right| left.label.cmp(&right.label));
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hmc-destination-{name}-{stamp}"));
        fs::create_dir_all(path.join(".git")).unwrap();
        path
    }

    fn write_env(repo: &Path, file: &str, url: &str, key: &str, org: &str) {
        fs::write(
            repo.join(file),
            format!("KHOTAN_API_URL='{url}'\nKHOTAN_API_KEY='{key}'\nKHOTAN_ORG_ID='{org}'\n"),
        )
        .unwrap();
    }

    #[test]
    fn parses_quotes_and_normalizes_url() {
        let map =
            parse_env("KHOTAN_API_URL='https://customer.example/'\nKHOTAN_ORG_ID=\"org-1\"\n");
        assert_eq!(map["KHOTAN_ORG_ID"], "org-1");
        assert_eq!(
            normalize_api_url(&map["KHOTAN_API_URL"]).unwrap(),
            "https://customer.example"
        );
    }

    #[test]
    fn route_ids_are_repeatable_and_urls_cannot_embed_secrets_or_paths() {
        let fingerprint = key_fingerprint("some-api-key");
        let first = stable_route_id("https://customer.example", fingerprint);
        let second = stable_route_id("https://customer.example", fingerprint);
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(normalize_api_url("https://user:secret@customer.example").is_err());
        assert!(normalize_api_url("https://customer.example/api").is_err());
    }

    #[test]
    fn identity_follows_the_key_not_the_organization() {
        let repo = temp_repo("identity");
        write_env(
            &repo,
            ENV_FILE,
            "https://customer.example",
            "key-one",
            "org",
        );
        let one = resolve(&repo).unwrap().unwrap();
        // Same origin, same declared org, a different key: a different queue.
        write_env(
            &repo,
            ENV_FILE,
            "https://customer.example",
            "key-two",
            "org",
        );
        let two = resolve(&repo).unwrap().unwrap();
        assert_ne!(one.id, two.id);
        assert_ne!(one.key_fingerprint, two.key_fingerprint);
        // The same key with no declared org still lands the same identity.
        fs::write(
            repo.join(ENV_FILE),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='key-one'\n",
        )
        .unwrap();
        let undeclared = resolve(&repo).unwrap().unwrap();
        assert_eq!(undeclared.id, one.id);
        assert_eq!(undeclared.org_id, None);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn env_without_a_git_boundary_is_not_a_destination() {
        let root = temp_repo("no-git");
        fs::remove_dir_all(root.join(".git")).unwrap();
        write_env(&root, ENV_FILE, "https://wrong.example", "key", "wrong");
        let child = root.join("nested").join("workspace");
        fs::create_dir_all(&child).unwrap();
        assert!(resolve(&child).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_complete_repo_destination() {
        let repo = temp_repo("complete");
        write_env(&repo, ENV_FILE, "https://customer.example/", "key", "org");
        let route = resolve(&repo).unwrap().unwrap();
        assert_eq!(route.api_url, "https://customer.example");
        assert_eq!(route.org_id.as_deref(), Some("org"));
        assert!(route.key_fingerprint.is_some());
        assert!(!route.id.contains("key"));
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn nearest_nested_destination_wins_within_repo_boundary() {
        let repo = temp_repo("nested");
        write_env(&repo, ENV_FILE, "https://root.example", "root", "root-org");
        let nested = repo.join("packages").join("customer");
        fs::create_dir_all(&nested).unwrap();
        write_env(
            &nested,
            ENV_FILE,
            "https://nested.example",
            "nested",
            "nested-org",
        );
        let route = resolve(&nested).unwrap().unwrap();
        assert_eq!(route.org_id.as_deref(), Some("nested-org"));
        assert_eq!(route.credential_path, nested.join(ENV_FILE));
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn an_origin_and_a_key_are_enough_with_or_without_the_org() {
        let repo = temp_repo("two-keys");
        // Two keys: origin and API key, no organization.
        fs::write(
            repo.join(ENV_FILE),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='key'\n",
        )
        .unwrap();
        let route = resolve(&repo).unwrap().unwrap();
        assert_eq!(route.org_id, None);
        assert_eq!(readiness(&repo), Readiness::Ready);

        // Three keys: the organization is accepted and carried when declared.
        write_env(&repo, ENV_FILE, "https://customer.example", "key", "org");
        assert_eq!(
            resolve(&repo).unwrap().unwrap().org_id.as_deref(),
            Some("org")
        );
        assert_eq!(readiness(&repo), Readiness::Ready);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn a_file_missing_the_api_key_is_blocked_naming_it() {
        let repo = temp_repo("no-key");
        fs::write(
            repo.join(ENV_FILE),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_ORG_ID='org'\n",
        )
        .unwrap();
        assert!(resolve(&repo).is_err());
        match readiness(&repo) {
            Readiness::Blocked(reason) => {
                assert!(reason.contains("KHOTAN_API_KEY"), "{reason}");
                assert!(!reason.contains("KHOTAN_ORG_ID"), "{reason}");
            }
            other => panic!("expected blocked, got {other:?}"),
        }
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn rejects_conflicting_dotted_files() {
        let repo = temp_repo("conflict");
        write_env(&repo, ENV_FILE, "https://one.example", "key", "org");
        write_env(&repo, DOTTED_ENV_FILE, "https://two.example", "key", "org");
        assert!(resolve(&repo).is_err());
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn deduplicates_mirrors_that_carry_the_same_origin_and_key() {
        let one = temp_repo("one");
        let two = temp_repo("two");
        // Identical origin and key: the same destination reached from two
        // checkouts. The trailing slash and the declared org must not matter.
        write_env(&one, ENV_FILE, "https://same.example", "shared-key", "org");
        write_env(
            &two,
            ENV_FILE,
            "https://same.example/",
            "shared-key",
            "other",
        );
        let routes = discover_routes(
            &[one.clone(), two.clone()],
            &[
                one.file_name().unwrap().to_string_lossy().into_owned(),
                two.file_name().unwrap().to_string_lossy().into_owned(),
            ],
        );
        assert_eq!(routes.len(), 1);
        let _ = fs::remove_dir_all(one);
        let _ = fs::remove_dir_all(two);
    }

    #[test]
    fn two_origins_and_two_keys_on_one_origin_stay_separate() {
        let a = temp_repo("origin-a");
        let b = temp_repo("origin-b");
        let c = temp_repo("origin-a-second-key");
        // Two different origins.
        write_env(&a, ENV_FILE, "https://a.example", "key-a", "org");
        write_env(&b, ENV_FILE, "https://b.example", "key-a", "org");
        // Same origin as `a`, a different key.
        write_env(&c, ENV_FILE, "https://a.example", "key-c", "org");
        let names: Vec<String> = [&a, &b, &c]
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let routes = discover_routes(&[a.clone(), b.clone(), c.clone()], &names);
        assert_eq!(routes.len(), 3);
        let ids: BTreeSet<&str> = routes.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "each origin/key pair is its own queue");
        let _ = fs::remove_dir_all(a);
        let _ = fs::remove_dir_all(b);
        let _ = fs::remove_dir_all(c);
    }

    #[test]
    fn worktree_inherits_primary_repo_destination() {
        let repo = temp_repo("primary");
        write_env(&repo, ENV_FILE, "https://customer.example", "key", "org");
        let parent = repo.parent().unwrap().to_path_buf();
        let worktree = parent.join(format!(
            "{}-worktree",
            repo.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir_all(&worktree).unwrap();
        fs::create_dir_all(repo.join(".git").join("worktrees").join("branch")).unwrap();
        fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                repo.join(".git").join("worktrees").join("branch").display()
            ),
        )
        .unwrap();

        let route = resolve(&worktree).unwrap().unwrap();
        assert_eq!(route.org_id.as_deref(), Some("org"));
        assert_eq!(route.credential_path, repo.join(ENV_FILE));
        let _ = fs::remove_dir_all(worktree);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn empty_allowlist_sends_nothing() {
        let repo = PathBuf::from("/Users/a/Developer/customer");
        assert!(!workspace_allowed(&repo, &[]));
        assert!(!workspace_allowed(&repo, &["".into()]));
    }

    #[test]
    fn allowlist_matches_exact_leaf_or_exact_path() {
        let repo = PathBuf::from("/Users/a/Developer/podium-automation");
        assert!(workspace_allowed(&repo, &["podium-automation".into()]));
        assert!(workspace_allowed(&repo, &["PODIUM-AUTOMATION".into()]));
        assert!(!workspace_allowed(&repo, &["podium".into()]));
        assert!(!workspace_allowed(
            &PathBuf::from("/Users/a/Developer/podium-automation-mirror"),
            &["podium-automation".into()]
        ));
        assert!(!workspace_allowed(
            &PathBuf::from("/Users/a/Developer/podium-automation-DEPRECATED"),
            &["podium-automation".into()]
        ));
        assert!(workspace_allowed(
            &repo,
            &["/Users/a/Developer/podium-automation".into()]
        ));
        assert!(!workspace_allowed(&repo, &["/Users/a/Developer".into()]));
        assert!(workspace_allowed(
            &PathBuf::from("/Users/a/Developer/chief-nutrition"),
            &["chief-nutrition".into()]
        ));
        assert!(!workspace_allowed(
            &PathBuf::from("/Users/a/Developer/chief-nutrition-unleashed-connector"),
            &["chief-nutrition".into()]
        ));
    }

    #[test]
    fn route_allowlist_uses_credential_parent() {
        let route = RouteRef::new(
            "https://customer.example".into(),
            Some("org".into()),
            "api-key",
            PathBuf::from("/Users/a/Developer/customer/env.khotan.local"),
            "customer".into(),
        );
        assert!(route_allowed(&route, &["customer".into()]));
        assert!(!route_allowed(&route, &["other".into()]));
        assert!(!route_allowed(&route, &[]));
    }
}
