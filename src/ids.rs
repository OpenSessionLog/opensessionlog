use uuid::{uuid, Uuid};

/// Project-wide namespace for UUIDv5 derivations.
/// Changing this value invalidates all previously-derived IDs, so it must never change.
const NAMESPACE_OSL: Uuid = uuid!("01925792-4a7c-4e6f-9d6f-1a2b3c4d5e6f");

pub fn session_id(source: &str, native_id: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE_OSL, format!("{source}:{native_id}").as_bytes())
}

pub fn message_id(session_id: Uuid, event_uuid: &str) -> Uuid {
    Uuid::new_v5(
        &NAMESPACE_OSL,
        format!("{session_id}:{event_uuid}").as_bytes(),
    )
}

pub fn tool_call_id(session_id: Uuid, call_id: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE_OSL, format!("{session_id}:{call_id}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic() {
        let a = session_id("claude", "sess-1");
        let b = session_id("claude", "sess-1");
        assert_eq!(a, b);
    }

    #[test]
    fn same_native_id_different_source_is_different() {
        let a = session_id("claude", "sess-1");
        let b = session_id("codex", "sess-1");
        assert_ne!(a, b);
    }
}
