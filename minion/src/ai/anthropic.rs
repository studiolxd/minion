//! Anthropic's Messages API.
//!
//! Close enough to Chat Completions to be tempting to fold into
//! [`super::openai_compat`], and different in three ways that make it not
//! worth it: the key goes in `x-api-key` rather than a bearer token, there
//! is a required `anthropic-version` header, and the system prompt is a
//! field of its own instead of the first message. The answer is a list of
//! content blocks, of which Minion reads the text ones.
//!
//! `stop_reason` carries one case worth handling by name: `"refusal"`,
//! where the model declined. That is an answer, not a failure — it comes
//! back as a short Spanish "no puedo con eso" rather than an error the
//! caller has to explain.

use serde::{Deserialize, Serialize};

use super::{post_json, AiError, Backend, Message, HTTP_TIMEOUT};

/// The API version header every request must carry.
pub const API_VERSION: &str = "2023-06-01";

/// Where the requests go, unless `[ai] base_url` says otherwise.
pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1/messages";

/// Used when `[ai] model` is empty.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// Same reasoning as the OpenAI-compatible side: this is read out loud.
const MAX_TOKENS: u32 = 400;

/// What Minion says when the model declines.
const REFUSAL: &str = "Lo siento, no puedo con eso.";

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Turn<'a>>,
}

#[derive(Debug, Serialize)]
struct Turn<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    content: Vec<Block>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// Builds the request body for one exchange. Pure, for the same reason as
/// its OpenAI-compatible counterpart.
pub fn build_request(model: &str, system: &str, history: &[Message]) -> String {
    let messages = history
        .iter()
        .map(|message| Turn { role: message.role, content: &message.content })
        .collect();
    let request = Request { model, max_tokens: MAX_TOKENS, system, messages };
    serde_json::to_string(&request).unwrap_or_default()
}

/// Reads the answer out of a Messages response.
pub fn parse_response(body: &str) -> Result<String, AiError> {
    let response: Response =
        serde_json::from_str(body).map_err(|e| AiError::Parse(format!("{e}: {body}")))?;
    if let Some(error) = response.error {
        return Err(AiError::Backend(error.message));
    }
    if response.stop_reason.as_deref() == Some("refusal") {
        return Ok(REFUSAL.into());
    }
    let text = response
        .content
        .iter()
        .filter(|block| block.kind == "text")
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(AiError::Parse("respuesta sin texto".into()));
    }
    Ok(text)
}

/// A live conversation with the Anthropic API.
pub struct Anthropic {
    url: String,
    model: String,
    key: String,
    history: Vec<Message>,
}

impl Anthropic {
    pub fn new(key: String, model: &str, base_url: &str) -> Self {
        Self {
            url: if base_url.is_empty() { DEFAULT_URL.into() } else { base_url.to_string() },
            model: if model.is_empty() { DEFAULT_MODEL.into() } else { model.to_string() },
            key,
            history: Vec::new(),
        }
    }

    fn headers(&self) -> Vec<String> {
        vec![
            "content-type: application/json".into(),
            format!("anthropic-version: {API_VERSION}"),
            format!("x-api-key: {}", self.key),
        ]
    }

    fn send(&self, system: &str, history: &[Message]) -> Result<String, AiError> {
        let body = build_request(&self.model, system, history);
        parse_response(&post_json(&self.url, &self.headers(), &body, HTTP_TIMEOUT)?)
    }
}

impl Backend for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn ask(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        let mut history = self.history.clone();
        history.push(Message::user(prompt));
        let answer = self.send(system, &history)?;
        history.push(Message::assistant(&answer));
        super::trim_history(&mut history);
        self.history = history;
        Ok(answer)
    }

    fn ask_once(&mut self, prompt: &str, system: &str) -> Result<String, AiError> {
        self.send(system, &[Message::user(prompt)])
    }

    fn forget(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_prompt_is_a_field_not_a_message() {
        let body = build_request(DEFAULT_MODEL, "sé breve", &[Message::user("hola")]);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["system"], "sé breve");
        assert_eq!(parsed["model"], "claude-opus-5");
        assert_eq!(parsed["max_tokens"], 400);
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["messages"][0]["role"], "user");
    }

    #[test]
    fn the_version_header_is_always_sent_and_the_key_goes_in_x_api_key() {
        let backend = Anthropic::new("secreto".into(), "", "");
        let headers = backend.headers();
        assert!(headers.iter().any(|header| header == "anthropic-version: 2023-06-01"));
        assert!(headers.iter().any(|header| header == "x-api-key: secreto"));
        assert_eq!(backend.url, DEFAULT_URL);
    }

    /// A real answer, trimmed of the fields Minion does not read.
    const ANSWER: &str = r#"{
      "id": "msg_01ABC",
      "type": "message",
      "role": "assistant",
      "model": "claude-opus-5",
      "content": [{"type": "text", "text": "El 15 % de 340 es 51."}],
      "stop_reason": "end_turn",
      "stop_sequence": null,
      "usage": {"input_tokens": 40, "output_tokens": 12}
    }"#;

    #[test]
    fn the_answer_is_read() {
        assert_eq!(parse_response(ANSWER).unwrap(), "El 15 % de 340 es 51.");
    }

    #[test]
    fn several_text_blocks_are_joined() {
        let body = r#"{"content":[{"type":"text","text":"Una. "},{"type":"text","text":"Y dos."}],"stop_reason":"end_turn"}"#;
        assert_eq!(parse_response(body).unwrap(), "Una. Y dos.");
    }

    #[test]
    fn a_thinking_block_is_not_part_of_the_answer() {
        let body = r#"{"content":[{"type":"thinking","thinking":"mmm"},{"type":"text","text":"Cincuenta y uno."}],"stop_reason":"end_turn"}"#;
        assert_eq!(parse_response(body).unwrap(), "Cincuenta y uno.");
    }

    #[test]
    fn a_refusal_is_an_answer_not_a_failure() {
        let body = r#"{"content":[],"stop_reason":"refusal"}"#;
        assert_eq!(parse_response(body).unwrap(), REFUSAL);
    }

    #[test]
    fn an_api_error_is_reported_with_its_message() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(parse_response(body), Err(AiError::Backend("invalid x-api-key".into())));
    }

    #[test]
    fn html_from_a_proxy_is_a_parse_error_not_a_panic() {
        assert!(matches!(parse_response("<html>502</html>"), Err(AiError::Parse(_))));
    }
}
