use moa_proto::{Call, CoreEvent, HelloOk, Patch, SnapshotMsg, StatusOk, UpgradeOk};
use schemars::schema_for;
use serde_json::json;

#[test]
fn protocol_schema() {
    let all = json!({
        "Call": schema_for!(Call),
        "HelloOk": schema_for!(HelloOk),
        "StatusOk": schema_for!(StatusOk),
        "UpgradeOk": schema_for!(UpgradeOk),
        "SnapshotMsg": schema_for!(SnapshotMsg),
        "Patch": schema_for!(Patch),
        "CoreEvent": schema_for!(CoreEvent),
    });
    let text = serde_json::to_string(&all).unwrap();
    // A debug-only op never appears in the published protocol.
    assert!(!text.contains("debug.panic"));
    insta::assert_json_snapshot!(all);
}
