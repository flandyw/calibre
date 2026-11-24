use std::io::Read;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::{Error, Result};

pub(crate) fn agent(redirects: u32) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(35)))
        .max_redirects(redirects)
        .user_agent(concat!("calibre-rust/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

pub(crate) fn response_error(service: &str, error: ureq::Error) -> Error {
    let reason = match error {
        ureq::Error::StatusCode(401 | 403) => {
            "access denied; check credentials and permissions".into()
        }
        ureq::Error::StatusCode(402) => "insufficient credits".into(),
        ureq::Error::StatusCode(429) => "rate limit reached; try again later".into(),
        ureq::Error::StatusCode(status) => format!("HTTP {status}"),
        ureq::Error::Timeout(_) => "request timed out".into(),
        _ => "connection failed".into(),
    };
    Error::Other(format!("{service}: {reason}"))
}

pub(crate) fn read(
    service: &str,
    response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut response = response.map_err(|error| response_error(service, error))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Other(format!("{service}: couldn't read response")))?;
    if bytes.len() as u64 > limit {
        return Err(Error::Other(format!("{service}: response too large")));
    }
    Ok(bytes)
}

pub(crate) fn json<T: DeserializeOwned>(
    service: &str,
    response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T> {
    serde_json::from_slice(&read(service, response, 8 * 1024 * 1024)?)
        .map_err(|_| Error::Other(format!("{service}: invalid JSON response")))
}

pub(crate) fn plain_comments(text: &str) -> String {
    html_escape::encode_text(text.trim()).replace('\n', "<br>")
}

#[cfg(test)]
pub(crate) mod test {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    pub fn serve(
        status: &str,
        body: impl AsRef<[u8]>,
        validate: impl FnOnce(&str, &str) + Send + 'static,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.as_ref().len()
        )
        .bytes()
        .chain(body.as_ref().iter().copied())
        .collect::<Vec<_>>();
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| value.trim().parse().unwrap());
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            validate(&headers, std::str::from_utf8(&body).unwrap());
            socket.write_all(&response).unwrap();
        });
        (base, handle)
    }
}
