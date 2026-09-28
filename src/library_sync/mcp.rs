//! Portable MCP definitions contain binding descriptions, never inline credentials.
use super::{model::Target, skills::error};
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
enum Portable {
    Literal { value: String },
    Binding { slot: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    version: u32,
    transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<Portable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<Portable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<Portable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<Portable>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, Portable>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, Portable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    env_vars: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bearer_env: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    options: BTreeMap<String, Value>,
}

fn binding(slot: impl Into<String>) -> Portable {
    Portable::Binding { slot: slot.into() }
}
fn literal(value: &str) -> Portable {
    Portable::Literal {
        value: value.into(),
    }
}
fn variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn header(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn command(value: &str) -> bool {
    matches!(
        value,
        "npx"
            | "node"
            | "nodejs"
            | "uvx"
            | "uv"
            | "python"
            | "python3"
            | "docker"
            | "deno"
            | "bun"
            | "java"
            | "ruby"
    )
}
fn argument(value: &str) -> bool {
    matches!(
        value,
        "-y" | "--yes" | "-m" | "--stdio" | "--transport=stdio" | "run" | "--rm" | "-i"
    )
}
fn public_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/" | "/mcp" | "/sse" | "/v1/mcp")
        && url.host_str().is_some_and(|host| {
            host.contains('.')
                && !host.contains('_')
                && !host.contains("token")
                && !host.contains("secret")
                && host.split('.').all(|s| s.len() <= 40)
                && host.parse::<std::net::IpAddr>().is_err()
        })
}
fn strings(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) if values.len() <= 256 => values
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| s.len() <= 4096)
                    .map(str::to_owned)
                    .ok_or_else(|| error("unsupported MCP option shape"))
            })
            .collect(),
        _ => Err(error("unsupported MCP option shape")),
    }
}

/// Decode only the supported native subset. Unknown executable helpers and
/// provider-owned connection objects are left under their original updater.
pub fn normalize(native: &Value, target: Target) -> Result<Value> {
    let object = native
        .as_object()
        .ok_or_else(|| error("unsupported MCP definition"))?;
    if object
        .get("enabled")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(error("unsupported MCP enablement policy"));
    }
    let allowed: BTreeSet<&str> = [
        "type",
        "command",
        "args",
        "url",
        "cwd",
        "env",
        "env_vars",
        "headers",
        "http_headers",
        "env_http_headers",
        "bearer_token_env_var",
        "startup_timeout_sec",
        "tool_timeout_sec",
        "enabled_tools",
        "disabled_tools",
        "enabled",
        "required",
    ]
    .into_iter()
    .collect();
    if object.keys().any(|k| !allowed.contains(k.as_str())) {
        return Err(error(
            "this MCP setup has unsupported client-specific options",
        ));
    }
    let transport = if let Some(value) = object.get("type") {
        match value.as_str() {
            Some("stdio") => "stdio",
            Some("http" | "streamable-http") => "http",
            Some("sse") => "sse",
            _ => return Err(error("this MCP transport is unsupported")),
        }
    } else if target == Target::Codex && object.contains_key("url") {
        "http"
    } else {
        "stdio"
    };
    let mut def = Definition {
        version: 1,
        transport: transport.into(),
        command: None,
        args: Vec::new(),
        url: None,
        cwd: None,
        env: BTreeMap::new(),
        headers: BTreeMap::new(),
        env_vars: Vec::new(),
        bearer_env: None,
        options: BTreeMap::new(),
    };
    if transport == "stdio" {
        let cmd = object
            .get("command")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or_else(|| error("MCP launch command is missing or unsupported"))?;
        def.command = Some(if command(cmd) {
            literal(cmd)
        } else {
            binding("command")
        });
        for (i, arg) in strings(object.get("args"))?.into_iter().enumerate() {
            def.args.push(if argument(&arg) {
                literal(&arg)
            } else {
                binding(format!("argument:{i}"))
            });
        }
    } else {
        let url = object
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| error("MCP endpoint is missing"))?;
        def.url = Some(if public_url(url) {
            literal(url)
        } else {
            binding("url")
        });
    }
    if object.contains_key("cwd") {
        def.cwd = Some(binding("cwd"));
    }
    for (field, is_header) in [("env", false), ("headers", true), ("http_headers", true)] {
        if let Some(value) = object.get(field) {
            let entries = value
                .as_object()
                .filter(|m| m.len() <= 128)
                .ok_or_else(|| error("unsupported MCP local bindings"))?;
            for (key, value) in entries {
                if !value.is_string()
                    || if is_header {
                        !header(key)
                    } else {
                        !variable(key)
                    }
                {
                    return Err(error("unsupported MCP local binding name"));
                }
                if is_header {
                    def.headers
                        .insert(key.clone(), binding(format!("header:{key}")));
                } else {
                    def.env.insert(key.clone(), binding(format!("env:{key}")));
                }
            }
        }
    }
    for name in strings(object.get("env_vars"))? {
        if !variable(&name) {
            return Err(error("unsupported environment variable name"));
        }
        def.env_vars.push(name);
    }
    if let Some(value) = object.get("env_http_headers") {
        for (key, value) in value
            .as_object()
            .ok_or_else(|| error("unsupported MCP header references"))?
        {
            let env = value
                .as_str()
                .filter(|s| variable(s))
                .ok_or_else(|| error("unsupported MCP environment reference"))?;
            if !header(key) {
                return Err(error("unsupported MCP header name"));
            }
            // Preserve the required variable name, never read its value here.
            def.headers.insert(
                key.clone(),
                binding(format!("environment-header:{key}:{env}")),
            );
        }
    }
    if let Some(env) = object.get("bearer_token_env_var") {
        def.bearer_env = Some(
            env.as_str()
                .filter(|s| variable(s))
                .ok_or_else(|| error("unsupported MCP credential reference"))?
                .into(),
        );
    }
    for key in [
        "startup_timeout_sec",
        "tool_timeout_sec",
        "enabled_tools",
        "disabled_tools",
        "required",
    ] {
        if let Some(value) = object.get(key) {
            let valid = match key {
                "startup_timeout_sec" | "tool_timeout_sec" => value
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=3600.0).contains(&n)),
                "required" => value.is_boolean(),
                _ => strings(Some(value))?.iter().all(|s| {
                    s.len() <= 100
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                }),
            };
            if !valid {
                return Err(error("unsupported MCP portable option"));
            }
            def.options.insert(key.into(), value.clone());
        }
    }
    serde_json::to_value(def).map_err(Into::into)
}

fn decode(value: &Value) -> Result<Definition> {
    let def: Definition =
        serde_json::from_value(value.clone()).map_err(|_| error("invalid portable MCP schema"))?;
    if def.version != 1
        || !matches!(def.transport.as_str(), "stdio" | "http" | "sse")
        || def.args.len() > 256
        || def.env.len() > 128
        || def.headers.len() > 128
    {
        return Err(error("unsupported portable MCP schema"));
    }
    let check = |value: &Portable, slot: &str, allow: fn(&str) -> bool| -> Result<()> {
        match value {
            Portable::Literal { value } if allow(value) => Ok(()),
            Portable::Binding { slot: s } if s == slot => Ok(()),
            _ => Err(error("unsafe portable MCP value")),
        }
    };
    if def.transport == "stdio" {
        check(
            def.command
                .as_ref()
                .ok_or_else(|| error("missing MCP command"))?,
            "command",
            command,
        )?;
        if def.url.is_some() {
            return Err(error("mixed MCP transports"));
        }
    } else {
        check(
            def.url
                .as_ref()
                .ok_or_else(|| error("missing MCP endpoint"))?,
            "url",
            public_url,
        )?;
        if def.command.is_some() || !def.args.is_empty() {
            return Err(error("mixed MCP transports"));
        }
    }
    for (i, arg) in def.args.iter().enumerate() {
        check(arg, &format!("argument:{i}"), argument)?;
    }
    if let Some(cwd) = &def.cwd {
        check(cwd, "cwd", |_| false)?;
    }
    for (key, value) in &def.env {
        if !variable(key) {
            return Err(error("invalid MCP environment name"));
        }
        check(value, &format!("env:{key}"), |_| false)?;
    }
    for (key, value) in &def.headers {
        if !header(key) {
            return Err(error("invalid MCP header name"));
        }
        match value {
            Portable::Binding { slot } if slot == &format!("header:{key}") => (),
            Portable::Binding { slot }
                if slot
                    .strip_prefix(&format!("environment-header:{key}:"))
                    .is_some_and(variable) => {}
            _ => return Err(error("unsafe portable MCP header")),
        }
    }
    if def.env_vars.iter().any(|s| !variable(s))
        || def.bearer_env.as_ref().is_some_and(|s| !variable(s))
    {
        return Err(error("invalid MCP credential reference"));
    }
    // Reuse normalization's option validation without copying any local values.
    normalize_options(&def.options)?;
    Ok(def)
}
fn normalize_options(options: &BTreeMap<String, Value>) -> Result<()> {
    for (key, value) in options {
        let valid = match key.as_str() {
            "startup_timeout_sec" | "tool_timeout_sec" => value
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=3600.0).contains(&n)),
            "required" => value.is_boolean(),
            "enabled_tools" | "disabled_tools" => strings(Some(value))?.iter().all(|s| {
                s.len() <= 100
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            }),
            _ => false,
        };
        if !valid {
            return Err(error("unsupported portable MCP option"));
        }
    }
    Ok(())
}

pub fn binding_slots(value: &Value) -> Result<Vec<String>> {
    let def = decode(value)?;
    let mut slots = BTreeSet::new();
    for v in def
        .command
        .iter()
        .chain(def.args.iter())
        .chain(def.url.iter())
        .chain(def.cwd.iter())
        .chain(def.env.values())
        .chain(def.headers.values())
    {
        if let Portable::Binding { slot } = v {
            slots.insert(slot.clone());
        }
    }
    Ok(slots.into_iter().collect())
}
/// Observe edits through the installed portable template. Binding values and
/// destination-only credential fields are account-local, so changing them does
/// not create a new cloud definition. Public commands/endpoints/options do.
pub fn observe(native: &Value, target: Target, template: &Value) -> Result<Value> {
    let previous = decode(template)?;
    let normalized = normalize(native, target)?;
    let mut current = decode(&normalized)?;
    if current.transport != previous.transport {
        return Ok(normalized);
    }
    if matches!(previous.command, Some(Portable::Binding { .. })) && current.command.is_some() {
        current.command = previous.command.clone();
    }
    if matches!(previous.url, Some(Portable::Binding { .. })) && current.url.is_some() {
        current.url = previous.url.clone();
    }
    if previous.cwd.is_some() && current.cwd.is_some() {
        current.cwd = previous.cwd.clone();
    }
    for (i, arg) in previous.args.iter().enumerate() {
        if matches!(arg, Portable::Binding { .. }) && i < current.args.len() {
            current.args[i] = arg.clone();
        }
    }
    current.env.retain(|key, _| previous.env.contains_key(key));
    current
        .headers
        .retain(|key, _| previous.headers.contains_key(key));
    for (key, value) in &previous.headers {
        if native
            .get("headers")
            .or_else(|| native.get("http_headers"))
            .and_then(|o| o.get(key))
            .is_some()
            || native
                .get("env_http_headers")
                .and_then(|o| o.get(key))
                .is_some()
        {
            current.headers.insert(key.clone(), value.clone());
        }
    }
    current.env_vars = previous
        .env_vars
        .iter()
        .filter(|name| {
            native
                .get("env_vars")
                .and_then(Value::as_array)
                .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(name.as_str())))
                || native.get("env").and_then(|v| v.get(*name)).is_some()
        })
        .cloned()
        .collect();
    current.bearer_env = if previous.bearer_env.is_some()
        && (native.get("bearer_token_env_var").is_some()
            || native
                .get("headers")
                .or_else(|| native.get("http_headers"))
                .and_then(|v| v.get("Authorization"))
                .is_some())
    {
        previous.bearer_env
    } else {
        None
    };
    serde_json::to_value(current).map_err(Into::into)
}
pub fn requirements(value: &Value) -> Result<Vec<String>> {
    let def = decode(value)?;
    let mut out = Vec::new();
    for slot in binding_slots(value)? {
        out.push(format!("Local binding required: {slot}"));
    }
    for env in &def.env_vars {
        out.push(format!("Environment variable required: {env}"));
    }
    if let Some(env) = def.bearer_env {
        out.push(format!("Credential environment variable required: {env}"));
    }
    if def.transport != "stdio" {
        out.push("Authenticate through the destination app if required; connection status is not verified by sync.".into());
    } else {
        out.push(
            "The destination Mac needs the configured MCP executable and its dependencies.".into(),
        );
    }
    Ok(out)
}

pub struct Rendered {
    pub native: Option<Value>,
    pub requirements: Vec<String>,
    pub remote: bool,
}
/// Bindings contain only `env:NAME` or `path:/absolute/path` local references.
/// Existing destination-native values take precedence and never leave this call.
pub fn render(
    value: &Value,
    target: Target,
    existing: Option<&Value>,
    bindings: &BTreeMap<String, String>,
) -> Result<Rendered> {
    let def = decode(value)?;
    if target == Target::Codex && def.transport == "sse" {
        return Ok(Rendered {
            native: None,
            requirements: vec![
                "Codex requires an HTTP MCP endpoint; this setup declares SSE.".into(),
            ],
            remote: true,
        });
    }
    if target == Target::ClaudeCode && !def.options.is_empty() {
        return Ok(Rendered{native:None,requirements:vec!["This setup uses Codex-specific timeout or tool-selection options; review a Claude-compatible definition.".into()],remote:def.transport!="stdio"});
    }
    let mut missing = Vec::new();
    let resolve = |portable: &Portable,
                   previous: Option<&Value>,
                   missing: &mut Vec<String>|
     -> Option<String> {
        match portable {
            Portable::Literal { value } => Some(value.clone()),
            Portable::Binding { slot } => {
                if let Some(old) = previous.and_then(Value::as_str) {
                    return Some(old.into());
                }
                let resolved = bindings.get(slot).and_then(|binding| {
                    if let Some(name) = binding.strip_prefix("env:").filter(|s| variable(s)) {
                        std::env::var(name).ok().filter(|s| !s.is_empty())
                    } else if (slot == "command" || slot == "cwd" || slot.starts_with("argument:"))
                        && binding.starts_with("path:/")
                    {
                        Some(binding[5..].into())
                    } else {
                        None
                    }
                });
                if resolved.is_none() {
                    missing.push(format!("Set up local binding: {slot}"));
                }
                resolved
            }
        }
    };
    let mut output = json!({});
    let old = existing.unwrap_or(&Value::Null);
    if target != Target::Codex {
        output["type"] = json!(def.transport);
    }
    if let Some(command) = &def.command
        && let Some(cmd) = resolve(command, old.get("command"), &mut missing)
    {
        output["command"] = json!(cmd);
    }
    if !def.args.is_empty() {
        let mut args = Vec::new();
        for (i, arg) in def.args.iter().enumerate() {
            if let Some(v) = resolve(arg, old.get("args").and_then(|a| a.get(i)), &mut missing) {
                args.push(v);
            }
        }
        output["args"] = json!(args);
    }
    if let Some(url) = &def.url
        && let Some(url) = resolve(url, old.get("url"), &mut missing)
    {
        output["url"] = json!(url);
    }
    if let Some(cwd) = &def.cwd {
        if target != Target::Codex {
            return Ok(Rendered {
                native: None,
                requirements: vec![
                    "This setup requires a working directory unsupported by this destination."
                        .into(),
                ],
                remote: false,
            });
        }
        if let Some(cwd) = resolve(cwd, old.get("cwd"), &mut missing) {
            output["cwd"] = json!(cwd);
        }
    }
    for (key, env) in &def.env {
        if let Some(v) = resolve(env, old.get("env").and_then(|o| o.get(key)), &mut missing) {
            output["env"][key] = json!(v);
        }
    }
    for (key, header_value) in &def.headers {
        if let Portable::Binding { slot } = header_value
            && let Some(env) = slot.strip_prefix(&format!("environment-header:{key}:"))
        {
            if target == Target::Codex {
                output["env_http_headers"][key] = json!(env);
            } else {
                output["headers"][key] = json!(format!("${{{env}}}"));
            }
            continue;
        }
        let previous = old
            .get("http_headers")
            .or_else(|| old.get("headers"))
            .and_then(|o| o.get(key));
        if let Some(v) = resolve(header_value, previous, &mut missing) {
            output[if target == Target::Codex {
                "http_headers"
            } else {
                "headers"
            }][key] = json!(v);
        }
    }
    if !def.env_vars.is_empty() {
        if target == Target::Codex {
            output["env_vars"] = json!(def.env_vars);
        } else {
            for env in &def.env_vars {
                output["env"][env] = json!(format!("${{{env}}}"));
            }
        }
    }
    if let Some(env) = &def.bearer_env {
        if target == Target::Codex {
            output["bearer_token_env_var"] = json!(env);
        } else {
            output["headers"]["Authorization"] = json!(format!("Bearer ${{{env}}}"));
        }
    }
    for (k, v) in &def.options {
        output[k] = v.clone();
    }
    let credential_bound = [
        "env",
        "headers",
        "http_headers",
        "env_http_headers",
        "env_vars",
        "bearer_token_env_var",
    ]
    .iter()
    .any(|key| {
        old.get(key)
            .is_some_and(|v| !v.is_null() && v.as_object().is_none_or(|m| !m.is_empty()))
    });
    let endpoint_changed = old.get("url") != output.get("url")
        || old.get("command") != output.get("command")
        || old.get("args") != output.get("args");
    if credential_bound && endpoint_changed {
        return Ok(Rendered{native:None,requirements:vec!["This update changes the server endpoint or launch command. Review its local credentials in the destination app before applying it.".into()],remote:def.transport!="stdio"});
    }
    // Destination-only credential fields remain local even when the originating
    // account did not need them. Unknown executable helpers are never invented.
    for key in ["env", "headers", "http_headers", "env_http_headers"] {
        if let Some(map) = old.get(key).and_then(Value::as_object) {
            for (k, v) in map {
                if output.get(key).and_then(|m| m.get(k)).is_none() {
                    output[key][k] = v.clone();
                }
            }
        }
    }
    for key in ["bearer_token_env_var", "env_vars"] {
        if output.get(key).is_none()
            && let Some(v) = old.get(key)
        {
            output[key] = v.clone();
        }
    }
    if target == Target::Codex
        && let Some(enabled) = old.get("enabled")
    {
        output["enabled"] = enabled.clone();
    }
    if let Some(cmd) = output.get("command").and_then(Value::as_str)
        && !executable_exists(cmd)
    {
        missing
            .push("The MCP executable is not available on this Mac; install it separately.".into());
    }
    Ok(Rendered {
        native: if missing.is_empty() {
            Some(output)
        } else {
            None
        },
        requirements: missing,
        remote: def.transport != "stdio",
    })
}

fn executable_exists(command: &str) -> bool {
    let executable = |path: &std::path::Path| {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if command.contains('/') {
        return executable(std::path::Path::new(command));
    }
    let search = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&search)
        .chain(
            ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
                .map(std::path::PathBuf::from),
        )
        .any(|dir| executable(&dir.join(command)))
}

/// Cowork has its own execution environment. Only a binding-free remote setup
/// can be exported without pretending a host executable exists in that VM.
pub fn cowork_definition(value: &Value) -> Result<Value> {
    let def = decode(value)?;
    if def.transport == "stdio"
        || !binding_slots(value)?.is_empty()
        || !def.env_vars.is_empty()
        || def.bearer_env.is_some()
        || !def.options.is_empty()
    {
        return Err(error(
            "this MCP setup needs local bindings or an unverified Cowork runtime",
        ));
    }
    let rendered = render(value, Target::Cowork, None, &BTreeMap::new())?;
    rendered
        .native
        .ok_or_else(|| error("this MCP setup is not portable to Cowork"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_and_opaque_arguments_never_enter_portable_payload() {
        let native = json!({"command":"/Users/alice/SECRET_COMMAND","args":["--token","SECRET_ARG","-y"],"env":{"API_KEY":"SECRET_ENV"},"http_headers":{"Authorization":"Bearer SECRET_HEADER"}});
        let out = normalize(&native, Target::Codex).unwrap();
        let bytes = out.to_string();
        assert!(!bytes.contains("SECRET"));
        assert_eq!(binding_slots(&out).unwrap().len(), 5);
        let url = normalize(
            &json!({"url":"https://example.com/mcp?token=SECRET_URL"}),
            Target::Codex,
        )
        .unwrap();
        assert!(!url.to_string().contains("SECRET"));
    }
    #[test]
    fn preserve_destination_credentials_and_public_http_roundtrip() {
        let source=normalize(&json!({"url":"https://example.com/mcp","http_headers":{"Authorization":"source-secret"}}),Target::Codex).unwrap();
        let existing = json!({"url":"https://example.com/mcp","headers":{"Authorization":"destination-secret"}});
        let out = render(
            &source,
            Target::ClaudeCode,
            Some(&existing),
            &BTreeMap::new(),
        )
        .unwrap()
        .native
        .unwrap();
        assert_eq!(out["headers"]["Authorization"], "destination-secret");
        assert_eq!(normalize(&out, Target::ClaudeCode).unwrap(), source);
        assert!(
            render(&source, Target::ClaudeCode, None, &BTreeMap::new())
                .unwrap()
                .native
                .is_none()
        );
    }
    #[test]
    fn cowork_only_accepts_binding_free_remote_configs() {
        let safe = normalize(&json!({"url":"https://example.com/mcp"}), Target::Codex).unwrap();
        assert_eq!(
            cowork_definition(&safe).unwrap(),
            json!({"type":"http","url":"https://example.com/mcp"})
        );
        let local = normalize(
            &json!({"command":"node","args":["server.js"]}),
            Target::Codex,
        )
        .unwrap();
        assert!(cowork_definition(&local).is_err());
    }
    #[test]
    fn credentials_are_not_forwarded_to_a_changed_endpoint() {
        let definition =
            normalize(&json!({"url":"https://other.example/mcp"}), Target::Codex).unwrap();
        let existing = json!({"url":"https://original.example/mcp","http_headers":{"Authorization":"PRIVATE_TOKEN"}});
        let rendered = render(
            &definition,
            Target::Codex,
            Some(&existing),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(rendered.native.is_none());
        assert!(!rendered.requirements.join(" ").contains("PRIVATE_TOKEN"));
    }
    #[test]
    fn cross_client_local_binding_projection_does_not_create_cloud_edits() {
        let definition=normalize(&json!({"url":"https://example.com/mcp","bearer_token_env_var":"TOKEN","env_http_headers":{"X-Api-Key":"OTHER_TOKEN"}}),Target::Codex).unwrap();
        let mut actual = render(&definition, Target::ClaudeCode, None, &BTreeMap::new())
            .unwrap()
            .native
            .unwrap();
        assert_eq!(
            observe(&actual, Target::ClaudeCode, &definition).unwrap(),
            definition
        );
        actual["headers"]["Authorization"] = json!("Bearer LOCAL_SECRET");
        actual["headers"]["X-Local-Account"] = json!("ANOTHER_SECRET");
        assert_eq!(
            observe(&actual, Target::ClaudeCode, &definition).unwrap(),
            definition
        );
        actual["url"] = json!("https://edited.example/mcp");
        assert_ne!(
            observe(&actual, Target::ClaudeCode, &definition).unwrap(),
            definition
        );
    }
    #[test]
    fn disabled_native_server_stays_disabled_after_adoption_and_update() {
        let existing = json!({"url":"https://example.com/mcp","enabled":false});
        let definition = normalize(&existing, Target::Codex).unwrap();
        assert!(!definition.to_string().contains("enabled"));
        let adopted = render(
            &definition,
            Target::Codex,
            Some(&existing),
            &BTreeMap::new(),
        )
        .unwrap()
        .native
        .unwrap();
        assert_eq!(adopted["enabled"], false);
        let update =
            normalize(&json!({"url":"https://updated.example/mcp"}), Target::Codex).unwrap();
        assert_eq!(
            render(&update, Target::Codex, Some(&existing), &BTreeMap::new())
                .unwrap()
                .native
                .unwrap()["enabled"],
            false
        );
    }
}
