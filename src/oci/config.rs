use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageConfig {
    pub config: Option<OciImageProcessConfig>,
    pub rootfs: Option<OciImageRootfs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageProcessConfig {
    #[serde(default, rename = "Entrypoint")]
    #[allow(non_snake_case)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, rename = "Cmd")]
    #[allow(non_snake_case)]
    pub cmd: Option<Vec<String>>,
    #[serde(default, rename = "Env")]
    #[allow(non_snake_case)]
    pub env_vars: Option<Vec<String>>,
    #[serde(default, rename = "WorkingDir")]
    #[allow(non_snake_case)]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciImageRootfs {
    #[serde(rename = "type")]
    pub rootfs_type: String,
    #[serde(default)]
    pub diff_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsBundleSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("oci image config has no process configuration")]
    MissingProcessConfig,
    #[error("process has neither Entrypoint nor Cmd")]
    MissingProcessArgv,
    #[error("environment entry is missing '=' delimiter: {0}")]
    InvalidEnvironmentEntry(String),
}

const DEFAULT_WORKDIR: &str = "/";

impl TryFrom<&OciImageConfig> for RootfsBundleSpec {
    type Error = ConfigError;

    fn try_from(config: &OciImageConfig) -> Result<Self, Self::Error> {
        let proc = config
            .config
            .as_ref()
            .ok_or(ConfigError::MissingProcessConfig)?;

        let entrypoint = proc.entrypoint.as_deref().unwrap_or(&[]);
        let cmd = proc.cmd.as_deref().unwrap_or(&[]);

        let argv: Vec<String> = if !entrypoint.is_empty() {
            [entrypoint, cmd].concat()
        } else if !cmd.is_empty() {
            cmd.to_vec()
        } else {
            return Err(ConfigError::MissingProcessArgv);
        };

        let env: Vec<(String, String)> = proc
            .env_vars
            .as_ref()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        let (key, value) = entry
                            .split_once('=')
                            .ok_or_else(|| ConfigError::InvalidEnvironmentEntry(entry.clone()))?;
                        Ok((key.to_string(), value.to_string()))
                    })
                    .collect::<Result<Vec<_>, ConfigError>>()
            })
            .transpose()?
            .unwrap_or_default();

        let workdir = proc
            .working_dir
            .as_deref()
            .unwrap_or(DEFAULT_WORKDIR)
            .to_string();

        Ok(Self { argv, env, workdir })
    }
}
