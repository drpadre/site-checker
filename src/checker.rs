use std::net::IpAddr;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::{redirect, Client};
use url::Url;

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct CheckResult {
    pub up: bool,
    pub detail: String,
    pub elapsed_ms: u64,
}

pub fn build_client(timeout_secs: u64) -> Result<Client> {
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (compatible; SiteCheckerBot/1.0)")
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.stop();
            }
            let host = attempt.url().host_str().unwrap_or("");
            if is_forbidden_host(host) {
                return attempt.error("редирект на внутренний адрес запрещён");
            }
            attempt.follow()
        }))
        .build()?;
    Ok(client)
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xff00) == 0xff00
        }
    }
}

fn is_forbidden_host(host: &str) -> bool {
    let h = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || h.ends_with(".internal") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(ip) => is_private_ip(ip),
        Err(_) => false,
    }
}

/// Нормализует URL и запрещает адреса внутренней сети (SSRF-защита).
pub async fn validate_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("пустой URL");
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).context("некорректный URL")?;
    match url.scheme() {
        "http" | "https" => {}
        other => bail!("поддерживаются только http и https (получено: {other})"),
    }
    let host = url.host_str().context("в URL нет хоста")?.to_owned();
    if is_forbidden_host(&host) {
        bail!("локальные и внутренние адреса запрещены");
    }
    if matches!(url.host(), Some(url::Host::Domain(_))) {
        let port = url.port_or_known_default().unwrap_or(443);
        if let Ok(addrs) = tokio::net::lookup_host((host.as_str(), port)).await {
            for addr in addrs {
                if is_private_ip(addr.ip()) {
                    bail!("домен {host} указывает на внутренний адрес — это запрещено");
                }
            }
        }
        // Если DNS сейчас не отвечает — разрешаем добавить: сайт может быть недоступен,
        // планировщик сообщит об ошибке DNS при проверке.
    }
    url.set_fragment(None);
    Ok(url.into())
}

fn describe_error(e: &reqwest::Error) -> String {
    let root = {
        let mut src: &dyn std::error::Error = e;
        while let Some(next) = src.source() {
            src = next;
        }
        src.to_string()
    };
    if e.is_timeout() {
        "таймаут запроса".to_owned()
    } else if e.is_connect() {
        format!("ошибка соединения: {root}")
    } else if e.is_redirect() {
        "слишком много перенаправлений (или редирект на внутренний адрес)".to_owned()
    } else if e.is_decode() {
        format!("ошибка чтения ответа: {root}")
    } else {
        root
    }
}

pub async fn check(client: &Client, url: &str, search_text: Option<&str>) -> CheckResult {
    let start = Instant::now();
    let fail = |detail: String| CheckResult {
        up: false,
        detail,
        elapsed_ms: start.elapsed().as_millis() as u64,
    };

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => return fail(describe_error(&e)),
    };
    let status = resp.status();
    if !status.is_success() {
        return fail(format!("HTTP {}", status.as_u16()));
    }
    let encoding = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| {
            ct.split(';')
                .find_map(|p| p.trim().strip_prefix("charset="))
        })
        .and_then(|label| encoding_rs::Encoding::for_label(label.trim_matches('"').as_bytes()));

    let mut body: Vec<u8> = Vec::new();
    let mut resp = resp;
    let read = async {
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_BODY_BYTES {
                break;
            }
        }
        Ok::<_, reqwest::Error>(())
    }
    .await;
    if let Err(e) = read {
        return fail(describe_error(&e));
    }

    let elapsed_ms = start.elapsed().as_millis() as u64;
    match search_text.filter(|t| !t.is_empty()) {
        Some(needle) => {
            let text = match encoding {
                Some(enc) if enc != encoding_rs::UTF_8 => enc.decode(&body).0,
                _ => String::from_utf8_lossy(&body),
            };
            if text.contains(needle) {
                CheckResult {
                    up: true,
                    detail: format!("HTTP {}, текст «{needle}» найден", status.as_u16()),
                    elapsed_ms,
                }
            } else {
                CheckResult {
                    up: false,
                    detail: format!("HTTP {}, но текст «{needle}» не найден на странице", status.as_u16()),
                    elapsed_ms,
                }
            }
        }
        None => CheckResult {
            up: true,
            detail: format!("HTTP {}", status.as_u16()),
            elapsed_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_server(response: impl Into<Vec<u8>> + Send + 'static) -> String {
        let bytes = response.into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(&bytes).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/")
    }

    fn client() -> Client {
        build_client(5).unwrap()
    }

    #[tokio::test]
    async fn ok_with_text_found() {
        let url = spawn_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
        )
        .await;
        let res = check(&client(), &url, Some("world")).await;
        assert!(res.up, "{}", res.detail);
    }

    #[tokio::test]
    async fn ok_but_text_missing_is_down() {
        let url = spawn_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
        )
        .await;
        let res = check(&client(), &url, Some("missing")).await;
        assert!(!res.up);
        assert!(res.detail.contains("не найден"));
    }

    #[tokio::test]
    async fn decodes_windows1251_body() {
        let (body, _, _) = encoding_rs::WINDOWS_1251.encode("привет мир");
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=windows-1251\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(&body);
        let url = spawn_server(response).await;
        let res = check(&client(), &url, Some("привет")).await;
        assert!(res.up, "{}", res.detail);
    }

    #[tokio::test]
    async fn http_500_is_down() {
        let url = spawn_server(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        let res = check(&client(), &url, None).await;
        assert!(!res.up);
        assert!(res.detail.contains("500"));
    }

    #[tokio::test]
    async fn connection_refused_is_down() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let res = check(&client(), &format!("http://{addr}/"), None).await;
        assert!(!res.up);
    }

    #[tokio::test]
    async fn validates_and_normalizes_url() {
        assert_eq!(
            validate_url("example.com").await.unwrap(),
            "https://example.com/"
        );
        assert!(validate_url("https://example.com/page#frag").await.unwrap().ends_with("/page"));
    }

    #[tokio::test]
    async fn rejects_internal_addresses() {
        for bad in [
            "http://127.0.0.1/x",
            "http://localhost:8080/",
            "http://192.168.1.1/",
            "http://10.0.0.5/",
            "http://[::1]/",
            "ftp://example.com/",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(validate_url(bad).await.is_err(), "должен быть отклонён: {bad}");
        }
    }
}