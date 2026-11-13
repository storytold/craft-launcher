use std::io::{self, Read as _};
use std::time::Duration;

use ureq::http::{Request, Response, header};

/// Uses the same platform certificate verifier as the desktop launcher.
#[derive(Clone)]
pub(crate) struct Https(ureq::Agent);

impl Https {
    pub(crate) fn new(timeout: Duration) -> Self {
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .tls_config(tls)
            .build()
            .into();
        Self(agent)
    }

    pub(crate) async fn send(&self, request: Request<Vec<u8>>, limit: usize) -> Result<Response<Vec<u8>>, io::Error> {
        let agent = self.0.clone();
        tokio::task::spawn_blocking(move || {
            if request.uri().scheme_str() != Some("https") {
                return Err(io::Error::other("HTTPS is required"));
            }
            let response = agent.run(request).map_err(|_| io::Error::other("HTTPS request failed"))?;
            let (parts, mut body) = response.into_parts();
            if parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|size| size > limit)
            {
                return Err(io::Error::other("HTTPS response exceeds its size limit"));
            }
            let mut bytes = Vec::new();
            body.as_reader().take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| io::Error::other("Cannot read HTTPS response"))?;
            if bytes.len() > limit {
                return Err(io::Error::other("HTTPS response exceeds its size limit"));
            }
            Ok(Response::from_parts(parts, bytes))
        })
        .await
        .map_err(|_| io::Error::other("HTTPS request task failed"))?
    }
}
