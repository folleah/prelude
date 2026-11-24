use std::{future::Future, time::Duration};

pub(crate) async fn retry<T, F: Future<Output = T>>(
    attempt: impl Fn() -> F,
    should_retry: impl Fn(&T) -> bool,
) -> T {
    let mut result = attempt().await;
    for seconds in [1, 2, 3] {
        if !should_retry(&result) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        result = attempt().await;
    }
    result
}

pub(crate) fn client() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    #[cfg(windows)]
    let builder = windows_proxy(builder);
    builder
}

#[cfg(all(test, windows))]
#[test]
#[ignore = "requires access to OpenAI through the current network"]
fn probes_openai_token_endpoint_without_credentials() {
    tauri::async_runtime::block_on(async {
        let response = client()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap()
            .post("https://auth.openai.com/api/accounts/oauth/token")
            .form(&[("grant_type", "authorization_code")])
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            400,
            "{}",
            response.text().await.unwrap()
        );
    });
}

#[cfg(windows)]
fn windows_proxy(mut builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Registry::*;

    fn setting(name: &str, flags: u32) -> Option<Vec<u16>> {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
        let name = wide(name);
        let mut size = 0;
        // Query the byte count, then pass a buffer of that size to the registry API.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                null_mut(),
                null_mut(),
                &mut size,
            )
        };
        if status != 0 || size == 0 || size > 65536 {
            return None;
        }
        let mut value = vec![0u16; (size as usize).div_ceil(2)];
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                null_mut(),
                value.as_mut_ptr().cast(),
                &mut size,
            )
        };
        (status == 0).then_some(value)
    }
    let string = |name| {
        setting(name, RRF_RT_REG_SZ).map(|value| {
            String::from_utf16_lossy(
                &value[..value.iter().position(|ch| *ch == 0).unwrap_or(value.len())],
            )
        })
    };
    if ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
        || !setting("ProxyEnable", RRF_RT_REG_DWORD)
            .is_some_and(|value| value.iter().any(|part| *part != 0))
    {
        return builder;
    }
    let Some(server) = string("ProxyServer") else {
        return builder;
    };
    let bypass = string("ProxyOverride")
        .unwrap_or_default()
        .replace(';', ",");
    let exclusions = format!("localhost,127.0.0.1,::1,{bypass}");
    for entry in server
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (scheme, address) = entry.split_once('=').unwrap_or(("all", entry));
        let url = if address.contains("://") {
            address.to_owned()
        } else {
            format!("http://{address}")
        };
        let proxy = match scheme {
            "all" => reqwest::Proxy::all(&url),
            "http" => reqwest::Proxy::http(&url),
            "https" => reqwest::Proxy::https(&url),
            _ => continue,
        };
        if let Ok(proxy) = proxy {
            builder = builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(&exclusions)));
        }
    }
    builder
}
