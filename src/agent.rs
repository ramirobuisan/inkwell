//! The assistant: a chat with the translation model that works on the open comic through
//! tools, as the original's agent does (koharu-agent: get_project, get_page, edit, …).
//! It changes the comic the way the person does, so each edit is one undo step. The model
//! runs on the same OpenAI-compatible server as translation (tool calling).
//!
//! The model talks in a background thread; tool calls are answered by the window, which
//! owns the project, through a channel.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    thread,
    time::Duration,
};

use eframe::egui;
use serde_json::{json, Value};

use crate::{ai, settings::Settings};

/// At most this many model calls answer one message (the original caps a run's turns).
pub const MAX_TURNS: usize = 8;

pub enum ToMain {
    /// A tool call for the window to answer.
    Call { name: String, args: Value, reply: Sender<String> },
    /// A message to keep in the conversation (assistant turns, tool results).
    Keep(Value),
    Status(String),
    Done,
    Failed(String),
}

pub struct Agent {
    pub rx: Receiver<ToMain>,
    pub cancel: Arc<AtomicBool>,
}

fn system_prompt(outline: &str) -> String {
    format!(
        "You are the assistant inside Inkwell, a manga translation app. You work on the comic that is open, only through your tools:\n\
- get_project: the pages, their sizes, and how many text boxes each has.\n\
- get_page: one page's text boxes in reading order, with their original text and translation.\n\
- show_page: shows a page to the person.\n\
- edit: changes, adds or deletes text boxes on one page, as one undo step the person can take back.\n\
Pages are numbered from 1. Read before you change anything. Keep translations natural and short enough for speech bubbles. Answer briefly, in the person's language.\n\n\
The comic now: {outline}"
    )
}

fn tools() -> Value {
    json!([
        {"type": "function", "function": {
            "name": "get_project",
            "description": "The comic's pages in order, each with its size, its number of text boxes and how many are translated. Only reads.",
            "parameters": {"type": "object", "properties": {}}
        }},
        {"type": "function", "function": {
            "name": "get_page",
            "description": "One page's text boxes in reading order: id, position and size in page pixels, original text, translation, visible. Only reads.",
            "parameters": {"type": "object", "properties": {"page": {"type": "integer", "description": "Page number, from 1."}}, "required": ["page"]}
        }},
        {"type": "function", "function": {
            "name": "show_page",
            "description": "Shows a page in the editor.",
            "parameters": {"type": "object", "properties": {"page": {"type": "integer"}}, "required": ["page"]}
        }},
        {"type": "function", "function": {
            "name": "edit",
            "description": "Changes one page's text boxes as one undo step: set a box's translation or original text, hide or show it, delete it, or add a new box.",
            "parameters": {"type": "object", "properties": {
                "page": {"type": "integer"},
                "changes": {"type": "array", "items": {"type": "object", "properties": {
                    "id": {"type": "integer"},
                    "translation": {"type": "string"},
                    "original": {"type": "string"},
                    "visible": {"type": "boolean"},
                    "delete": {"type": "boolean"}
                }, "required": ["id"]}},
                "add": {"type": "array", "items": {"type": "object", "properties": {
                    "x": {"type": "number"}, "y": {"type": "number"}, "w": {"type": "number"}, "h": {"type": "number"},
                    "translation": {"type": "string"}, "original": {"type": "string"}
                }, "required": ["x", "y", "w", "h"]}}
            }, "required": ["page"]}
        }}
    ])
}

/// Answers `user` with the model, calling tools as it asks, after `history`.
pub fn start(ctx: egui::Context, settings: Settings, history: Vec<Value>, outline: String, user: String) -> Agent {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    thread::Builder::new()
        .name("assistant".into())
        .spawn(move || {
            let send = |m: ToMain| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            if let Err(e) = ai::prepare_server(&settings, |label, _| send(ToMain::Status(label))) {
                send(ToMain::Failed(e));
                return;
            }
            let mut messages = vec![json!({"role": "system", "content": system_prompt(&outline)})];
            messages.extend(history);
            let user_msg = json!({"role": "user", "content": user});
            messages.push(user_msg.clone());
            send(ToMain::Keep(user_msg));
            for _ in 0..MAX_TURNS {
                if stop.load(Ordering::Relaxed) {
                    send(ToMain::Done);
                    return;
                }
                send(ToMain::Status("Thinking…".into()));
                let reply = match call(&settings, &messages) {
                    Ok(r) => r,
                    Err(e) => {
                        send(ToMain::Failed(e));
                        return;
                    }
                };
                let calls = reply["tool_calls"].as_array().cloned().unwrap_or_default();
                let mut kept = json!({"role": "assistant", "content": clean(reply["content"].as_str().unwrap_or(""))});
                if !calls.is_empty() {
                    kept["tool_calls"] = Value::Array(calls.clone());
                }
                messages.push(kept.clone());
                send(ToMain::Keep(kept));
                if calls.is_empty() {
                    send(ToMain::Done);
                    return;
                }
                for (n, c) in calls.iter().enumerate() {
                    let name = c["function"]["name"].as_str().unwrap_or("").to_owned();
                    let args = match &c["function"]["arguments"] {
                        Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                        other => other.clone(),
                    };
                    send(ToMain::Status(format!("Using {name}…")));
                    let (rtx, rrx) = channel();
                    send(ToMain::Call { name: name.clone(), args, reply: rtx });
                    let answer = rrx
                        .recv_timeout(Duration::from_secs(30))
                        .unwrap_or_else(|_| json!({"error": "the app didn't answer"}).to_string());
                    let id = c["id"].as_str().map(str::to_owned).unwrap_or_else(|| format!("call_{n}"));
                    let result = json!({"role": "tool", "tool_call_id": id, "name": name, "content": answer});
                    messages.push(result.clone());
                    send(ToMain::Keep(result));
                }
            }
            send(ToMain::Failed(format!(
                "Stopped after {MAX_TURNS} steps. Ask again to continue."
            )));
        })
        .expect("start the assistant");
    Agent { rx, cancel }
}

/// A reply without the model's thinking.
pub fn clean(text: &str) -> String {
    match text.rfind("</think>") {
        Some(i) => text[i + 8..].trim().to_owned(),
        None => text.trim().to_owned(),
    }
}

fn call(s: &Settings, messages: &[Value]) -> Result<Value, String> {
    let body = json!({"model": s.model, "messages": messages, "tools": tools(), "stream": false});
    let mut req = ureq::post(&ai::chat_url(&s.endpoint))
        .timeout(Duration::from_secs(600))
        .set("Content-Type", "application/json");
    if !s.api_key.trim().is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", s.api_key.trim()));
    }
    let resp: Value = req
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let body = r.into_string().unwrap_or_default();
                if code == 404 && body.contains("not found") {
                    ai::missing_model_help(&s.model)
                } else if body.contains("does not support tools") {
                    format!("The model '{}' can't use tools. Pick one that can (for example qwen3:4b) in the Setup tab.", s.model)
                } else {
                    format!("The server answered {code}: {body}")
                }
            }
            other => format!("Can't reach the translation server at {} ({other}).", s.endpoint),
        })?
        .into_json()
        .map_err(|e| format!("The server's answer isn't JSON: {e}"))?;
    Ok(resp["choices"][0]["message"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_described_and_thinking_is_dropped() {
        let t = tools();
        let names: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["get_project", "get_page", "show_page", "edit"]);
        assert_eq!(clean("<think>hmm</think>\nDone."), "Done.");
        assert!(system_prompt("2 pages").contains("2 pages"));
    }
}
