use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    pub options: Option<Vec<ToolRequestUserInputOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub questions: Vec<ToolRequestUserInputQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputResponse {
    pub answers: HashMap<String, ToolRequestUserInputAnswer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolvedNotification {
    pub thread_id: String,
    pub request_id: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveRequestSource {
    Direct,
    Tui,
}

#[derive(Debug)]
enum PendingResponder {
    Direct(oneshot::Sender<ToolRequestUserInputResponse>),
}

#[derive(Debug, Clone)]
pub struct InteractivePromptSnapshot {
    pub request_id: i64,
    pub source: InteractiveRequestSource,
    pub thread_key: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub question_index: usize,
    pub question: ToolRequestUserInputQuestion,
    pub prompt_message_id: Option<i32>,
    pub awaiting_freeform_text: bool,
}

#[derive(Debug)]
pub enum CompletedInteractiveRequest {
    Direct {
        prompt_message_id: Option<i32>,
        response: ToolRequestUserInputResponse,
        responder: oneshot::Sender<ToolRequestUserInputResponse>,
    },
    Tui {
        thread_key: String,
        request_id: i64,
        prompt_message_id: Option<i32>,
        response: ToolRequestUserInputResponse,
    },
}

#[derive(Debug)]
pub enum InteractiveAdvance {
    Updated(InteractivePromptSnapshot),
    Completed(CompletedInteractiveRequest),
}

#[derive(Debug, Clone)]
pub struct ResolvedInteractiveRequest {
    pub chat_id: i64,
    pub telegram_thread_id: i32,
    pub prompt_message_id: Option<i32>,
}

#[derive(Debug)]
struct PendingInteractiveRequest {
    request_id: i64,
    source: InteractiveRequestSource,
    chat_id: i64,
    telegram_thread_id: i32,
    thread_key: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
    questions: Vec<ToolRequestUserInputQuestion>,
    answers: HashMap<String, ToolRequestUserInputAnswer>,
    current_question_index: usize,
    awaiting_freeform_text: bool,
    prompt_message_id: Option<i32>,
    responder: Option<PendingResponder>,
}

impl PendingInteractiveRequest {
    fn current_snapshot(&self) -> Option<InteractivePromptSnapshot> {
        let question = self.questions.get(self.current_question_index)?.clone();
        Some(InteractivePromptSnapshot {
            request_id: self.request_id,
            source: self.source,
            thread_key: self.thread_key.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            item_id: self.item_id.clone(),
            question_index: self.current_question_index,
            question,
            prompt_message_id: self.prompt_message_id,
            awaiting_freeform_text: self.awaiting_freeform_text,
        })
    }

    fn advance_after_answer(&mut self) -> Option<InteractivePromptSnapshot> {
        self.current_question_index += 1;
        self.awaiting_freeform_text = false;
        self.current_snapshot()
    }

    fn build_response(&self) -> ToolRequestUserInputResponse {
        ToolRequestUserInputResponse {
            answers: self.answers.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InteractiveRequestRegistry {
    inner: Arc<Mutex<HashMap<String, PendingInteractiveRequest>>>,
}

impl InteractiveRequestRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register_direct(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        thread_key: String,
        request_id: i64,
        params: ToolRequestUserInputParams,
        responder: oneshot::Sender<ToolRequestUserInputResponse>,
    ) -> Result<InteractivePromptSnapshot> {
        self.register(
            chat_id,
            telegram_thread_id,
            thread_key,
            request_id,
            params,
            Some(PendingResponder::Direct(responder)),
            InteractiveRequestSource::Direct,
        )
        .await
    }

    pub async fn register_tui(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        thread_key: String,
        request_id: i64,
        params: ToolRequestUserInputParams,
    ) -> Result<InteractivePromptSnapshot> {
        self.register(
            chat_id,
            telegram_thread_id,
            thread_key,
            request_id,
            params,
            None,
            InteractiveRequestSource::Tui,
        )
        .await
    }

    async fn register(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        thread_key: String,
        request_id: i64,
        params: ToolRequestUserInputParams,
        responder: Option<PendingResponder>,
        source: InteractiveRequestSource,
    ) -> Result<InteractivePromptSnapshot> {
        let key = conversation_key(chat_id, telegram_thread_id);
        let request = PendingInteractiveRequest {
            request_id,
            source,
            chat_id,
            telegram_thread_id,
            thread_key,
            thread_id: params.thread_id,
            turn_id: params.turn_id,
            item_id: params.item_id,
            questions: params.questions,
            answers: HashMap::new(),
            current_question_index: 0,
            awaiting_freeform_text: false,
            prompt_message_id: None,
            responder,
        };
        let snapshot = request
            .current_snapshot()
            .context("request_user_input is missing questions")?;
        self.inner.lock().await.insert(key, request);
        Ok(snapshot)
    }

    pub async fn prompt_for(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
    ) -> Option<InteractivePromptSnapshot> {
        self.inner
            .lock()
            .await
            .get(&conversation_key(chat_id, telegram_thread_id))
            .and_then(PendingInteractiveRequest::current_snapshot)
    }

    pub async fn set_prompt_message_id(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        message_id: i32,
    ) {
        if let Some(request) = self
            .inner
            .lock()
            .await
            .get_mut(&conversation_key(chat_id, telegram_thread_id))
        {
            request.prompt_message_id = Some(message_id);
        }
    }

    pub async fn resolve_request_id(
        &self,
        thread_id: &str,
        request_id: &serde_json::Value,
    ) -> Option<ResolvedInteractiveRequest> {
        let request_id = match request_id {
            serde_json::Value::Number(number) => number.as_i64(),
            _ => None,
        }?;
        let mut guard = self.inner.lock().await;
        let key = guard.iter().find_map(|(key, pending)| {
            (pending.thread_id == thread_id && pending.request_id == request_id)
                .then(|| key.clone())
        })?;
        let pending = guard.remove(&key)?;
        Some(ResolvedInteractiveRequest {
            chat_id: pending.chat_id,
            telegram_thread_id: pending.telegram_thread_id,
            prompt_message_id: pending.prompt_message_id,
        })
    }

    pub async fn choose_option(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        request_id: i64,
        option_index: usize,
    ) -> Result<Option<InteractiveAdvance>> {
        let key = conversation_key(chat_id, telegram_thread_id);
        let mut guard = self.inner.lock().await;
        let pending = match guard.get_mut(&key) {
            Some(pending) if pending.request_id == request_id => pending,
            _ => return Ok(None),
        };
        let question = pending
            .questions
            .get(pending.current_question_index)
            .context("interactive question is missing")?;
        let options = question
            .options
            .as_ref()
            .context("interactive question has no options")?;
        if option_index == options.len() {
            pending.awaiting_freeform_text = true;
            return Ok(pending.current_snapshot().map(InteractiveAdvance::Updated));
        }
        let option = options
            .get(option_index)
            .context("interactive option index is out of range")?;
        pending.answers.insert(
            question.id.clone(),
            ToolRequestUserInputAnswer {
                answers: vec![option.label.clone()],
            },
        );
        let next_snapshot = pending.advance_after_answer();
        if let Some(snapshot) = next_snapshot {
            return Ok(Some(InteractiveAdvance::Updated(snapshot)));
        }
        let pending = guard
            .remove(&key)
            .context("interactive request disappeared before completion")?;
        Ok(Some(InteractiveAdvance::Completed(to_completed_request(
            pending,
        ))))
    }

    pub async fn submit_text(
        &self,
        chat_id: i64,
        telegram_thread_id: i32,
        text: String,
    ) -> Result<Option<InteractiveAdvance>> {
        let key = conversation_key(chat_id, telegram_thread_id);
        let mut guard = self.inner.lock().await;
        let pending = match guard.get_mut(&key) {
            Some(pending) => pending,
            None => return Ok(None),
        };
        let question = pending
            .questions
            .get(pending.current_question_index)
            .context("interactive question is missing")?;
        if question.options.is_some() && !pending.awaiting_freeform_text {
            return Ok(None);
        }
        pending.answers.insert(
            question.id.clone(),
            ToolRequestUserInputAnswer {
                answers: vec![text],
            },
        );
        let next_snapshot = pending.advance_after_answer();
        if let Some(snapshot) = next_snapshot {
            return Ok(Some(InteractiveAdvance::Updated(snapshot)));
        }
        let pending = guard
            .remove(&key)
            .context("interactive request disappeared before completion")?;
        Ok(Some(InteractiveAdvance::Completed(to_completed_request(
            pending,
        ))))
    }

    pub async fn clear_conversation(&self, chat_id: i64, telegram_thread_id: i32) {
        self.inner
            .lock()
            .await
            .remove(&conversation_key(chat_id, telegram_thread_id));
    }
}

fn to_completed_request(pending: PendingInteractiveRequest) -> CompletedInteractiveRequest {
    let response = pending.build_response();
    match pending.responder {
        Some(PendingResponder::Direct(responder)) => CompletedInteractiveRequest::Direct {
            prompt_message_id: pending.prompt_message_id,
            response,
            responder,
        },
        None => CompletedInteractiveRequest::Tui {
            thread_key: pending.thread_key,
            request_id: pending.request_id,
            prompt_message_id: pending.prompt_message_id,
            response,
        },
    }
}

fn conversation_key(chat_id: i64, telegram_thread_id: i32) -> String {
    format!("{chat_id}:{telegram_thread_id}")
}
