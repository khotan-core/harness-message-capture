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
    pub org_id: String,
    pub api_url: String,
    pub credential_path: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub api_key: String,
}

impl RouteRef {
    fn new(api_url: String, org_id: String, credential_path: PathBuf, label: String) -> RouteRef {
        RouteRef {
            id: stable_route_id(&api_url, &org_id),
            org_id,
            api_url,
            credential_path,
            label,
        }
    }
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

/// A specified FNV-1a digest keeps queue directory names stable across Rust
/// versions. Existing route metadata still makes collisions fail closed.
fn stable_route_id(api_url: &str, org_id: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in api_url
        .bytes()
        .chain(std::iter::once(b'\n'))
        .chain(org_id.bytes())
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
    let _api_key = required(&parsed, "KHOTAN_API_KEY", path)?;
    let org_id = required(&parsed, "KHOTAN_ORG_ID", path)?;
    let api_url = normalize_api_url(&api_url)?;
    let repo = path.parent().context("destination file has no parent")?;
    let label = repo
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "customer".to_string());
    Ok(RouteRef::new(api_url, org_id, path.to_path_buf(), label))
}

pub fn read_credentials(route: &RouteRef) -> Result<Credentials> {
    let current = load_route(&route.credential_path)?;
    if current.api_url != route.api_url || current.org_id != route.org_id {
        bail!("destination identity changed since records were queued")
    }
    let parsed = parse_env(&fs::read_to_string(&route.credential_path)?);
    Ok(Credentials {
        api_key: required(&parsed, "KHOTAN_API_KEY", &route.credential_path)?,
    })
}

fn required(map: &BTreeMap<String, String>, key: &str, path: &Path) -> Result<String> {
    map.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("{key} is missing in {}", path.display()))
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
            if seen.insert((route.org_id.clone(), route.api_url.clone())) {
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
        let first = stable_route_id("https://customer.example", "org-1");
        let second = stable_route_id("https://customer.example", "org-1");
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(normalize_api_url("https://user:secret@customer.example").is_err());
        assert!(normalize_api_url("https://customer.example/api").is_err());
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
        assert_eq!(route.org_id, "org");
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
        assert_eq!(route.org_id, "nested-org");
        assert_eq!(route.credential_path, nested.join(ENV_FILE));
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn rejects_incomplete_destination() {
        let repo = temp_repo("incomplete");
        fs::write(
            repo.join(ENV_FILE),
            "KHOTAN_API_URL='https://customer.example'\nKHOTAN_API_KEY='key'\n",
        )
        .unwrap();
        assert!(resolve(&repo).is_err());
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
    fn deduplicates_mirrors_by_org_and_origin() {
        let one = temp_repo("one");
        let two = temp_repo("two");
        write_env(&one, ENV_FILE, "https://same.example", "one", "org");
        write_env(&two, ENV_FILE, "https://same.example/", "two", "org");
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
        assert_eq!(route.org_id, "org");
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
            "org".into(),
            PathBuf::from("/Users/a/Developer/customer/env.khotan.local"),
            "customer".into(),
        );
        assert!(route_allowed(&route, &["customer".into()]));
        assert!(!route_allowed(&route, &["other".into()]));
        assert!(!route_allowed(&route, &[]));
    }
}
