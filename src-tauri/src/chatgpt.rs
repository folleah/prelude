use crate::{
    api_planning,
    chatgpt_storage::{self, Renewal},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{header::HeaderValue, Client, Url};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
    signature,
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[derive(Default)]
pub struct ChatGptSession(Mutex<(u64, Option<HeaderValue>)>);

fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "The random number generator is unavailable.")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

struct BrowserCallback {
    stream: TcpStream,
    grant: Result<(String, String), String>,
}

fn reply(stream: &mut TcpStream, result: &Result<(), String>) {
    let (status, body) = match result {
        Ok(()) => (
            "200 OK",
            "ChatGPT is connected. Return to Prelude.".to_owned(),
        ),
        Err(error) => (
            "400 Bad Request",
            format!("Could not complete sign-in to Prelude.\n{error}"),
        ),
    };
    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}", body.len());
}

fn callback(listener: TcpListener, state: &str) -> Result<BrowserCallback, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        let (stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        let timeout = Some(Duration::from_secs(2));
        stream
            .set_read_timeout(timeout)
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(timeout)
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        use std::io::Read;
        if BufReader::new(&stream)
            .take(8192)
            .read_line(&mut line)
            .is_err()
        {
            continue;
        }
        let mut request = line.split_whitespace();
        if request.next() != Some("GET") {
            continue;
        }
        let Some(path) = request
            .next()
            .filter(|path| path.starts_with("/auth/callback?"))
        else {
            continue;
        };
        let url = Url::parse(&format!("http://127.0.0.1{path}"))
            .map_err(|_| "Invalid response from the browser.")?;
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        if params.get("state").map(|value| value.as_ref()) != Some(state) {
            continue;
        }
        let grant = (|| {
            if params.contains_key("error") {
                return Err("ChatGPT sign-in was canceled or denied.".into());
            }
            let client = params
                .get("client_id")
                .filter(|value| {
                    !value.is_empty()
                        && value.as_ref() != "dynamic_agent_client"
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                })
                .ok_or("OpenAI did not return a registration ID.")?;
            let code = params
                .get("code")
                .filter(|value| !value.is_empty())
                .ok_or("OpenAI did not return a sign-in code.")?;
            Ok((code.to_string(), client.to_string()))
        })();
        return Ok(BrowserCallback { stream, grant });
    }
    Err("ChatGPT sign-in timed out.".into())
}

async fn validate_id_token(
    http: &Client,
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<(), String> {
    let invalid = || "Could not verify the ChatGPT account identity.".to_string();
    let (input, signed) = token.rsplit_once('.').ok_or_else(invalid)?;
    let (header, body) = input.split_once('.').ok_or_else(invalid)?;
    let decode = |value| URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid());
    let header: Value = serde_json::from_slice(&decode(header)?).map_err(|_| invalid())?;
    if header["alg"] != "RS256" || header["kid"].as_str().is_none_or(str::is_empty) {
        return Err(invalid());
    }
    let keys: Value = http
        .get(format!("{ISSUER}/.well-known/jwks.json"))
        .send()
        .await
        .map_err(|_| "Could not retrieve the OpenAI verification keys.".to_string())?
        .error_for_status()
        .map_err(|error| {
            format!(
                "OpenAI rejected the verification key request (HTTP {}).",
                error.status().map_or(0, |status| status.as_u16())
            )
        })?
        .json()
        .await
        .map_err(|_| invalid())?;
    let key = keys["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter()
                .find(|key| key["kid"] == header["kid"] && key["kty"] == "RSA")
        })
        .ok_or_else(invalid)?;
    let n = decode(key["n"].as_str().ok_or_else(invalid)?)?;
    let e = decode(key["e"].as_str().ok_or_else(invalid)?)?;
    signature::RsaPublicKeyComponents { n, e }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            input.as_bytes(),
            &decode(signed)?,
        )
        .map_err(|_| invalid())?;
    let claims: Value = serde_json::from_slice(&decode(body)?).map_err(|_| invalid())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid())?
        .as_secs();
    if claims["iss"] != ISSUER
        || claims["nonce"] != nonce
        || claims["exp"].as_u64().is_none_or(|expiry| expiry <= now)
        || claims["sub"].as_str().is_none_or(str::is_empty)
        || !(claims["aud"] == client_id
            || claims["aud"]
                .as_array()
                .is_some_and(|aud| aud.len() == 1 && aud[0] == client_id))
    {
        return Err(invalid());
    }
    Ok(())
}

#[tauri::command]
pub async fn connect_chatgpt(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, ChatGptSession>,
) -> Result<(), String> {
    let (listener, host, generation) = {
        let mut session = state
            .0
            .lock()
            .map_err(|_| "The ChatGPT session is unavailable.")?;
        session.0 += 1;
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("chatgpt-host-id");
        let host = match fs::read_to_string(&path) {
            Ok(host) => host,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let host = format!("urn:uuid:{}", uuid::Uuid::new_v4());
                fs::write(&path, &host).map_err(|e| e.to_string())?;
                host
            }
            Err(error) => return Err(error.to_string()),
        };
        (
            TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?,
            host,
            session.0,
        )
    };
    let redirect = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr().map_err(|e| e.to_string())?.port()
    );
    let csrf = random()?;
    let verifier = random()?;
    let nonce = random()?;
    let challenge = URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()));
    let url = Url::parse_with_params(
        &format!("{ISSUER}/api/accounts/authorize"),
        [
            ("client_id", "dynamic_agent_client"),
            ("agent_name_hint", "Prelude"),
            ("ext_agent_host_id", &host),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", &csrf),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ],
    )
    .map_err(|_| "Could not create the sign-in link.")?;
    webbrowser::open(url.as_str()).map_err(|_| "Could not open the browser.")?;
    let BrowserCallback { mut stream, grant } =
        tauri::async_runtime::spawn_blocking(move || callback(listener, &csrf))
            .await
            .map_err(|_| "Could not finish waiting for sign-in.")??;
    let result = match grant {
        Ok((code, client_id)) => {
            exchange(
                &state,
                generation,
                &redirect,
                &verifier,
                &nonce,
                &code,
                &client_id,
                &app.path().app_data_dir().map_err(|e| e.to_string())?,
            )
            .await
        }
        Err(error) => Err(error),
    };
    reply(&mut stream, &result);
    let _ = window.set_focus();
    result
}

async fn exchange(
    state: &ChatGptSession,
    generation: u64,
    redirect: &str,
    verifier: &str,
    nonce: &str,
    code: &str,
    client_id: &str,
    storage: &std::path::Path,
) -> Result<(), String> {
    let http = crate::network::client()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "HTTPS client error.")?;
    let response = http
        .post(format!("{ISSUER}/api/accounts/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "The OpenAI sign-in code exchange timed out."
            } else {
                "Could not exchange the sign-in code because the OpenAI authorization server is unavailable."
            }
        })?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let payload = response.json::<Value>().await.ok();
        return Err(token_error(status, payload.as_ref()));
    }
    let token: Value = response
        .json()
        .await
        .map_err(|_| "OpenAI returned an invalid response.")?;
    if !token["scope"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .any(|scope| scope == "chatgpt.tokens.use.direct")
    {
        return Err("The account did not authorize access to its ChatGPT subscription.".into());
    }
    validate_id_token(
        &http,
        token["id_token"]
            .as_str()
            .ok_or("OpenAI did not return an account identity token.")?,
        client_id,
        nonce,
    )
    .await?;
    let authorization = token_header(&token)?;
    let mut session = state
        .0
        .lock()
        .map_err(|_| "The ChatGPT session is unavailable.")?;
    if session.0 != generation {
        return Err("The sign-in attempt was canceled.".into());
    }
    #[cfg(windows)]
    {
        let refresh_token = token["refresh_token"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("OpenAI did not issue a session refresh token.")?;
        chatgpt_storage::save(
            storage,
            &Renewal {
                client_id: client_id.into(),
                refresh_token: refresh_token.into(),
            },
        )?;
    }
    #[cfg(not(windows))]
    let _ = storage;
    session.1 = Some(authorization);
    Ok(())
}

fn token_header(token: &Value) -> Result<HeaderValue, String> {
    let bearer = token["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or("OpenAI did not return an access token.")?;
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| "Invalid OpenAI token.")?;
    authorization.set_sensitive(true);
    Ok(authorization)
}

#[tauri::command]
pub async fn restore_chatgpt(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChatGptSession>,
) -> Result<bool, String> {
    let storage = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let (generation, mut renewal) = {
        let mut session = state
            .0
            .lock()
            .map_err(|_| "The ChatGPT session is unavailable.")?;
        if session.1.is_some() {
            return Ok(true);
        }
        let Some(renewal) = chatgpt_storage::load(&storage)? else {
            return Ok(false);
        };
        session.0 += 1;
        (session.0, renewal)
    };
    let client = crate::network::client()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "HTTPS client error.")?;
    let response = crate::network::retry(
        || {
            client
                .post(format!("{ISSUER}/api/accounts/oauth/token"))
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", renewal.client_id.as_str()),
                    ("refresh_token", renewal.refresh_token.as_str()),
                    ("resource", RESOURCE),
                ])
                .send()
        },
        |result| match result {
            Ok(response) => matches!(response.status().as_u16(), 408 | 429 | 500..=599),
            Err(error) => error.is_connect() || error.is_timeout() || error.is_request(),
        },
    )
    .await
    .map_err(|_| "Could not restore the ChatGPT session over the network.")?;
    let status = response.status();
    let token: Value = response
        .json()
        .await
        .map_err(|_| token_error(status.as_u16(), None))?;
    let mut session = state
        .0
        .lock()
        .map_err(|_| "The ChatGPT session is unavailable.")?;
    if session.0 != generation {
        return Ok(false);
    }
    if !status.is_success() {
        if token["error"] == "invalid_grant"
            || token.pointer("/error/code").and_then(Value::as_str) == Some("invalid_grant")
        {
            chatgpt_storage::delete(&storage)?;
            return Err("The saved ChatGPT session expired. Sign in again.".into());
        }
        return Err(token_error(status.as_u16(), Some(&token)));
    }
    if token["scope"].as_str().is_some_and(|scope| {
        !scope
            .split_whitespace()
            .any(|item| item == "chatgpt.tokens.use.direct")
    }) {
        chatgpt_storage::delete(&storage)?;
        return Err("Access to the ChatGPT subscription was revoked. Sign in again.".into());
    }
    let authorization = token_header(&token)?;
    if let Some(rotated) = token["refresh_token"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        renewal.refresh_token = rotated.into();
        chatgpt_storage::save(&storage, &renewal)?;
    }
    session.1 = Some(authorization);
    Ok(true)
}

fn token_error(status: u16, payload: Option<&Value>) -> String {
    let code = payload
        .and_then(|body| {
            body.pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| body.get("error").and_then(Value::as_str))
        })
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 80
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        });
    match code {
        Some("unsupported_country_region_territory") => format!(
            "OpenAI rejected the code exchange (HTTP {status}, unsupported_country_region_territory). Make sure your browser and Prelude use the same network in a supported region."
        ),
        Some(code) => format!("OpenAI rejected the code exchange (HTTP {status}, {code})."),
        None => format!("OpenAI rejected the code exchange (HTTP {status}, no error code returned)."),
    }
}

pub(crate) fn authorization(state: &ChatGptSession) -> Result<HeaderValue, String> {
    state
        .0
        .lock()
        .map_err(|_| "The ChatGPT session is unavailable.")?
        .1
        .clone()
        .ok_or("Sign in with ChatGPT.".into())
}

#[tauri::command]
pub fn disconnect_chatgpt(
    app: tauri::AppHandle,
    state: tauri::State<'_, ChatGptSession>,
) -> Result<(), String> {
    let mut session = state
        .0
        .lock()
        .map_err(|_| "The ChatGPT session is unavailable.")?;
    session.0 += 1;
    chatgpt_storage::delete(&app.path().app_data_dir().map_err(|e| e.to_string())?)?;
    session.1 = None;
    Ok(())
}

#[tauri::command]
pub async fn test_chatgpt(
    model: String,
    state: tauri::State<'_, ChatGptSession>,
) -> Result<(), String> {
    api_planning::test_responses(&authorization(&state)?, &model).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn callback_accepts_only_matching_state_and_issued_client() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || callback(listener, "expected"));
        let request = |state| {
            let mut stream = TcpStream::connect(address).unwrap();
            let request = format!("GET /auth/callback?state={state}&code=sample&client_id=oaiapp_example HTTP/1.1\r\n\r\n");
            stream.write_all(request.as_bytes()).unwrap();
            stream
        };
        drop(request("wrong"));
        let mut browser = request("expected");
        let mut received = worker.join().unwrap().unwrap();
        assert_eq!(
            received.grant.unwrap(),
            ("sample".into(), "oaiapp_example".into())
        );
        reply(
            &mut received.stream,
            &Err("Test code exchange error.".into()),
        );
        drop(received.stream);
        let mut page = String::new();
        browser.read_to_string(&mut page).unwrap();
        assert!(page.starts_with("HTTP/1.1 400"));
        assert!(page.contains("Test code exchange error."));
        assert!(!page.contains("sample"));
    }
}
