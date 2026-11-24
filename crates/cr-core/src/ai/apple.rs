//! On-device Apple Intelligence integration via macOS 26+ FoundationModels.

use serde::{Deserialize, Serialize};

use super::{ChatReply, Message, Role};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppleAvailability {
    Available,
    DeviceNotEligible,
    NotEnabled,
    ModelNotReady,
    UnsupportedOs,
    SdkUnavailable,
    UnsupportedPlatform,
    Unavailable { reason: String },
}

impl AppleAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Available => "Ready · On-device Apple Intelligence (private & offline)".into(),
            Self::DeviceNotEligible => {
                "This Mac does not support Apple Intelligence (requires Apple Silicon).".into()
            }
            Self::NotEnabled => {
                "Apple Intelligence is turned off. Enable it in System Settings → Apple Intelligence & Siri."
                    .into()
            }
            Self::ModelNotReady => {
                "Apple Intelligence model assets are still downloading on this Mac.".into()
            }
            Self::UnsupportedOs => "Requires macOS 26 or newer with Apple Intelligence.".into(),
            Self::SdkUnavailable => {
                "This build was compiled without the macOS 26 SDK; rebuild with Xcode 26+ to enable Apple Intelligence."
                    .into()
            }
            Self::UnsupportedPlatform => {
                "Apple Intelligence is only available on macOS 26+ (Apple Silicon).".into()
            }
            Self::Unavailable { reason } => reason.clone(),
        }
    }
}

#[cfg_attr(not(all(target_os = "macos", apple_intelligence_sdk)), allow(dead_code))]
pub(crate) fn split_system_and_prompt(messages: &[Message]) -> (String, String) {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    for msg in messages {
        let content = msg.content.trim();
        if content.is_empty() {
            continue;
        }
        match msg.role {
            Role::System => system_parts.push(content),
            Role::User => turns.push((Role::User, content)),
            Role::Assistant => turns.push((Role::Assistant, content)),
        }
    }
    let system = system_parts.join("\n\n");
    let prompt = if turns.len() == 1 && matches!(turns[0].0, Role::User) {
        turns[0].1.to_owned()
    } else {
        turns
            .into_iter()
            .map(|(role, content)| match role {
                Role::User => format!("User: {content}"),
                Role::Assistant => format!("Assistant: {content}"),
                Role::System => content.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    (system, prompt)
}

#[cfg(all(target_os = "macos", apple_intelligence_sdk))]
mod native {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int, c_void};

    use super::{split_system_and_prompt, AppleAvailability};
    use crate::ai::{ChatReply, Message};
    use crate::{Error, Result};

    extern "C" {
        fn cr_apple_ai_string_free(ptr: *mut c_char);
        fn cr_apple_ai_availability(reason_out: *mut *mut c_char) -> c_int;
        fn cr_apple_ai_complete(
            system: *const c_char,
            prompt: *const c_char,
            out_text: *mut *mut c_char,
            out_error: *mut *mut c_char,
        ) -> c_int;
        fn cr_apple_ai_stream(
            system: *const c_char,
            prompt: *const c_char,
            on_delta: extern "C" fn(*mut c_void, *const c_char) -> c_int,
            ctx: *mut c_void,
            out_error: *mut *mut c_char,
        ) -> c_int;
    }

    unsafe fn take_c_string(ptr: *mut c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        cr_apple_ai_string_free(ptr);
        Some(text)
    }

    pub fn availability() -> AppleAvailability {
        let mut reason: *mut c_char = std::ptr::null_mut();
        let code = unsafe { cr_apple_ai_availability(&mut reason) };
        let reason_text = unsafe { take_c_string(reason) }.unwrap_or_default();
        match code {
            0 => AppleAvailability::Available,
            1 => AppleAvailability::DeviceNotEligible,
            2 => AppleAvailability::NotEnabled,
            3 => AppleAvailability::ModelNotReady,
            4 => AppleAvailability::UnsupportedOs,
            _ => AppleAvailability::Unavailable {
                reason: if reason_text.is_empty() {
                    "Apple Intelligence is unavailable on this Mac.".into()
                } else {
                    reason_text
                },
            },
        }
    }

    pub fn complete(messages: &[Message]) -> Result<String> {
        let avail = availability();
        if !avail.is_available() {
            return Err(Error::Other(avail.summary()));
        }
        let (system, prompt) = split_system_and_prompt(messages);
        if prompt.trim().is_empty() {
            return Err(Error::Other("Enter a message first".into()));
        }
        let c_system = CString::new(system.replace('\0', " "))
            .map_err(|_| Error::Other("Apple Intelligence: invalid system prompt".into()))?;
        let c_prompt = CString::new(prompt.replace('\0', " "))
            .map_err(|_| Error::Other("Apple Intelligence: invalid prompt".into()))?;
        let mut out_text: *mut c_char = std::ptr::null_mut();
        let mut out_err: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cr_apple_ai_complete(
                c_system.as_ptr(),
                c_prompt.as_ptr(),
                &mut out_text,
                &mut out_err,
            )
        };
        let text = unsafe { take_c_string(out_text) };
        let err = unsafe { take_c_string(out_err) };
        if code == 0 {
            text.map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    Error::Other("Apple Intelligence: the model did not return text".into())
                })
        } else {
            Err(Error::Other(
                err.unwrap_or_else(|| "Apple Intelligence request failed".into()),
            ))
        }
    }

    struct StreamState<'a, F: FnMut(&str) -> Result<()>> {
        delta: &'a mut F,
        reply: String,
        error: Option<Error>,
    }

    extern "C" fn on_delta<F: FnMut(&str) -> Result<()>>(
        ctx: *mut c_void,
        chunk: *const c_char,
    ) -> c_int {
        if ctx.is_null() || chunk.is_null() {
            return 0;
        }
        let state = unsafe { &mut *(ctx as *mut StreamState<'_, F>) };
        let text = unsafe { CStr::from_ptr(chunk) }.to_string_lossy();
        if let Err(err) = (state.delta)(&text) {
            state.error = Some(err);
            return 1;
        }
        state.reply.push_str(&text);
        0
    }

    pub fn chat_stream(
        messages: &[Message],
        mut delta: impl FnMut(&str) -> Result<()>,
    ) -> Result<ChatReply> {
        let avail = availability();
        if !avail.is_available() {
            return Err(Error::Other(avail.summary()));
        }
        let (system, prompt) = split_system_and_prompt(messages);
        if prompt.trim().is_empty() {
            return Err(Error::Other("Enter a message first".into()));
        }
        let c_system = CString::new(system.replace('\0', " "))
            .map_err(|_| Error::Other("Apple Intelligence: invalid system prompt".into()))?;
        let c_prompt = CString::new(prompt.replace('\0', " "))
            .map_err(|_| Error::Other("Apple Intelligence: invalid prompt".into()))?;
        let mut state = StreamState {
            delta: &mut delta,
            reply: String::new(),
            error: None,
        };
        let mut out_err: *mut c_char = std::ptr::null_mut();
        let code = unsafe {
            cr_apple_ai_stream(
                c_system.as_ptr(),
                c_prompt.as_ptr(),
                on_delta::<_>,
                (&mut state as *mut StreamState<'_, _>).cast(),
                &mut out_err,
            )
        };
        let err = unsafe { take_c_string(out_err) };
        if let Some(cb_err) = state.error {
            return Err(cb_err);
        }
        if code != 0 {
            return Err(Error::Other(
                err.unwrap_or_else(|| "Apple Intelligence streaming failed".into()),
            ));
        }
        if state.reply.trim().is_empty() {
            return Err(Error::Other(
                "Apple Intelligence: the model did not return text".into(),
            ));
        }
        Ok(ChatReply {
            text: state.reply,
            model: Some("apple/on-device".into()),
            usage: None,
        })
    }
}

pub fn availability() -> AppleAvailability {
    #[cfg(all(target_os = "macos", apple_intelligence_sdk))]
    {
        native::availability()
    }
    #[cfg(all(target_os = "macos", not(apple_intelligence_sdk)))]
    {
        AppleAvailability::SdkUnavailable
    }
    #[cfg(not(target_os = "macos"))]
    {
        AppleAvailability::UnsupportedPlatform
    }
}

pub fn complete(messages: &[Message]) -> Result<String> {
    #[cfg(all(target_os = "macos", apple_intelligence_sdk))]
    {
        native::complete(messages)
    }
    #[cfg(not(all(target_os = "macos", apple_intelligence_sdk)))]
    {
        let _ = messages;
        Err(Error::Other(availability().summary()))
    }
}

pub fn chat_stream(
    messages: &[Message],
    delta: impl FnMut(&str) -> Result<()>,
) -> Result<ChatReply> {
    #[cfg(all(target_os = "macos", apple_intelligence_sdk))]
    {
        native::chat_stream(messages, delta)
    }
    #[cfg(not(all(target_os = "macos", apple_intelligence_sdk)))]
    {
        let _ = (messages, delta);
        Err(Error::Other(availability().summary()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_single_and_multi_turn_messages() {
        let single = vec![
            Message::new(Role::System, "Be concise."),
            Message::new(Role::User, "What is this book about?"),
        ];
        let (sys, prompt) = split_system_and_prompt(&single);
        assert_eq!(sys, "Be concise.");
        assert_eq!(prompt, "What is this book about?");

        let multi = vec![
            Message::new(Role::System, "Sys 1"),
            Message::new(Role::User, "Hello"),
            Message::new(Role::Assistant, "Hi there"),
            Message::new(Role::User, "Next question"),
        ];
        let (sys, prompt) = split_system_and_prompt(&multi);
        assert_eq!(sys, "Sys 1");
        assert!(prompt.contains("User: Hello"));
        assert!(prompt.contains("Assistant: Hi there"));
        assert!(prompt.contains("User: Next question"));
    }

    #[test]
    fn availability_summary_is_descriptive() {
        assert!(AppleAvailability::Available.is_available());
        assert!(!AppleAvailability::NotEnabled.is_available());
        assert!(AppleAvailability::NotEnabled
            .summary()
            .contains("System Settings"));
        assert!(AppleAvailability::UnsupportedPlatform
            .summary()
            .contains("macOS 26+"));
    }
}
