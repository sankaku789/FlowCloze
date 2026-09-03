use crate::compose::ComposeError;
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct ChatResponse {
    pub(super) choices: Vec<Choice>,
}

#[derive(Deserialize)]
pub(super) struct Choice {
    pub(super) message: Message,
}

#[derive(Deserialize)]
pub(super) struct Message {
    #[serde(default)]
    pub(super) content: Option<MessageContent>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ContentPart {
    Text(String),
    Object {
        #[serde(default)]
        text: Option<String>,
    },
}

pub(super) fn extract_text(response: ChatResponse) -> Result<String, ComposeError> {
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or(ComposeError::EmptyResponse)?;

    let text = match content {
        MessageContent::Text(text) => text,
        MessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text),
                ContentPart::Object { text } => text,
            })
            .collect::<Vec<_>>()
            .join(""),
    };

    if text.trim().is_empty() {
        Err(ComposeError::EmptyResponse)
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_string_and_parts_content() {
        let plain: ChatResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"hello"}}]}"#).unwrap();
        assert_eq!(extract_text(plain).unwrap(), "hello");

        let parts: ChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":[{"type":"text","text":"hel"},{"text":"lo"}]}}]}"#,
        )
        .unwrap();
        assert_eq!(extract_text(parts).unwrap(), "hello");
    }
}
