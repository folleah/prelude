use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Gemini,
    Kiro,
}

impl Provider {
    fn executable(self) -> &'static str {
        match self {
            Self::Github => "copilot",
            Self::Gemini => "gemini",
            Self::Kiro => "kiro-cli",
        }
    }

    fn command(self) -> Result<Command, String> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let name = self.executable();
        let names = if cfg!(windows) {
            vec![format!("{name}.exe"), format!("{name}.cmd")]
        } else {
            vec![name.to_owned()]
        };
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|path| path.is_file())
            .map(Command::new)
            .ok_or_else(|| {
                format!("{name} is not on PATH. Install the official CLI and restart Prelude.")
            })
    }
}

#[tauri::command]
pub fn open_acp_login(provider: Provider) -> Result<(), String> {
    let mut command = provider.command()?;
    command.current_dir(std::env::temp_dir());
    match provider {
        Provider::Github => {
            command.args(["login", "--web-flow"]);
        }
        Provider::Gemini => {}
        Provider::Kiro => {
            command.arg("login");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x10);
    }
    let mut terminal = command.spawn().map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let _ = terminal.wait();
    });
    Ok(())
}

#[tauri::command]
pub async fn check_acp_connection(provider: Provider, probe: bool) -> Result<(), String> {
    if probe {
        return run(
            provider,
            Some("Reply with the single word READY. Do not use tools.".into()),
        )
        .await
        .map(|_| ());
    }
    crate::network::retry(
        || run(provider, None),
        |result| {
            result.as_ref().is_err_and(|error| {
                let detail = error.to_ascii_lowercase();
                [
                    "timed out",
                    "timeout",
                    "network error",
                    "fetch failed",
                    "connection reset",
                    "connection refused",
                    "econnreset",
                    "econnrefused",
                    "enotfound",
                    "eai_again",
                ]
                .iter()
                .any(|pattern| detail.contains(pattern))
            })
        },
    )
    .await
    .map(|_| ())
}

pub async fn complete(provider: Provider, messages: Vec<Value>) -> Result<String, String> {
    let prompt = messages
        .iter()
        .map(|message| {
            let label = if message["role"] == "system" {
                "Instructions"
            } else {
                "Input"
            };
            format!(
                "{label}:\n{}",
                message["content"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    run(
        provider,
        Some(format!(
            "Respond with text only. Do not use tools.\n\n{prompt}"
        )),
    )
    .await
}

async fn run(provider: Provider, prompt: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let directory = std::env::temp_dir().join(format!("prelude-acp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
        let result = session(provider, &directory, prompt);
        let _ = std::fs::remove_dir_all(directory);
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

fn session(
    provider: Provider,
    directory: &PathBuf,
    prompt: Option<String>,
) -> Result<String, String> {
    let mut command = provider.command()?;
    match provider {
        Provider::Github => {
            command.args([
                "--acp",
                "--stdio",
                "--no-custom-instructions",
                "--available-tools=prelude_no_tools",
            ]);
        }
        Provider::Gemini => {
            command.args([
                "--acp",
                "--approval-mode",
                "default",
                "--extensions",
                "none",
            ]);
        }
        Provider::Kiro => {
            command.arg("acp");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let cwd = directory.to_str().ok_or("Invalid ACP session directory.")?;
    let mut rpc = Rpc::start(command, directory)?;
    let initialized = rpc.call(
        1,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "prelude", "version": env!("CARGO_PKG_VERSION")}
        }),
    )?;
    if initialized["protocolVersion"] != 1 {
        return Err("The provider does not support Prelude's ACP protocol version.".into());
    }
    let session = rpc.call(2, "session/new", json!({"cwd": cwd, "mcpServers": []}))?;
    let id = session["sessionId"]
        .as_str()
        .ok_or("The provider returned no ACP session ID.")?;
    if let Some(prompt) = prompt {
        let result = rpc.call(
            3,
            "session/prompt",
            json!({
                "sessionId": id,
                "prompt": [{"type": "text", "text": prompt}]
            }),
        )?;
        if result["stopReason"] != "end_turn" {
            return Err(format!(
                "The provider stopped before completing the request: {}",
                result["stopReason"]
            ));
        }
        if rpc.answer.trim().is_empty() {
            return Err("The provider returned no text.".into());
        }
    }
    Ok(std::mem::take(&mut rpc.answer))
}

struct Rpc {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<String>,
    answer: String,
}

impl Rpc {
    fn start(mut command: Command, directory: &PathBuf) -> Result<Self, String> {
        let mut child = command
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take();
        let mut output = BufReader::new(child.stdout.take().expect("piped ACP output"));
        let (sender, receiver) = mpsc::sync_channel(16);
        std::thread::spawn(move || loop {
            let mut line = String::new();
            match output.by_ref().take(1024 * 1024).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if !line.ends_with('\n') => break,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            output: receiver,
            answer: String::new(),
        })
    }

    fn send(&mut self, message: Value) -> Result<(), String> {
        let input = self.input.as_mut().expect("piped ACP input");
        writeln!(input, "{message}").map_err(|error| format!("ACP write failed: {error}"))
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(if id == 3 { 180 } else { 20 });
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let line = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| {
                    format!("ACP {method}: {error}. Check that the CLI is current and signed in.")
                })?;
            let message: Value = serde_json::from_str(&line)
                .map_err(|_| "The provider returned an invalid or oversized ACP message.")?;
            if let Some(method) = message["method"].as_str() {
                if let Some(request_id) = message.get("id") {
                    self.send(if method == "session/request_permission" {
                        json!({"jsonrpc": "2.0", "id": request_id, "result": {"outcome": {"outcome": "cancelled"}}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "Method unavailable in Prelude"}})
                    })?;
                } else if method == "session/update"
                    && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                {
                    if let Some(text) = message
                        .pointer("/params/update/content/text")
                        .and_then(Value::as_str)
                    {
                        self.answer.push_str(text);
                        if self.answer.len() > 1024 * 1024 {
                            return Err("The provider response is too large.".into());
                        }
                    }
                }
            } else if message["id"] == id {
                if let Some(error) = message.get("error") {
                    let detail = error
                        .pointer("/data/message")
                        .and_then(Value::as_str)
                        .or_else(|| error["message"].as_str())
                        .unwrap_or("Unknown error");
                    return Err(format!("ACP {method}: {detail}"));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "Missing ACP result.".into());
            }
        }
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
