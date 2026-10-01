use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Codex,
    ClaudeCode,
    Cowork,
}

impl Target {
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
            Self::Cowork => "cowork",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Skill,
    Mcp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SkillFile {
    pub path: String,
    pub content_base64: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    Skill { files: Vec<SkillFile> },
    Mcp { definition: serde_json::Value },
}
impl Content {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Skill { .. } => Kind::Skill,
            Self::Mcp { .. } => Kind::Mcp,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LibraryItem {
    pub id: String,
    pub name: String,
    pub targets: BTreeSet<Target>,
    pub enabled: bool,
    pub deleted: bool,
    pub content: Content,
    #[serde(default)]
    pub requirements: Vec<String>,
}
impl LibraryItem {
    pub fn active(&self) -> bool {
        self.enabled && !self.deleted
    }
}

/// This structure is local inventory only. Its locator is never published.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryCandidate {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub source: Target,
    pub classification: String,
    pub detail: String,
    pub locator: String,
    pub content: Option<Content>,
    #[serde(default)]
    pub requirements: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallStatus {
    Ready,
    WaitingForApp,
    NeedsSetup,
    SignInNeeded,
    Conflict,
    InstallRequired,
    UpdateAvailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyResult {
    pub status: InstallStatus,
    pub detail: String,
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DestinationStatus {
    pub target: Target,
    pub status: InstallStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemStatus {
    pub item: LibraryItem,
    pub revision: String,
    pub conflicts: Vec<String>,
    pub destinations: Vec<DestinationStatus>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunReport {
    pub items: Vec<ItemStatus>,
    pub published: usize,
    pub installed: usize,
    pub conflicts: usize,
    pub pending: usize,
}

/// Native writers must compare the current portable digest to `expected`
/// immediately before replacing anything, in addition to client readiness.
pub trait Adapter {
    /// A scoped run may recover native journals only for this item identity.
    fn limit_recovery_to(&mut self, _item_id: Option<&str>) {}
    fn ready(&self, _target: Target) -> crate::Result<bool> {
        Ok(true)
    }
    fn recover_pending(
        &mut self,
        _target: Target,
        _item: &LibraryItem,
        _locator: &str,
    ) -> crate::Result<()> {
        Ok(())
    }
    fn destinations(&mut self, target: Target, item: &LibraryItem) -> crate::Result<Vec<String>>;
    fn inspect(
        &mut self,
        target: Target,
        item: &LibraryItem,
        locator: &str,
    ) -> crate::Result<Option<Content>>;
    fn apply(
        &mut self,
        target: Target,
        item: &LibraryItem,
        locator: &str,
        expected: Option<&str>,
    ) -> crate::Result<ApplyResult>;
}
