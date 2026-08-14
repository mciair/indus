//! HTTP transports for Indus's compatible interim providers.

use std::{
    collections::{BTreeMap, HashSet},
    io::{BufRead, BufReader},
    time::Duration,
};

use reqwest::{
    StatusCode,
    blocking::{Client, RequestBuilder, Response},
    header::RETRY_AFTER,
};
use serde_json::{Value, json};

use crate::provider::{ModelSelection, ProviderId, ProviderStore};

use super::model::{
    CancellationToken, ModelContent, ModelEvent, ModelMessage, ModelRequest, ModelTransport, Role,
    StopReason, ToolDefinition, TransportError, TransportErrorKind, Usage,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct ProviderTransport {
    client: Client,
}

impl ProviderTransport {
    pub fn new() -> Result<Self, TransportError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("indus/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                TransportError::fatal(format!("Could not initialize provider transport: {error}"))
            })?;
        Ok(Self { client })
    }

    fn credentials(&self) -> Result<(ModelSelection, String), TransportError> {
        let store = ProviderStore::load();
        let selection = store.active_selection().cloned().ok_or_else(|| {
            TransportError::fatal(
                "Select a provider and model with /model before sending a prompt.",
            )
        })?;
        let key = store
            .api_key(selection.provider)
            .map(str::to_owned)
            .ok_or_else(|| {
                TransportError::fatal(format!(
                    "No API key is configured for {}. Open /model to configure it.",
                    selection.provider.name()
                ))
            })?;
        Ok((selection, key))
    }
}

impl Default for ProviderTransport {
    fn default() -> Self {
        Self::new().expect("provider HTTP client should initialize")
    }
}

impl ModelTransport for ProviderTransport {
    fn stream(
        &self,
        request: ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError> {
        cancellation.check()?;
        let (selection, key) = self.credentials()?;
        match selection.provider {
            ProviderId::OpenAi | ProviderId::Groq | ProviderId::OpenRouter => {
                self.stream_openai_compatible(&selection, &key, request, on_event, cancellation)
            }
            ProviderId::Anthropic => {
                self.stream_anthropic(&selection, &key, request, on_event, cancellation)
            }
            ProviderId::Gemini => {
                self.stream_gemini(&selection, &key, request, on_event, cancellation)
            }
        }
    }
}

impl ProviderTransport {
    fn stream_openai_compatible(
        &self,
        selection: &ModelSelection,
        key: &str,
        request: ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError> {
        let body = json!({
            "model": selection.model_id,
            "messages": openai_messages(&request),
            "tools": openai_tools(&request.tools),
            "tool_choice": if request.tools.is_empty() { Value::Null } else { json!("auto") },
            "stream": true,
            "stream_options": { "include_usage": true }
        });
        let mut builder = self
            .client
            .post(format!(
                "{}/chat/completions",
                selection.provider.base_url()
            ))
            .bearer_auth(key)
            .json(&body);
        if selection.provider == ProviderId::OpenRouter {
            builder = builder
                .header("HTTP-Referer", "https://mciair.in")
                .header("X-Title", "Indus");
        }
        let response = send(builder, selection.provider, cancellation)?;
        on_event(ModelEvent::StepStarted)?;

        let mut text_open = false;
        let mut reasoning_open = false;
        let mut tools: BTreeMap<usize, OpenAiToolCall> = BTreeMap::new();
        let mut usage = Usage::default();
        let mut reason = StopReason::Unknown;
        read_sse(response, cancellation, |_, data| {
            if data == "[DONE]" {
                return Ok(());
            }
            let value: Value = serde_json::from_str(data).map_err(|error| {
                TransportError::fatal(format!("Provider returned invalid stream data: {error}"))
            })?;
            if let Some(error) = stream_error(&value) {
                return Err(TransportError::fatal(error));
            }
            if let Some(item) = value.get("usage") {
                usage = openai_usage(item);
            }
            let Some(choice) = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
            else {
                return Ok(());
            };
            if let Some(value) = choice.get("finish_reason").and_then(Value::as_str) {
                reason = stop_reason(value);
            }
            let Some(delta) = choice.get("delta") else {
                return Ok(());
            };
            if let Some(text) = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .or_else(|| delta.get("reasoning").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
            {
                if !reasoning_open {
                    reasoning_open = true;
                    on_event(ModelEvent::ReasoningStarted {
                        id: "reasoning-1".into(),
                    })?;
                }
                on_event(ModelEvent::ReasoningDelta {
                    id: "reasoning-1".into(),
                    text: text.into(),
                })?;
            }
            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                if !text_open {
                    text_open = true;
                    on_event(ModelEvent::TextStarted {
                        id: "text-1".into(),
                    })?;
                }
                on_event(ModelEvent::TextDelta {
                    id: "text-1".into(),
                    text: text.into(),
                })?;
            }
            for item in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let function = item.get("function").unwrap_or(&Value::Null);
                let id = item.get("id").and_then(Value::as_str);
                let name = function.get("name").and_then(Value::as_str);
                let arguments = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let entry = tools.entry(index).or_insert_with(|| OpenAiToolCall {
                    id: id
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("call-{}-{index}", request.step)),
                    name: name.unwrap_or("unknown").to_owned(),
                    input: String::new(),
                    started: false,
                });
                if let Some(id) = id {
                    entry.id = id.to_owned();
                }
                if let Some(name) = name {
                    entry.name = name.to_owned();
                }
                if !entry.started {
                    entry.started = true;
                    on_event(ModelEvent::ToolInputStarted {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                    })?;
                }
                if !arguments.is_empty() {
                    entry.input.push_str(arguments);
                    on_event(ModelEvent::ToolInputDelta {
                        id: entry.id.clone(),
                        text: arguments.into(),
                    })?;
                }
            }
            Ok(())
        })?;

        if reasoning_open {
            on_event(ModelEvent::ReasoningFinished {
                id: "reasoning-1".into(),
            })?;
        }
        if text_open {
            on_event(ModelEvent::TextFinished {
                id: "text-1".into(),
            })?;
        }
        for (_, tool) in tools {
            on_event(ModelEvent::ToolCall {
                id: tool.id,
                name: tool.name,
                input: valid_json_object(tool.input),
            })?;
        }
        on_event(ModelEvent::StepFinished { reason, usage })
    }

    fn stream_anthropic(
        &self,
        selection: &ModelSelection,
        key: &str,
        request: ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError> {
        let body = json!({
            "model": selection.model_id,
            "system": request.system.join("\n\n"),
            "messages": anthropic_messages(&request.messages),
            "tools": anthropic_tools(&request.tools),
            "max_tokens": 16384,
            "stream": true
        });
        let response = send(
            self.client
                .post(format!("{}/messages", selection.provider.base_url()))
                .header("x-api-key", key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body),
            selection.provider,
            cancellation,
        )?;
        on_event(ModelEvent::StepStarted)?;

        let mut blocks: BTreeMap<usize, AnthropicBlock> = BTreeMap::new();
        let mut usage = Usage::default();
        let mut reason = StopReason::Unknown;
        read_sse(response, cancellation, |_, data| {
            let value: Value = serde_json::from_str(data).map_err(|error| {
                TransportError::fatal(format!("Anthropic returned invalid stream data: {error}"))
            })?;
            match value.get("type").and_then(Value::as_str).unwrap_or("") {
                "message_start" => {
                    if let Some(item) = value.pointer("/message/usage") {
                        merge_anthropic_usage(&mut usage, item);
                    }
                }
                "content_block_start" => {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let block = value.get("content_block").unwrap_or(&Value::Null);
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            let id = format!("text-{index}");
                            blocks.insert(index, AnthropicBlock::Text(id.clone()));
                            on_event(ModelEvent::TextStarted { id })?;
                        }
                        "thinking" | "redacted_thinking" => {
                            let id = format!("reasoning-{index}");
                            blocks.insert(index, AnthropicBlock::Reasoning(id.clone()));
                            on_event(ModelEvent::ReasoningStarted { id })?;
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("tool-use")
                                .to_owned();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                                .to_owned();
                            blocks.insert(
                                index,
                                AnthropicBlock::Tool {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: String::new(),
                                },
                            );
                            on_event(ModelEvent::ToolInputStarted { id, name })?;
                        }
                        _ => {}
                    }
                }
                "content_block_delta" => {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let delta = value.get("delta").unwrap_or(&Value::Null);
                    match blocks.get_mut(&index) {
                        Some(AnthropicBlock::Text(id)) => {
                            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                on_event(ModelEvent::TextDelta {
                                    id: id.clone(),
                                    text: text.into(),
                                })?;
                            }
                        }
                        Some(AnthropicBlock::Reasoning(id)) => {
                            if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                                on_event(ModelEvent::ReasoningDelta {
                                    id: id.clone(),
                                    text: text.into(),
                                })?;
                            }
                        }
                        Some(AnthropicBlock::Tool { id, input, .. }) => {
                            if let Some(text) = delta.get("partial_json").and_then(Value::as_str) {
                                input.push_str(text);
                                on_event(ModelEvent::ToolInputDelta {
                                    id: id.clone(),
                                    text: text.into(),
                                })?;
                            }
                        }
                        None => {}
                    }
                }
                "content_block_stop" => {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    if let Some(block) = blocks.remove(&index) {
                        finish_anthropic_block(block, on_event)?;
                    }
                }
                "message_delta" => {
                    if let Some(value) = value.pointer("/delta/stop_reason").and_then(Value::as_str)
                    {
                        reason = stop_reason(value);
                    }
                    if let Some(item) = value.get("usage") {
                        merge_anthropic_usage(&mut usage, item);
                    }
                }
                "error" => {
                    return Err(TransportError::fatal(
                        value
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("Anthropic stream failed"),
                    ));
                }
                _ => {}
            }
            Ok(())
        })?;
        for (_, block) in blocks {
            finish_anthropic_block(block, on_event)?;
        }
        on_event(ModelEvent::StepFinished { reason, usage })
    }

    fn stream_gemini(
        &self,
        selection: &ModelSelection,
        key: &str,
        request: ModelRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
        cancellation: &CancellationToken,
    ) -> Result<(), TransportError> {
        let model = selection
            .model_id
            .strip_prefix("models/")
            .unwrap_or(&selection.model_id);
        let body = json!({
            "systemInstruction": { "parts": [{ "text": request.system.join("\n\n") }] },
            "contents": gemini_messages(&request.messages),
            "tools": [{ "functionDeclarations": gemini_tools(&request.tools) }],
            "generationConfig": { "maxOutputTokens": 16384 }
        });
        let response = send(
            self.client
                .post(format!(
                    "{}/models/{model}:streamGenerateContent?alt=sse",
                    selection.provider.base_url()
                ))
                .header("x-goog-api-key", key)
                .json(&body),
            selection.provider,
            cancellation,
        )?;
        on_event(ModelEvent::StepStarted)?;

        let mut text_open = false;
        let mut reasoning_open = false;
        let mut calls = Vec::new();
        let mut seen_calls = HashSet::new();
        let mut usage = Usage::default();
        let mut reason = StopReason::Unknown;
        read_sse(response, cancellation, |_, data| {
            let value: Value = serde_json::from_str(data).map_err(|error| {
                TransportError::fatal(format!("Gemini returned invalid stream data: {error}"))
            })?;
            if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
                return Err(TransportError::fatal(error));
            }
            if let Some(item) = value.get("usageMetadata") {
                usage = gemini_usage(item);
            }
            let Some(candidate) = value
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
            else {
                return Ok(());
            };
            if let Some(value) = candidate.get("finishReason").and_then(Value::as_str) {
                reason = stop_reason(value);
            }
            for (index, part) in candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        if !reasoning_open {
                            reasoning_open = true;
                            on_event(ModelEvent::ReasoningStarted {
                                id: "reasoning-1".into(),
                            })?;
                        }
                        on_event(ModelEvent::ReasoningDelta {
                            id: "reasoning-1".into(),
                            text: text.into(),
                        })?;
                    } else {
                        if !text_open {
                            text_open = true;
                            on_event(ModelEvent::TextStarted {
                                id: "text-1".into(),
                            })?;
                        }
                        on_event(ModelEvent::TextDelta {
                            id: "text-1".into(),
                            text: text.into(),
                        })?;
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    let input = call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string();
                    let signature = format!("{name}:{input}");
                    if seen_calls.insert(signature) {
                        let id = format!("call-{}-{index}", request.step);
                        on_event(ModelEvent::ToolInputStarted {
                            id: id.clone(),
                            name: name.clone(),
                        })?;
                        on_event(ModelEvent::ToolInputDelta {
                            id: id.clone(),
                            text: input.clone(),
                        })?;
                        calls.push((id, name, input));
                    }
                }
            }
            Ok(())
        })?;
        if reasoning_open {
            on_event(ModelEvent::ReasoningFinished {
                id: "reasoning-1".into(),
            })?;
        }
        if text_open {
            on_event(ModelEvent::TextFinished {
                id: "text-1".into(),
            })?;
        }
        for (id, name, input) in calls {
            on_event(ModelEvent::ToolCall { id, name, input })?;
        }
        on_event(ModelEvent::StepFinished { reason, usage })
    }
}

#[derive(Default)]
struct OpenAiToolCall {
    id: String,
    name: String,
    input: String,
    started: bool,
}

enum AnthropicBlock {
    Text(String),
    Reasoning(String),
    Tool {
        id: String,
        name: String,
        input: String,
    },
}

fn finish_anthropic_block(
    block: AnthropicBlock,
    on_event: &mut dyn FnMut(ModelEvent) -> Result<(), TransportError>,
) -> Result<(), TransportError> {
    match block {
        AnthropicBlock::Text(id) => on_event(ModelEvent::TextFinished { id }),
        AnthropicBlock::Reasoning(id) => on_event(ModelEvent::ReasoningFinished { id }),
        AnthropicBlock::Tool { id, name, input } => on_event(ModelEvent::ToolCall {
            id,
            name,
            input: valid_json_object(input),
        }),
    }
}

fn send(
    builder: RequestBuilder,
    provider: ProviderId,
    cancellation: &CancellationToken,
) -> Result<Response, TransportError> {
    cancellation.check()?;
    let response = builder.send().map_err(|error| {
        if error.is_timeout() || error.is_connect() {
            TransportError {
                kind: TransportErrorKind::Retryable,
                message: format!("Could not reach {}: {error}", provider.name()),
                retry_after_ms: None,
            }
        } else {
            TransportError::fatal(format!("{} request failed: {error}", provider.name()))
        }
    })?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let retry_after_ms = retry_after(&response);
    let body = response.text().unwrap_or_default();
    let message = provider_error_message(provider, status, &body);
    let lower = body.to_ascii_lowercase();
    let kind = if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        TransportErrorKind::Retryable
    } else if status == StatusCode::PAYLOAD_TOO_LARGE
        || lower.contains("context_length")
        || lower.contains("context window")
        || lower.contains("too many tokens")
    {
        TransportErrorKind::ContextOverflow
    } else {
        TransportErrorKind::Fatal
    };
    Err(TransportError {
        kind,
        message,
        retry_after_ms,
    })
}

fn read_sse(
    response: Response,
    cancellation: &CancellationToken,
    mut handle: impl FnMut(&str, &str) -> Result<(), TransportError>,
) -> Result<(), TransportError> {
    let reader = BufReader::new(response);
    let mut event = String::new();
    let mut data = String::new();
    for line in reader.lines() {
        cancellation.check()?;
        let line = line.map_err(|error| TransportError {
            kind: TransportErrorKind::Retryable,
            message: format!("Provider stream was interrupted: {error}"),
            retry_after_ms: None,
        })?;
        if line.is_empty() {
            if !data.is_empty() {
                handle(&event, data.trim_end_matches('\n'))?;
            }
            event.clear();
            data.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
            data.push('\n');
        }
    }
    if !data.is_empty() {
        handle(&event, data.trim_end_matches('\n'))?;
    }
    cancellation.check()
}

fn openai_messages(request: &ModelRequest) -> Vec<Value> {
    let mut output = Vec::new();
    if !request.system.is_empty() {
        output.push(json!({ "role": "system", "content": request.system.join("\n\n") }));
    }
    for message in &request.messages {
        match message.role {
            Role::User => output.push(json!({ "role": "user", "content": text_content(message) })),
            Role::Assistant => {
                let tool_calls: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ModelContent::ToolCall {
                            call_id,
                            name,
                            input,
                        } => Some(json!({
                            "id": call_id,
                            "type": "function",
                            "function": { "name": name, "arguments": input }
                        })),
                        _ => None,
                    })
                    .collect();
                let mut item = json!({ "role": "assistant", "content": text_content(message) });
                if !tool_calls.is_empty() {
                    item["tool_calls"] = json!(tool_calls);
                }
                output.push(item);
            }
            Role::Tool => {
                for part in &message.content {
                    if let ModelContent::ToolResult {
                        call_id,
                        output: result,
                        ..
                    } = part
                    {
                        output.push(
                            json!({ "role": "tool", "tool_call_id": call_id, "content": result }),
                        );
                    }
                }
            }
        }
    }
    output
}

fn openai_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": schema_value(tool)
                }
            })
        })
        .collect()
}

fn anthropic_messages(messages: &[ModelMessage]) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    for message in messages {
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let mut content = Vec::new();
        for part in &message.content {
            match part {
                ModelContent::Text(text) => content.push(json!({ "type": "text", "text": text })),
                ModelContent::ToolCall { call_id, name, input } => content.push(json!({
                    "type": "tool_use", "id": call_id, "name": name,
                    "input": serde_json::from_str::<Value>(input).unwrap_or_else(|_| json!({}))
                })),
                ModelContent::ToolResult { call_id, output: result, is_error, .. } => content.push(json!({
                    "type": "tool_result", "tool_use_id": call_id, "content": result, "is_error": is_error
                })),
            }
        }
        if content.is_empty() {
            continue;
        }
        if let Some(last) = output.last_mut().filter(|last| last["role"] == role) {
            if let Some(items) = last.get_mut("content").and_then(Value::as_array_mut) {
                items.extend(content);
            }
        } else {
            output.push(json!({ "role": role, "content": content }));
        }
    }
    output
}

fn anthropic_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": schema_value(tool)
            })
        })
        .collect()
}

fn gemini_messages(messages: &[ModelMessage]) -> Vec<Value> {
    let mut output = Vec::new();
    for message in messages {
        let role = if message.role == Role::Assistant {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        for part in &message.content {
            match part {
                ModelContent::Text(text) => parts.push(json!({ "text": text })),
                ModelContent::ToolCall { name, input, .. } => parts.push(json!({
                    "functionCall": { "name": name, "args": serde_json::from_str::<Value>(input).unwrap_or_else(|_| json!({})) }
                })),
                ModelContent::ToolResult { name, output: result, is_error, .. } => parts.push(json!({
                    "functionResponse": { "name": name, "response": { "output": result, "is_error": is_error } }
                })),
            }
        }
        if !parts.is_empty() {
            output.push(json!({ "role": role, "parts": parts }));
        }
    }
    output
}

fn gemini_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": schema_value(tool)
            })
        })
        .collect()
}

fn text_content(message: &ModelMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ModelContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn schema_value(tool: &ToolDefinition) -> Value {
    serde_json::from_str(&tool.input_schema).unwrap_or_else(|_| json!({ "type": "object" }))
}

fn valid_json_object(input: String) -> String {
    serde_json::from_str::<Value>(&input)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
        .to_string()
}

fn stop_reason(value: &str) -> StopReason {
    match value.to_ascii_lowercase().as_str() {
        "stop" | "end_turn" | "stop_sequence" => StopReason::Stop,
        "tool_calls" | "tool_use" | "function_call" => StopReason::ToolCalls,
        "length" | "max_tokens" | "max_tokens_reached" => StopReason::Length,
        _ => StopReason::Unknown,
    }
}

fn openai_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: value
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
    }
}

fn merge_anthropic_usage(usage: &mut Usage, value: &Value) {
    usage.input_tokens = usage.input_tokens.max(
        value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    usage.output_tokens = usage.output_tokens.max(
        value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    usage.cache_read_tokens = usage.cache_read_tokens.max(
        value
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    usage.cache_write_tokens = usage.cache_write_tokens.max(
        value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
}

fn gemini_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: value
            .get("thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: value
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
    }
}

fn stream_error(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn retry_after(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1000))
}

fn provider_error_message(provider: ProviderId, status: StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_owned()
        });
    format!("{} returned {}: {detail}", provider.name(), status.as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_projection_preserves_tool_call_and_result_ids() {
        let request = ModelRequest {
            system: vec!["system".into()],
            messages: vec![
                ModelMessage {
                    role: Role::Assistant,
                    content: vec![ModelContent::ToolCall {
                        call_id: "call-1".into(),
                        name: "read".into(),
                        input: "{\"path\":\"a\"}".into(),
                    }],
                },
                ModelMessage {
                    role: Role::Tool,
                    content: vec![ModelContent::ToolResult {
                        call_id: "call-1".into(),
                        name: "read".into(),
                        output: "body".into(),
                        is_error: false,
                    }],
                },
            ],
            tools: Vec::new(),
            step: 1,
        };
        let messages = openai_messages(&request);
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[2]["tool_call_id"], "call-1");
    }

    #[test]
    fn malformed_tool_arguments_become_an_empty_object() {
        assert_eq!(valid_json_object("{".into()), "{}");
    }

    #[test]
    fn provider_errors_do_not_echo_response_bodies() {
        let message = provider_error_message(
            ProviderId::OpenAi,
            StatusCode::UNAUTHORIZED,
            "secret response",
        );
        assert!(!message.contains("secret response"));
    }
}
