#[derive(Debug)]
struct ClaimPrepareRequest {
    id: String,
    requested_changes: Value,
}

#[derive(Debug)]
struct BattlePrepareRequest {
    id: Option<String>,
    requested_changes: Value,
}

#[derive(Debug)]
struct BattleExecuteRequest {
    id: Option<String>,
    requested_changes: Value,
    reason: String,
}

impl BattleExecuteRequest {
    fn as_value(&self) -> Value {
        json!({
            "target": {
                "type": "battle",
                "id": self.id,
                "kind": if self.id.is_some() { "existing" } else { "new" },
            },
            "requested_changes": self.requested_changes,
            "reason": self.reason,
            "confirmed": true,
        })
    }
}

#[derive(Debug)]
struct UserPrepareRequest {
    id: String,
    requested_changes: Value,
}

#[derive(Debug)]
struct UserExecuteRequest {
    id: String,
    requested_changes: Value,
    reason: String,
    self_change_confirmed: bool,
}

impl UserExecuteRequest {
    fn as_value(&self) -> Value {
        json!({
            "target": {
                "type": "user",
                "id": self.id,
            },
            "requested_changes": self.requested_changes,
            "reason": self.reason,
            "confirmed": true,
            "self_change_confirmed": self.self_change_confirmed,
        })
    }
}

