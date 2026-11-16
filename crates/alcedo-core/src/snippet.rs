use serde::{Deserialize, Serialize};

use crate::commands::ComposeFormat;
use crate::contact::Contact;
use crate::ids::{AccountId, SnippetId};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snippet {
    pub id: SnippetId,
    pub name: String,
    pub format: ComposeFormat,
    pub body: String,
    pub position: i32,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSignature {
    pub account_id: AccountId,
    pub format: ComposeFormat,
    pub body: String,
    #[serde(default)]
    pub inlines: Vec<crate::commands::ComposeInline>,
}

impl std::fmt::Debug for AccountSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSignature")
            .field("account_id", &self.account_id)
            .field("format", &self.format)
            .field("body", &self.body)
            .field("inlines", &format_args!("len={}", self.inlines.len()))
            .finish()
    }
}

const SNIPPET_NAME_MAX: usize = 80;
const SNIPPET_BODY_MAX: usize = 32 * 1024;

impl Snippet {
    pub fn validate(&self) -> Result<(), &'static str> {
        let name = self.name.trim();
        if name.is_empty() || name.len() > SNIPPET_NAME_MAX {
            return Err("Snippet name must be 1–80 characters");
        }
        if self.body.len() > SNIPPET_BODY_MAX {
            return Err("Snippet body is too long");
        }
        Ok(())
    }
}

impl AccountSignature {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.body.len() > SNIPPET_BODY_MAX {
            return Err("Signature body is too long");
        }
        if self.inlines.len() > crate::MAX_COMPOSE_INLINES {
            return Err("Too many inline images (max 10)");
        }
        for inline in &self.inlines {
            if !crate::content_id_is_safe(&inline.content_id) {
                return Err("invalid inline content-id");
            }
            if !crate::filename_is_safe(&inline.filename) {
                return Err("filename not allowed");
            }
        }
        Ok(())
    }
}

fn mailbox_display_part(email: &str) -> &str {
    email.split('@').next().unwrap_or(email)
}

fn first_name_from_display(display: &str, email: &str) -> String {
    let trimmed = display.trim();
    if !trimmed.is_empty()
        && let Some(token) = trimmed.split_whitespace().next()
    {
        return token.to_string();
    }
    let local = mailbox_display_part(email);
    local
        .split('+')
        .next()
        .unwrap_or(local)
        .split('.')
        .next()
        .unwrap_or(local)
        .to_string()
}

/// Expand `{{first_name}}`, `{{full_name}}`, and `{{email}}` in snippet text.
pub fn expand_snippet_placeholders(
    body: &str,
    first_to_email: Option<&str>,
    contact: Option<&Contact>,
) -> String {
    let email = first_to_email.map(str::trim).filter(|s| !s.is_empty());
    let Some(email) = email else {
        return body.to_string();
    };

    let display = contact
        .and_then(|c| {
            let name = c.display_name.trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .unwrap_or_default();

    let full_name = if display.is_empty() {
        mailbox_display_part(email).to_string()
    } else {
        display.clone()
    };
    let first_name = first_name_from_display(&display, email);

    body.replace("{{email}}", email)
        .replace("{{full_name}}", &full_name)
        .replace("{{first_name}}", &first_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn signature_validate_inline_count() {
        use crate::commands::ComposeInline;
        let id = AccountId(Uuid::new_v4());
        let mut inlines = Vec::new();
        for i in 0..10 {
            inlines.push(ComposeInline {
                content_id: format!("img-{i}@example.com"),
                filename: format!("img{i}.png"),
                mime: "image/png".to_string(),
                bytes: vec![1],
            });
        }
        let ok = AccountSignature {
            account_id: id,
            format: ComposeFormat::Plain,
            body: String::new(),
            inlines,
        };
        assert!(ok.validate().is_ok());

        let too_many = AccountSignature {
            inlines: {
                let mut v = ok.inlines.clone();
                v.push(ComposeInline {
                    content_id: "extra@example.com".to_string(),
                    filename: "extra.png".to_string(),
                    mime: "image/png".to_string(),
                    bytes: vec![1],
                });
                v
            },
            ..ok
        };
        assert_eq!(too_many.validate(), Err("Too many inline images (max 10)"));
    }
}
