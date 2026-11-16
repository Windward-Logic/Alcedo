//! Messages and list ids for features that live outside the public crates.
//!
//! An extension owns its payload format. The app only routes a message to the
//! extension with the matching name, so these types carry no feature logic.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// A command or event addressed to one extension, by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionMessage {
    pub extension: String,
    pub payload: serde_json::Value,
}

impl ExtensionMessage {
    /// Wrap `payload` for the extension named `extension`.
    pub fn new(extension: &str, payload: &impl Serialize) -> Self {
        Self {
            extension: extension.to_string(),
            payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
        }
    }

    /// The payload as `T`, when this message is for `extension`.
    pub fn decode<T: DeserializeOwned>(&self, extension: &str) -> Option<T> {
        if self.extension != extension {
            return None;
        }
        serde_json::from_value(self.payload.clone()).ok()
    }
}

/// A message list that an extension fills, such as a virtual mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtensionListId(pub Uuid);

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Ping {
        n: u32,
    }

    #[test]
    fn decodes_payload_for_matching_extension() {
        let msg = ExtensionMessage::new("demo", &Ping { n: 7 });
        assert_eq!(msg.decode::<Ping>("demo"), Some(Ping { n: 7 }));
    }

    #[test]
    fn ignores_payload_for_other_extensions() {
        let msg = ExtensionMessage::new("demo", &Ping { n: 7 });
        assert_eq!(msg.decode::<Ping>("other"), None);
    }

    #[test]
    fn mismatched_payload_shape_decodes_to_none() {
        let msg = ExtensionMessage::new("demo", &"text");
        assert_eq!(msg.decode::<Ping>("demo"), None);
    }
}
