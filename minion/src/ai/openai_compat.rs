//! OpenAI-compatible Chat Completions.
//!
//! One shape of request serves nearly every provider worth pointing Minion
//! at, including the two that run on this machine: `POST <base>/chat/completions`
//! with a bearer token, a list of `{role, content}` messages, and an answer
//! at `choices[0].message.content`. The presets below are only a base URL,
//! a default model and whether a key is needed — anything not listed can
//! still be reached by naming the nearest preset and overriding
//! `[ai] base_url`.
//!
//! Google's Gemini is here rather than in a module of its own because it
//! now serves this exact protocol at
//! `generativelanguage.googleapis.com/v1beta/openai`; Anthropic does not,
//! and lives in [`super::anthropic`].

use serde::{Deserialize, Serialize};

use super::{post_json, AiError, Backend, Message, HTTP_TIMEOUT};

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// Reads `GET {base_url}/models`'s `data[].id` — every provider here
/// answers the same shape, whatever it actually offers by way of a
/// friendlier name.
pub fn parse_models_response(body: &str) -> Result<Vec<String>, AiError> {
    let response: ModelsResponse =
        serde_json::from_str(body).map_err(|e| AiError::Parse(format!("{e}: {body}")))?;
    if let Some(error) = response.error {
        return Err(AiError::Backend(error.message));
    }
    Ok(response.data.into_iter().map(|entry| entry.id).collect())
}

/// A provider that speaks Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// The `[ai] backend` value.
    pub name: &'static str,
    /// Everything before `/chat/completions`.
    pub base_url: &'static str,
    /// Used when `[ai] model` is empty.
    pub model: &'static str,
    /// The two local servers answer without one.
    pub needs_key: bool,
}

/// The providers Minion knows how to reach without being told a URL.
///
/// The models are the small, fast ones on purpose: this answers a spoken
/// question in a menu-bar app, where two seconds is already a long silence.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "openai",
        base_url: "https://api.openai.com/v1",
        model: "gpt-4o-mini",
        needs_key: true,
    },
    Preset {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        needs_key: true,
    },
    Preset {
        name: "mistral",
        base_url: "https://api.mistral.ai/v1",
        model: "mistral-small-latest",
        needs_key: true,
    },
    Preset {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1",
        model: "llama-3.3-70b-versatile",
        needs_key: true,
    },
    Preset {
        name: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        model: "openai/gpt-4o-mini",
        needs_key: true,
    },
    Preset {
        name: "xai",
        base_url: "https://api.x.ai/v1",
        model: "grok-3-mini",
        needs_key: true,
    },
    Preset {
        name: "gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        model: "gemini-2.5-flash",
        needs_key: true,
    },
    Preset {
        name: "ollama",
        base_url: "http://localhost:11434/v1",
        model: "llama3.2",
        needs_key: false,
    },
    Preset {
        name: "lmstudio",
        base_url: "http://localhost:1234/v1",
        model: "local-model",
        needs_key: false,
    },
];

/// The preset that `[ai] backend` names, if it names one.
pub fn preset(name: &str) -> Option<Preset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

/// Answers to fit in one spoken reply. Enough for a couple of sentences
/// and a little slack; not enough to pay for an essay nobody asked for.
const MAX_TOKENS: u32 = 400;

/// Low, not zero: a voice assistant should give the same answer to the
/// same question, but a flat zero makes some models repeat themselves.
const TEMPERATURE: f32 = 0.2;

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: Vec<Turn<'a>>,
    max_tokens: u32,
    temperature: f32,
    /// Minion reads a whole answer at a time and then speaks it; there is
    /// nothing to do with a stream.
    stream: bool,
}

#[derive(Debug, Serialize)]
struct Turn<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<Reply>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// Builds the request body for one exchange.
///
/// Pure, so the shape can be checked against a fixture rather than against
/// a provider that charges per look.
pub fn build_request(model: &str, system: &str, history: &[Message]) -> String {
    let mut messages = vec![Turn { role: "system", content: system }];
    for message in history {
        messages.push(Turn { role: message.role, content: &message.content });
    }
    let request =
        Request { model, messages, max_tokens: MAX_TOKENS, temperature: TEMPERATURE, stream: false };
    serde_json::to_string(&request).unwrap_or_default()
}

/// Reads the answer out of a Chat Completions response.
pub fn parse_response(body: &str) -> Result<String, AiError> {
    let response: Response =
        serde_json::from_str(body).map_err(|e| AiError::Parse(format!("{e}: {body}")))?;
    if let Some(error) = response.error {
        return Err(AiError::Backend(error.message));
    }
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AiError::Parse("la respuesta no traía ninguna opción".into()))?;
    let text = choice
        .message
        .and_then(|message| message.content)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        // A provider that refuses answers with an empty message and a
        // finish reason saying why; without this it would look like a
        // parse failure rather than a "no".
        let reason = choice.finish_reason.unwrap_or_else(|| "sin texto".into());
        if reason == "content_filter" {
            return Ok("Lo siento, no puedo con eso.".into());
        }
        return Err(AiError::Parse(format!("respuesta vacía ({reason})")));
    }
    Ok(text)
}

/// A live conversation with one OpenAI-compatible provider.
pub struct OpenAiCompatible {
    name: String,
    url: String,
    model: String,
    key: String,
    history: Vec<Message>,
}

impl OpenAiCompatible {
    /// `base_url` overrides the preset's, for a provider not listed or a
    /// proxy in front of one that is.
    pub fn new(preset: Preset, key: String, model: &str, base_url: &str) -> Self {
        let base = if base_url.is_empty() { preset.base_url } else { base_url };
        let model = if model.is_empty() { preset.model } else { model };
        Self {
            name: preset.name.to_string(),
            url: format!("{}/chat/completions", base.trim_end_matches('/')),
            model: model.to_string(),
            key,
            history: Vec::new(),
        }
    }

    fn headers(&self) -> Vec<String> {
        let mut headers = vec!["content-type: application/json".to_string()];
        if !self.key.is_empty() {
            headers.push(format!("authorization: Bearer {}", self.key));
        }
        headers
    }

    fn send(&self, system: &str, history: &[Message]) -> Result<String, AiError> {
        let body = build_request(&self.model, system, history);
        parse_response(&post_json(&self.url, &self.headers(), &body, HTTP_TIMEOUT)?)
    }
}

impl Backend for OpenAiCompatible {
    fn name(&self) -> &str {
        &self.name
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
    fn every_preset_has_a_usable_url_and_model() {
        for preset in PRESETS {
            assert!(preset.base_url.starts_with("http"), "{}", preset.name);
            assert!(!preset.model.is_empty(), "{}", preset.name);
        }
    }

    #[test]
    fn the_local_servers_are_the_ones_that_need_no_key() {
        let keyless: Vec<&str> =
            PRESETS.iter().filter(|preset| !preset.needs_key).map(|preset| preset.name).collect();
        assert_eq!(keyless, vec!["ollama", "lmstudio"]);
    }

    #[test]
    fn the_url_is_the_preset_unless_one_is_given() {
        let backend = OpenAiCompatible::new(preset("openai").unwrap(), "k".into(), "", "");
        assert_eq!(backend.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(backend.model, "gpt-4o-mini");

        let overridden =
            OpenAiCompatible::new(preset("openai").unwrap(), "k".into(), "m", "http://proxy/v1/");
        assert_eq!(overridden.url, "http://proxy/v1/chat/completions");
        assert_eq!(overridden.model, "m");
    }

    #[test]
    fn the_key_never_appears_without_a_key() {
        let backend = OpenAiCompatible::new(preset("ollama").unwrap(), String::new(), "", "");
        assert_eq!(backend.headers(), vec!["content-type: application/json".to_string()]);
    }

    #[test]
    fn the_request_carries_the_system_prompt_first() {
        let body = build_request("gpt-4o-mini", "sé breve", &[Message::user("hola")]);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["model"], "gpt-4o-mini");
        assert_eq!(parsed["messages"][0]["role"], "system");
        assert_eq!(parsed["messages"][0]["content"], "sé breve");
        assert_eq!(parsed["messages"][1]["role"], "user");
        assert_eq!(parsed["messages"][1]["content"], "hola");
        assert_eq!(parsed["stream"], false);
    }

    #[test]
    fn the_whole_history_is_sent() {
        let history =
            vec![Message::user("una"), Message::assistant("dos"), Message::user("tres")];
        let body = build_request("m", "s", &history);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 4);
        assert_eq!(parsed["messages"][2]["role"], "assistant");
    }

    /// A real answer, trimmed of the fields Minion does not read.
    const ANSWER: &str = r#"{
      "id": "chatcmpl-123",
      "object": "chat.completion",
      "model": "gpt-4o-mini",
      "choices": [{
        "index": 0,
        "message": {"role": "assistant", "content": "El 15 % de 340 es 51.\n"},
        "finish_reason": "stop"
      }],
      "usage": {"prompt_tokens": 40, "completion_tokens": 9, "total_tokens": 49}
    }"#;

    #[test]
    fn the_answer_is_read_and_trimmed() {
        assert_eq!(parse_response(ANSWER).unwrap(), "El 15 % de 340 es 51.");
    }

    #[test]
    fn an_api_error_is_reported_with_its_message() {
        let body = r#"{"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}}"#;
        assert_eq!(
            parse_response(body),
            Err(AiError::Backend("Incorrect API key provided".into()))
        );
    }

    #[test]
    fn a_content_filter_becomes_a_polite_no() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"content_filter"}]}"#;
        assert_eq!(parse_response(body).unwrap(), "Lo siento, no puedo con eso.");
    }

    #[test]
    fn a_response_with_no_choices_is_a_parse_error() {
        assert!(matches!(parse_response(r#"{"choices":[]}"#), Err(AiError::Parse(_))));
    }

    #[test]
    fn html_from_a_proxy_is_a_parse_error_not_a_panic() {
        assert!(matches!(parse_response("<html>502</html>"), Err(AiError::Parse(_))));
    }

    /// A real `/v1/models` answer, trimmed of the fields Minion does not
    /// read.
    const MODELS: &str = r#"{
      "object": "list",
      "data": [
        {"id": "gpt-4o", "object": "model", "created": 1715367049, "owned_by": "system"},
        {"id": "gpt-4o-mini", "object": "model", "created": 1721172741, "owned_by": "system"},
        {"id": "o4-mini", "object": "model", "created": 1740000000, "owned_by": "system"}
      ]
    }"#;

    #[test]
    fn model_ids_are_read_from_the_list() {
        assert_eq!(
            parse_models_response(MODELS).unwrap(),
            vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string(), "o4-mini".to_string()]
        );
    }

    #[test]
    fn a_models_error_is_reported_with_its_message() {
        let body = r#"{"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}}"#;
        assert_eq!(
            parse_models_response(body),
            Err(AiError::Backend("Incorrect API key provided".into()))
        );
    }

    #[test]
    fn an_empty_models_list_is_not_an_error() {
        assert_eq!(parse_models_response(r#"{"data":[]}"#).unwrap(), Vec::<String>::new());
    }
}
