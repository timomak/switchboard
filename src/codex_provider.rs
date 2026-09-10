//! User-configured Codex connections. Secret values never enter the registry.
mod activation;
mod compatibility;
use crate::{
    Result, cache,
    codex_account::store::{error, private_dir},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Azure,
    Bedrock,
    #[serde(rename = "openai-compatible")]
    #[value(name = "openai-compatible")]
    Compatible,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Azure => "Azure",
            Self::Bedrock => "Amazon Bedrock",
            Self::Compatible => "OpenAI-compatible API",
        }
    }
    fn defaults(self) -> Bindings {
        match self {
            Self::Azure => Bindings {
                credential_key: "AZURE_OPENAI_API_KEY".into(),
                endpoint_key: Some("AZURE_OPENAI_ENDPOINT".into()),
                region_key: None,
            },
            Self::Bedrock => Bindings {
                credential_key: "AWS_BEARER_TOKEN_BEDROCK".into(),
                endpoint_key: None,
                region_key: Some("AWS_REGION".into()),
            },
            Self::Compatible => Bindings {
                credential_key: "OPENAI_API_KEY".into(),
                endpoint_key: Some("OPENAI_BASE_URL".into()),
                region_key: None,
            },
        }
    }
}
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Action {
    /// Select a connection and restart Codex in its existing home.
    Use {
        label: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Restore the configuration from before connection selection.
    Subscription {
        /// Saved ChatGPT account to select after restoring subscription settings.
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Roll back an interrupted configuration change.
    Recover {
        #[arg(long)]
        yes: bool,
    },
    /// Register a credential source without activating or copying credentials.
    #[command(name = "add", alias = "stage")]
    Stage {
        label: String,
        #[arg(long, value_enum)]
        provider: Provider,
        #[arg(long)]
        source: PathBuf,
        /// Azure deployment name or Bedrock model ID, if already known.
        #[arg(long)]
        model: Option<String>,
        /// Environment-variable names in the source file, never key values.
        #[arg(long)]
        credential_key: Option<String>,
        #[arg(long)]
        endpoint_key: Option<String>,
        #[arg(long)]
        region_key: Option<String>,
    },
    /// List saved connections. Never reads credentials or contacts a provider.
    List,
    /// Inspect source fields and local config offline. Never switches profiles.
    Verify {
        label: String,
        #[arg(long)]
        json: bool,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub label: String,
    /// Absent for legacy records, which retain their guarded routing IDs.
    #[serde(default)]
    pub routing_id: Option<String>,
    pub provider: Provider,
    pub source: PathBuf,
    pub model: Option<String>,
    #[serde(default)]
    pub bindings: Option<Bindings>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Bindings {
    credential_key: String,
    endpoint_key: Option<String>,
    region_key: Option<String>,
}
impl Bindings {
    fn keys(&self) -> Vec<&str> {
        let mut keys = vec![self.credential_key.as_str()];
        keys.extend(self.endpoint_key.as_deref());
        keys.extend(self.region_key.as_deref());
        keys
    }
}
impl Profile {
    fn provider_id(&self) -> Result<String> {
        if !matches!(self.provider, Provider::Bedrock)
            && let Some(id) = &self.routing_id
        {
            if managed_key(id).is_none() {
                return Err(error("Invalid saved connection routing identity."));
            }
            return Ok(id.clone());
        }
        Ok(match self.provider {
            Provider::Azure => "azure",
            Provider::Compatible => "custom",
            Provider::Bedrock => "amazon-bedrock",
        }
        .into())
    }
    fn runtime_key(&self) -> Result<String> {
        Ok(managed_key(&self.provider_id()?).unwrap_or_else(|| self.bindings().credential_key))
    }
    fn bindings(&self) -> Bindings {
        self.bindings.clone().unwrap_or_else(|| {
            let mut b = self.provider.defaults();
            // Backward compatibility for existing records created before configurable bindings.
            if matches!(self.provider, Provider::Azure) {
                b.credential_key = "AZURE_FOUNDRY_API_KEY".into();
            }
            b
        })
    }
}
// IDs and credential variable names are opaque and independent of labels/secrets.
fn managed_key(id: &str) -> Option<String> {
    let suffix = id.strip_prefix("switchboard_")?;
    (suffix.len() == 32
        && suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then(|| format!("SWITCHBOARD_KEY_{}", suffix.to_ascii_uppercase()))
}
fn templates() -> Value {
    json!([Provider::Azure,Provider::Bedrock,Provider::Compatible].iter().map(|p| {
        let b=p.defaults();
        let mut fields=vec![json!({"argument":"credential-key","title":"API key variable","value":b.credential_key})];
        if let Some(key)=b.endpoint_key {fields.push(json!({"argument":"endpoint-key","title":"Endpoint variable","value":key}));}
        if let Some(key)=b.region_key {fields.push(json!({"argument":"region-key","title":"Region variable","value":key}));}
        json!({"id":p,"name":p.name(),"fields":fields})
    }).collect::<Vec<_>>())
}
fn root() -> Result<PathBuf> {
    Ok(cache::home_dir()?.join(".claude-acc/codex-cloud-providers"))
}
fn path(root: &Path, label: &str) -> Result<PathBuf> {
    crate::config::validate_account_label(label)?;
    Ok(root.join(format!("{label}.json")))
}
fn load(root: &Path, label: &str) -> Result<Profile> {
    let p = path(root, label)?;
    let bytes =
        std::fs::read(&p).map_err(|_| error("Could not read the saved connection profile."))?;
    let profile: Profile =
        serde_json::from_slice(&bytes).map_err(|_| error("Invalid provider profile."))?;
    if profile.label != label {
        return Err(error("Provider profile label mismatch."));
    }
    Ok(profile)
}

/// Read only selected literal dotenv assignments. Never source a shell file,
/// interpolate variables, execute substitutions, or include values in errors.
pub(crate) fn fields(source: &Path, required: &[&str]) -> Result<BTreeMap<String, String>> {
    let data = std::fs::read_to_string(source)
        .map_err(|_| error("Credential source could not be read."))?;
    let mut result = BTreeMap::new();
    for line in data.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !required.contains(&key) {
            continue;
        }
        let raw = raw.trim();
        let value = if raw.starts_with(['\'', '"']) {
            let quote = raw.as_bytes()[0] as char;
            let end = raw[1..]
                .find(quote)
                .map(|i| i + 1)
                .ok_or_else(|| error("A required source field is not a single-line literal."))?;
            let tail = raw[end + 1..].trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return Err(error("Unexpected text after a required source field."));
            }
            &raw[1..end]
        } else {
            raw.split(" #").next().unwrap_or("").trim()
        };
        if value.is_empty()
            || value.contains(['$', '`', '\\'])
            || value.chars().any(char::is_control)
        {
            return Err(error(
                "A required source field is empty or needs unsupported interpolation.",
            ));
        }
        if result.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(error("A required source field is duplicated."));
        }
    }
    if required.iter().any(|key| !result.contains_key(*key)) {
        return Err(error(
            "One or more required credential-source fields are missing.",
        ));
    }
    Ok(result)
}
fn validate_source(profile: &Profile) -> Result<BTreeMap<String, String>> {
    if !profile.source.is_absolute() {
        return Err(error("Use an absolute credential-source path."));
    }
    let bindings = profile.bindings();
    let keys = bindings.keys();
    if keys.iter().any(|k| {
        k.is_empty()
            || !k
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    }) {
        return Err(error(
            "Credential mappings must be environment-variable names, not values.",
        ));
    }
    if matches!(profile.provider, Provider::Bedrock)
        && (bindings.region_key.is_none() || bindings.endpoint_key.is_some())
        || !matches!(profile.provider, Provider::Bedrock)
            && (bindings.endpoint_key.is_none() || bindings.region_key.is_some())
    {
        return Err(error("The connection has incompatible field mappings."));
    }
    let values = fields(&profile.source, &keys)?;
    match profile.provider {
        Provider::Azure => {
            let url = reqwest::Url::parse(
                &values[bindings
                    .endpoint_key
                    .as_deref()
                    .ok_or_else(|| error("Endpoint mapping missing."))?],
            )
            .map_err(|_| error("Azure endpoint is invalid."))?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !["", "/", "/openai", "/openai/", "/openai/v1", "/openai/v1/"]
                    .contains(&url.path())
                || !url
                    .host_str()
                    .is_some_and(|h| h.ends_with(".openai.azure.com"))
            {
                return Err(error(
                    "Expected an HTTPS Azure OpenAI endpoint without embedded credentials.",
                ));
            }
        }
        Provider::Bedrock => {
            let region = &values[bindings
                .region_key
                .as_deref()
                .ok_or_else(|| error("Region mapping missing."))?];
            if region.len() > 40
                || !region.contains('-')
                || !region
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(error("AWS region is invalid."));
            }
        }
        Provider::Compatible => {
            let url = reqwest::Url::parse(
                &values[bindings
                    .endpoint_key
                    .as_deref()
                    .ok_or_else(|| error("Endpoint mapping missing."))?],
            )
            .map_err(|_| error("API endpoint is invalid."))?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(error(
                    "Use an HTTPS endpoint without embedded credentials or query parameters.",
                ));
            }
        }
    }
    if profile
        .model
        .as_ref()
        .is_some_and(|m| m.is_empty() || m.chars().any(char::is_control))
    {
        return Err(error("Model or deployment name is invalid."));
    }
    Ok(values)
}
fn stage(root: &Path, profile: &Profile) -> Result<()> {
    let target = path(root, &profile.label)?;
    validate_source(profile)?;
    private_dir(root)?;
    let _lock = cache::acquire_lock(
        &root.join("registry.lock"),
        std::time::Duration::from_secs(2),
    )?;
    if target.exists() {
        return Err(error(
            "That saved connection already exists. Choose another label.",
        ));
    }
    let mut profile = profile.clone();
    if !matches!(profile.provider, Provider::Bedrock) {
        profile.routing_id = Some(format!("switchboard_{}", uuid::Uuid::new_v4().simple()));
    }
    let bytes = serde_json::to_vec_pretty(&profile)
        .map_err(|_| error("Could not encode provider profile."))?;
    cache::atomic_write(&target, &bytes)
}
fn list(root: &Path) -> Result<Vec<Profile>> {
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut profiles = vec![];
    for entry in std::fs::read_dir(root).map_err(|_| error("Could not list provider profiles."))? {
        let entry = entry.map_err(|_| error("Could not read provider entry."))?;
        if entry.path().extension().is_some_and(|e| e == "json") {
            let p = entry.path();
            let label = p
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| error("Invalid provider filename."))?;
            profiles.push(load(root, label)?);
        }
    }
    profiles.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(profiles)
}
fn selection_status() -> Value {
    match root().and_then(|root| {
        crate::codex_account::store::Paths::resolve()
            .and_then(|paths| activation::status(&root, &paths.home))
    }) {
        Ok(s) => s,
        Err(e) => {
            let detail = e.to_string();
            let safe = if detail.starts_with("Codex config.toml field `") {
                detail.as_str()
            } else {
                "Connection configuration changed or needs recovery. Preserve your settings and inspect the local configuration before recovery."
            };
            json!({"error":safe,"recovery_required":true})
        }
    }
}
pub async fn execute(action: &Action) -> i32 {
    if matches!(
        action,
        Action::Use { .. } | Action::Subscription { .. } | Action::Recover { .. }
    ) {
        match activation::run(action).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!(
                    "{}",
                    crate::display::sanitize_untrusted_line(&e.to_string())
                );
                1
            }
        }
    } else {
        run(action)
    }
}
pub fn status_default() -> Value {
    match root().and_then(|r| list(&r)) {
        Ok(profiles) => {
            json!({"profiles": profiles.iter().map(|p| json!({"label":p.label,"provider":p.provider.name(),"model":p.model,"model_access":"not-checked"})).collect::<Vec<_>>(),"templates":templates(),"selection":selection_status()})
        }
        Err(_) => {
            json!({"profiles":[],"templates":templates(),"error":"Could not read saved connections."})
        }
    }
}
fn read_config(home: &Path) -> Result<toml::Value> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(s) => toml::from_str(&s).map_err(|_| error("Codex configuration could not be parsed.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(Default::default()))
        }
        Err(_) => Err(error("Codex configuration could not be read.")),
    }
}
fn verify(profile: &Profile, home: &Path) -> Result<Value> {
    let values = validate_source(profile)?;
    let bindings = profile.bindings();
    let config = read_config(home)?;
    let mut requirements = vec!["Credential validity, model access, and runtime behavior have not been tested.".to_string(),
        "Preserving files does not guarantee identical chat visibility or workflow execution after switching.".to_string()];
    if config.get("forced_login_method").and_then(|v| v.as_str()) == Some("chatgpt") {
        requirements.push("Current Codex configuration requires ChatGPT login; a provider switch must handle and restore that setting.".into());
    }
    if profile.model.is_none() {
        requirements.push(match profile.provider { Provider::Azure => "Select an existing Azure deployment name; catalog model names do not prove a deployment exists.", Provider::Bedrock => "Select a Bedrock model ID and confirm regional access.", Provider::Compatible => "Select a model ID offered by this connection and verify Responses API compatibility." }.into());
    }
    let mut workflow_models = BTreeMap::<String, usize>::new();
    let mut workflow_count = 0;
    let dir = home.join("automations");
    if dir.exists() {
        for e in
            std::fs::read_dir(dir).map_err(|_| error("Could not inspect workflow definitions."))?
        {
            let f = e
                .map_err(|_| error("Could not inspect workflow entry."))?
                .path()
                .join("automation.toml");
            if !f.exists() {
                continue;
            }
            let text = std::fs::read_to_string(f)
                .map_err(|_| error("Could not read workflow definition."))?;
            let data: toml::Value =
                toml::from_str(&text).map_err(|_| error("Could not parse workflow definition."))?;
            *workflow_models
                .entry(
                    data.get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("inherited")
                        .to_owned(),
                )
                .or_default() += 1;
            workflow_count += 1;
        }
    }
    if workflow_count > 0 {
        requirements.push("Existing scheduled workflows need provider/model compatibility review; none have been edited.".into());
    }
    // A proposal only, never merged into the live config. No credential value
    // is put into it. Keep the same CODEX_HOME and do not modify thread providers.
    let mut proposal = toml::map::Map::new();
    if let Some(model) = &profile.model {
        proposal.insert("model".into(), toml::Value::String(model.clone()));
    }
    let provider_id = profile.provider_id()?;
    let runtime_key = profile.runtime_key()?;
    match profile.provider {
        Provider::Azure => {
            proposal.insert("model_provider".into(), provider_id.clone().into());
            let mut endpoint = reqwest::Url::parse(
                &values[bindings
                    .endpoint_key
                    .as_deref()
                    .ok_or_else(|| error("Endpoint mapping missing."))?],
            )
            .map_err(|_| error("Azure endpoint is invalid."))?;
            endpoint.set_path("/openai/v1");
            let base = endpoint.as_str();
            let provider = json!({"name":"Azure","base_url":base,"env_key":runtime_key,"wire_api":"responses"});
            let provider: toml::Value = serde_json::from_value(provider)
                .map_err(|_| error("Could not prepare Azure config."))?;
            proposal.insert(
                "model_providers".into(),
                toml::Value::Table([(provider_id.clone(), provider)].into_iter().collect()),
            );
        }
        Provider::Bedrock => {
            proposal.insert("model_provider".into(), "amazon-bedrock".into());
            let provider: toml::Value = serde_json::from_value(
                json!({"amazon-bedrock":{"aws":{"region":values[bindings.region_key.as_deref().ok_or_else(|| error("Region mapping missing."))?]}}}),
            )
            .map_err(|_| error("Could not prepare Bedrock config."))?;
            proposal.insert("model_providers".into(), provider);
        }
        Provider::Compatible => {
            proposal.insert("model_provider".into(), provider_id.clone().into());
            let provider: toml::Value=serde_json::from_value(json!({(provider_id):{"name":"Switchboard connection","base_url":values[bindings.endpoint_key.as_deref().ok_or_else(|| error("Endpoint mapping missing."))?],"env_key":runtime_key,"wire_api":"responses"}})).map_err(|_| error("Could not prepare custom provider config."))?;
            proposal.insert("model_providers".into(), provider);
        }
    }
    let candidate = toml::to_string_pretty(&proposal)
        .map_err(|_| error("Could not serialize proposed config."))?;
    let _: toml::Value =
        toml::from_str(&candidate).map_err(|_| error("Proposed config is invalid."))?;
    Ok(
        json!({"label":profile.label,"provider":profile.provider.name(),"offline_checks_passed":true,"activation_verified":false,
        "source_fields_present":bindings.keys(),"configured_model":profile.model,"model_access":"not-checked","workflow_count":workflow_count,"workflow_models":workflow_models,
        "codex_home_changes":false,"auth_changes":false,"history_changes":false,"pin_changes":false,"workflow_changes":false,
        "requirements":requirements,"proposed_config":candidate}),
    )
}
pub fn run(action: &Action) -> i32 {
    let result = (|| -> Result<()> {
        let root = root()?;
        match action {
            Action::Use { .. } | Action::Subscription { .. } | Action::Recover { .. } => {
                return Err(error("Use the asynchronous connection command."));
            }
            Action::Stage {
                label,
                provider,
                source,
                model,
                credential_key,
                endpoint_key,
                region_key,
            } => {
                let mut bindings = provider.defaults();
                if let Some(key) = credential_key {
                    bindings.credential_key = key.clone();
                }
                if let Some(key) = endpoint_key {
                    bindings.endpoint_key = Some(key.clone());
                }
                if let Some(key) = region_key {
                    bindings.region_key = Some(key.clone());
                }
                stage(
                    &root,
                    &Profile {
                        routing_id: None,
                        label: label.clone(),
                        provider: *provider,
                        source: source.clone(),
                        model: model.clone(),
                        bindings: Some(bindings),
                    },
                )?;
                println!("Connection saved. Codex was not changed or restarted.");
            }
            Action::List => println!("{}", status_default()),
            Action::Verify { label, json } => {
                let report = verify(
                    &load(&root, label)?,
                    &crate::codex_account::store::Paths::resolve()?.home,
                )?;
                if *json {
                    println!(
                        "{}",
                        crate::display::sanitize_untrusted_line(&report.to_string())
                    );
                } else {
                    println!("Offline source and configuration checks passed. ");
                    println!(
                        "No credentials used. No Codex files changed. No switch or model call performed."
                    );
                    for r in report["requirements"].as_array().into_iter().flatten() {
                        if let Some(s) = r.as_str() {
                            println!("{}", crate::display::sanitize_untrusted_line(s));
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "{}",
                crate::display::sanitize_untrusted_line(&e.to_string())
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_and_verify_preserve_state_and_never_copy_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("codex");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "forced_login_method = 'chatgpt'\n",
        )
        .unwrap();
        for file in ["auth.json", ".codex-global-state.json", "state_5.sqlite"] {
            std::fs::write(home.join(file), b"fixture-state").unwrap();
        }
        let dir = home.join("automations/task");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("automation.toml"), "model = 'gpt-5.6-sol'\n").unwrap();
        let source = tmp.path().join("source.env");
        std::fs::write(
            &source,
            "AWS_BEARER_TOKEN_BEDROCK='fixture-secret'\nAWS_REGION=us-east-2\n",
        )
        .unwrap();
        let profile = Profile {
            routing_id: None,
            label: "work".into(),
            provider: Provider::Bedrock,
            source,
            model: None,
            bindings: None,
        };
        let root = tmp.path().join("registry");
        stage(&root, &profile).unwrap();
        let report = verify(&load(&root, "work").unwrap(), &home).unwrap();
        assert_eq!(report["workflow_count"], 1);
        assert_eq!(report["activation_verified"], false);
        assert!(!report.to_string().contains("fixture-secret"));
        assert!(
            !std::fs::read_to_string(root.join("work.json"))
                .unwrap()
                .contains("fixture-secret")
        );
        for file in ["auth.json", ".codex-global-state.json", "state_5.sqlite"] {
            assert_eq!(std::fs::read(home.join(file)).unwrap(), b"fixture-state");
        }
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "forced_login_method = 'chatgpt'\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("automation.toml")).unwrap(),
            "model = 'gpt-5.6-sol'\n"
        );
        assert!(stage(&root, &profile).is_err());
    }
    #[test]
    fn reject_substitution_duplicate_missing_and_invalid_endpoints() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.env");
        for text in [
            "KEY=$(touch /tmp/unwanted)",
            "KEY=`command`",
            "KEY=x\nKEY=y",
            "UNRELATED=value",
            "KEY=\"unterminated",
        ] {
            std::fs::write(&source, text).unwrap();
            assert!(fields(&source, &["KEY"]).is_err());
        }
        std::fs::write(&source,"AZURE_FOUNDRY_API_KEY=fixture-secret\nAZURE_OPENAI_ENDPOINT=https://untrusted.example\n").unwrap();
        let p = Profile {
            routing_id: None,
            label: "azure".into(),
            provider: Provider::Azure,
            source: source.clone(),
            model: None,
            bindings: None,
        };
        assert!(validate_source(&p).is_err());
        std::fs::write(&source,"AZURE_FOUNDRY_API_KEY=fixture-secret\nAZURE_OPENAI_ENDPOINT=https://fixture.openai.azure.com\n").unwrap();
        let r = verify(&p, &tmp.path().join("absent-home")).unwrap();
        assert!(!r.to_string().contains("fixture-secret"));
        assert!(!tmp.path().join("absent-home").exists());
        assert!(path(tmp.path(), "../escape").is_err());
    }
    #[test]
    fn custom_connections_use_configurable_keys_and_no_model_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("custom.env");
        std::fs::write(
            &source,
            "TEAM_TOKEN=fixture-secret\nTEAM_URL=https://gateway.example/v1\n",
        )
        .unwrap();
        let profile = Profile {
            routing_id: None,
            label: "Team gateway".into(),
            provider: Provider::Compatible,
            source,
            model: Some("future-org-model".into()),
            bindings: Some(Bindings {
                credential_key: "TEAM_TOKEN".into(),
                endpoint_key: Some("TEAM_URL".into()),
                region_key: None,
            }),
        };
        let result = verify(&profile, &tmp.path().join("home")).unwrap();
        assert_eq!(result["configured_model"], "future-org-model");
        assert!(result.get("catalog_models").is_none());
        assert_eq!(result["model_access"], "not-checked");
        assert!(!result.to_string().contains("fixture-secret"));
        assert!(
            result["proposed_config"]
                .as_str()
                .unwrap()
                .contains("TEAM_TOKEN")
        );
        let mut malformed = profile.clone();
        malformed.bindings.as_mut().unwrap().credential_key = "not-a-variable".into();
        assert!(validate_source(&malformed).is_err());
    }
    #[test]
    fn legacy_records_keep_their_original_source_mapping() {
        let profile:Profile=serde_json::from_str(r#"{"label":"existing","provider":"azure","source":"/fixture/source.env","model":null}"#).unwrap();
        assert_eq!(profile.bindings().credential_key, "AZURE_FOUNDRY_API_KEY");
        assert_eq!(
            Provider::Azure.defaults().credential_key,
            "AZURE_OPENAI_API_KEY"
        );
        let t = templates();
        assert_eq!(t.as_array().unwrap().len(), 3);
        assert!(!t.to_string().contains("gpt-"));
    }
}
