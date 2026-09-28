//! Isolated two-Mac fixtures. No live app, account store, or iCloud directory.
use super::NativeSession;
use super::engine::{Adapter, Counts, Provider, State, error, sync};
use crate::Result;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

const CHAT: &str = "58ef3e95-57d1-4f80-880c-9de6007d9d38";

#[derive(Default)]
struct NativeFixture {
    sessions: BTreeMap<String, NativeSession>,
    fail_before_restore: bool,
    fail_after_restore: bool,
}

impl NativeFixture {
    fn with_text(text: &str) -> Self {
        let session = NativeSession {
            id: CHAT.into(),
            payload: json!({"text": text}),
        };
        Self {
            sessions: BTreeMap::from([(CHAT.into(), session)]),
            ..Self::default()
        }
    }

    fn text(&mut self, text: &str) {
        self.sessions.get_mut(CHAT).unwrap().payload = json!({"text": text});
    }

    fn contents(&self) -> Vec<String> {
        let mut texts: Vec<_> = self
            .sessions
            .values()
            .map(|s| s.payload["text"].as_str().unwrap().into())
            .collect();
        texts.sort();
        texts
    }
}

impl Adapter for NativeFixture {
    fn capture(&mut self) -> Result<Vec<NativeSession>> {
        Ok(self.sessions.values().cloned().collect())
    }

    fn restore(&mut self, session: &NativeSession, target_id: &str) -> Result<()> {
        if std::mem::take(&mut self.fail_before_restore) {
            return Err(error("Synthetic interrupted restore before write"));
        }
        let mut restored = session.clone();
        restored.id = target_id.into();
        self.sessions.insert(target_id.into(), restored);
        if std::mem::take(&mut self.fail_after_restore) {
            return Err(error("Synthetic interrupted restore after write"));
        }
        Ok(())
    }

    fn ready(&self) -> Result<bool> {
        Ok(true)
    }
}

fn pass(
    root: &Path,
    receipt: &Path,
    state: &mut State,
    provider: Provider,
    native: &mut NativeFixture,
) -> Counts {
    sync(root, receipt, state, provider, native).unwrap()
}

#[test]
fn two_macs_converge_concurrent_native_chats_without_repeat_imports() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("initial conversation");
    let mut second_native = NativeFixture::default();
    assert_eq!(
        pass(
            &cloud,
            &first_receipt,
            &mut first,
            Provider::Codex,
            &mut first_native
        )
        .exported,
        1
    );
    assert_eq!(
        pass(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut second_native
        )
        .imported,
        1
    );
    first_native.text("continued on first Mac");
    second_native.text("continued on second Mac");
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert_eq!(
        pass(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut second_native
        )
        .conflicts,
        1
    );
    assert_eq!(
        pass(
            &cloud,
            &first_receipt,
            &mut first,
            Provider::Codex,
            &mut first_native
        )
        .conflicts,
        1
    );
    assert_eq!(first_native.contents(), second_native.contents());
    assert_eq!(first_native.sessions.len(), 2);
    for (receipt, state, native) in [
        (&first_receipt, &mut first, &mut first_native),
        (&second_receipt, &mut second, &mut second_native),
    ] {
        let counts = pass(&cloud, receipt, state, Provider::Codex, native);
        assert_eq!(
            (counts.exported, counts.imported, counts.conflicts),
            (0, 0, 0)
        );
    }
}

#[test]
fn same_native_identity_in_codex_and_claude_stays_separate() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut codex = NativeFixture::with_text("Codex conversation");
    let mut claude = NativeFixture::with_text("Claude conversation");
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut codex,
    );
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Claude,
        &mut claude,
    );
    let mut restored_codex = NativeFixture::default();
    let mut restored_claude = NativeFixture::default();
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut restored_codex,
    );
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Claude,
        &mut restored_claude,
    );
    assert_eq!(restored_codex.contents(), ["Codex conversation"]);
    assert_eq!(restored_claude.contents(), ["Claude conversation"]);
}

#[test]
fn another_provider_cannot_clear_an_interrupted_restore_receipt() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut NativeFixture::with_text("Codex conversation"),
    );
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Claude,
        &mut NativeFixture::with_text("Claude conversation"),
    );
    let mut interrupted = NativeFixture {
        fail_before_restore: true,
        ..NativeFixture::default()
    };
    assert!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut interrupted
        )
        .is_err()
    );
    let pending = serde_json::to_value(&second).unwrap()["pending"]["codex"].clone();
    assert_eq!(pending["provider"], "codex");
    let mut restored_claude = NativeFixture::default();
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Claude,
        &mut restored_claude,
    );
    assert_eq!(restored_claude.contents(), ["Claude conversation"]);
    assert_eq!(
        serde_json::to_value(&second).unwrap()["pending"]["codex"],
        pending
    );
}

#[test]
fn restore_succeeded_before_crash_does_not_create_a_duplicate_chat() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut NativeFixture::with_text("complete conversation"),
    );
    let mut native = NativeFixture {
        fail_after_restore: true,
        ..NativeFixture::default()
    };
    assert!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut native
        )
        .is_err()
    );
    assert_eq!(native.sessions.len(), 1);
    // Reload the receipt to exercise recovery after a process restart.
    second = State::load(&second_receipt).unwrap();
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut native,
    );
    assert_eq!(native.sessions.len(), 1);
    assert_eq!(native.contents(), ["complete conversation"]);
}

#[test]
fn either_mac_can_continue_each_preserved_branch_without_extra_forks() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("initial");
    let mut second_native = NativeFixture::default();
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    first_native.text("first branch");
    second_native.text("second branch");
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    let first_ids: Vec<_> = first_native.sessions.keys().cloned().collect();
    let second_ids: Vec<_> = second_native.sessions.keys().cloned().collect();

    // Continue the other Mac's branch, whose native ID differs on this Mac.
    first_native
        .sessions
        .values_mut()
        .find(|session| session.payload["text"] == "second branch")
        .unwrap()
        .payload = json!({"text": "second branch continued on first Mac"});
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    let advanced = pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    assert_eq!((advanced.imported, advanced.conflicts), (1, 0));
    second_native
        .sessions
        .values_mut()
        .find(|session| session.payload["text"] == "first branch")
        .unwrap()
        .payload = json!({"text": "first branch continued on second Mac"});
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    let advanced = pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert_eq!((advanced.imported, advanced.conflicts), (1, 0));
    assert_eq!(first_native.contents(), second_native.contents());
    assert_eq!(
        first_native.sessions.keys().cloned().collect::<Vec<_>>(),
        first_ids
    );
    assert_eq!(
        second_native.sessions.keys().cloned().collect::<Vec<_>>(),
        second_ids
    );
}

#[test]
fn local_edits_after_interrupted_restore_survive_and_reach_the_other_mac() {
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("remote conversation");
    let mut second_native = NativeFixture {
        fail_after_restore: true,
        ..NativeFixture::default()
    };
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut second_native
        )
        .is_err()
    );
    second_native.text("local edits after interruption");
    second = State::load(&second_receipt).unwrap();
    let recovered = pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    assert_eq!(recovered.conflicts, 1);
    assert_eq!(
        second_native.contents(),
        ["local edits after interruption", "remote conversation"]
    );
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert_eq!(first_native.contents(), second_native.contents());
    assert_eq!(first_native.sessions.len(), 2);
    let repeated = pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    assert_eq!(
        (repeated.imported, repeated.exported, repeated.conflicts),
        (0, 0, 0)
    );
}

#[test]
fn an_app_editing_and_closing_during_cloud_scan_cannot_lose_its_changes() {
    use std::cell::{Cell, RefCell};
    struct ReopenedApp {
        native: RefCell<NativeFixture>,
        ready_calls: Cell<usize>,
    }
    impl Adapter for ReopenedApp {
        fn capture(&mut self) -> Result<Vec<NativeSession>> {
            self.native.get_mut().capture()
        }
        fn restore(&mut self, session: &NativeSession, target: &str) -> Result<()> {
            self.native.get_mut().restore(session, target)
        }
        fn ready(&self) -> Result<bool> {
            let calls = self.ready_calls.get() + 1;
            self.ready_calls.set(calls);
            if calls == 2 {
                self.native
                    .borrow_mut()
                    .text("new local edits made during cloud scan");
            }
            // The app has already quit again by each process check.
            Ok(true)
        }
    }
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("initial");
    let mut second_native = NativeFixture::default();
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    first_native.text("remote continuation");
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    let mut reopened = ReopenedApp {
        native: RefCell::new(second_native),
        ready_calls: Cell::new(0),
    };
    assert!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut reopened
        )
        .is_err()
    );
    assert_eq!(
        reopened.native.borrow().contents(),
        ["new local edits made during cloud scan"]
    );
    let counts = sync(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut reopened,
    )
    .unwrap();
    assert_eq!(counts.conflicts, 1);
    assert_eq!(
        reopened.native.borrow().contents(),
        [
            "new local edits made during cloud scan",
            "remote continuation"
        ]
    );
}

#[test]
fn edits_observed_after_another_import_are_not_mistaken_for_a_clean_replica() {
    struct EditedOtherChat {
        native: NativeFixture,
        edit_after_next_restore: bool,
    }
    impl Adapter for EditedOtherChat {
        fn capture(&mut self) -> Result<Vec<NativeSession>> {
            self.native.capture()
        }
        fn restore(&mut self, session: &NativeSession, target: &str) -> Result<()> {
            self.native.restore(session, target)?;
            if std::mem::take(&mut self.edit_after_next_restore) {
                let other = self
                    .native
                    .sessions
                    .values_mut()
                    .find(|session| session.id != target)
                    .unwrap();
                other.payload = json!({"text": "unexported edit to the other chat"});
            }
            Ok(())
        }
        fn ready(&self) -> Result<bool> {
            Ok(true)
        }
    }
    const SECOND: &str = "b17e2851-1a68-4e7c-820e-53d36018785f";
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("initial first");
    first_native.sessions.insert(
        SECOND.into(),
        NativeSession {
            id: SECOND.into(),
            payload: json!({"text": "initial second"}),
        },
    );
    let mut second_native = NativeFixture::default();
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    pass(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut second_native,
    );
    first_native.text("remote first continuation");
    first_native.sessions.get_mut(SECOND).unwrap().payload =
        json!({"text": "remote second continuation"});
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    let mut edited = EditedOtherChat {
        native: second_native,
        edit_after_next_restore: true,
    };
    let counts = sync(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut edited,
    )
    .unwrap();
    assert_eq!(counts.conflicts, 1);
    assert_eq!(
        edited.native.contents(),
        [
            "remote first continuation",
            "remote second continuation",
            "unexported edit to the other chat"
        ]
    );
}

#[test]
fn app_reopening_during_restore_keeps_pending_receipt_and_exports_its_continuation() {
    struct ReopenedDuringRestore {
        native: NativeFixture,
        reopen_next: bool,
        busy: bool,
    }
    impl Adapter for ReopenedDuringRestore {
        fn capture(&mut self) -> Result<Vec<NativeSession>> {
            self.native.capture()
        }
        fn restore(&mut self, session: &NativeSession, target: &str) -> Result<()> {
            self.native.restore(session, target)?;
            if std::mem::take(&mut self.reopen_next) {
                self.busy = true;
                self.native.sessions.get_mut(target).unwrap().payload =
                    json!({"text": "local continuation after reopening"});
            }
            Ok(())
        }
        fn ready(&self) -> Result<bool> {
            Ok(!self.busy)
        }
    }
    let fixture = tempfile::tempdir().unwrap();
    let cloud = fixture.path().join("icloud");
    let first_receipt = fixture.path().join("first/state.json");
    let second_receipt = fixture.path().join("second/state.json");
    let mut first = State::default();
    let mut second = State::default();
    let mut first_native = NativeFixture::with_text("remote conversation");
    let mut reopened = ReopenedDuringRestore {
        native: NativeFixture::default(),
        reopen_next: true,
        busy: false,
    };
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut reopened
        )
        .is_err()
    );
    assert_eq!(
        reopened.native.contents(),
        ["local continuation after reopening"]
    );
    second = State::load(&second_receipt).unwrap();
    let receipt = serde_json::to_value(&second).unwrap();
    assert_eq!(receipt["pending"]["codex"]["local_id"], CHAT);
    assert!(receipt["replicas"].as_array().unwrap().is_empty());
    assert_eq!(
        sync(
            &cloud,
            &second_receipt,
            &mut second,
            Provider::Codex,
            &mut reopened
        )
        .unwrap()
        .pending,
        1
    );

    reopened.busy = false;
    let recovered = sync(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut reopened,
    )
    .unwrap();
    assert_eq!(recovered.conflicts, 1);
    assert_eq!(
        reopened.native.contents(),
        ["local continuation after reopening", "remote conversation"]
    );
    pass(
        &cloud,
        &first_receipt,
        &mut first,
        Provider::Codex,
        &mut first_native,
    );
    assert_eq!(first_native.contents(), reopened.native.contents());
    let repeated = sync(
        &cloud,
        &second_receipt,
        &mut second,
        Provider::Codex,
        &mut reopened,
    )
    .unwrap();
    assert_eq!(
        (repeated.imported, repeated.exported, repeated.conflicts),
        (0, 0, 0)
    );
}
