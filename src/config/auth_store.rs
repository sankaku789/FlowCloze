use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Credential {
    ApiKey { value: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthStore {
    #[serde(default)]
    credentials: HashMap<String, Credential>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

#[derive(Debug)]
pub enum AuthError {
    Io(std::io::Error),
    Yaml(serde_yaml::Error),
    Config(String),
    Missing { provider: String },
    EmptyKey,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "auth file error: {error}"),
            Self::Yaml(error) => write!(f, "invalid auth.yaml: {error}"),
            Self::Config(error) => write!(f, "auth config error: {error}"),
            Self::Missing { provider } => write!(f, "missing API key for provider: {provider}"),
            Self::EmptyKey => write!(f, "API key must not be empty"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<std::io::Error> for AuthError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_yaml::Error> for AuthError {
    fn from(value: serde_yaml::Error) -> Self {
        Self::Yaml(value)
    }
}

impl AuthStore {
    pub fn load() -> Result<Self, AuthError> {
        let directory = crate::config::config_dir().map_err(AuthError::Config)?;
        Self::load_from(directory.join("auth.yaml"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self, AuthError> {
        if !path.exists() {
            return Ok(Self {
                path: Some(path),
                ..Self::default()
            });
        }
        let mut store: Self = serde_yaml::from_str(&fs::read_to_string(&path)?)?;
        store.path = Some(path);
        Ok(store)
    }

    pub fn api_key(&self, provider: &str) -> Option<&str> {
        match self.credentials.get(provider) {
            Some(Credential::ApiKey { value }) => Some(value),
            None => None,
        }
    }

    pub fn require_api_key(&self, provider: &str) -> Result<&str, AuthError> {
        self.api_key(provider).ok_or_else(|| AuthError::Missing {
            provider: provider.to_string(),
        })
    }

    pub fn set_api_key(&mut self, provider: &str, key: &str) -> Result<(), AuthError> {
        if key.trim().is_empty() {
            return Err(AuthError::EmptyKey);
        }
        self.credentials.insert(
            provider.to_string(),
            Credential::ApiKey {
                value: key.to_string(),
            },
        );
        Ok(())
    }

    pub fn save(&self) -> Result<(), AuthError> {
        let path = match &self.path {
            Some(path) => path.clone(),
            None => crate::config::config_dir()
                .map_err(AuthError::Config)?
                .join("auth.yaml"),
        };
        write_private_file(&path, serde_yaml::to_string(self)?.as_bytes())
    }
}

fn write_private_file(path: &Path, body: &[u8]) -> Result<(), AuthError> {
    let directory = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "auth path has no parent")
    })?;
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = path.with_extension("yaml.tmp");
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(body)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_yaml_roundtrip_preserves_permissions() {
        let directory = std::env::temp_dir().join(format!(
            "flowcloze-auth-yaml-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("auth.yaml");
        let mut store = AuthStore::load_from(path.clone()).unwrap();
        store.set_api_key("google", "secret").unwrap();
        store.save().unwrap();
        assert_eq!(
            AuthStore::load_from(path.clone())
                .unwrap()
                .api_key("google"),
            Some("secret")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn auth_yaml_rejects_unknown_fields_and_reports_missing_keys() {
        assert!(serde_yaml::from_str::<AuthStore>("unknown: true\n").is_err());
        assert!(matches!(
            AuthStore::default().require_api_key("google"),
            Err(AuthError::Missing { .. })
        ));
    }
}
