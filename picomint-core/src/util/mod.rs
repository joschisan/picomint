use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::LazyLock;

use picomint_logging::LOG_CORE;
use serde::{Deserialize, Serialize};
use tracing::warn;
use url::{Host, ParseError, Url};

use crate::envs::{DEBUG_SHOW_SECRETS_ENV, is_env_var_set};

/// Stream that is `Send` unless targeting WASM
pub type BoxStream<'a, T> = Pin<Box<dyn futures::Stream<Item = T> + 'a + Send>>;

// TODO: make fully RFC1738 conformant
/// Wrapper for `Url` that only prints the scheme, domain, port and path portion
/// of a `Url` in its `Display` implementation.
///
/// This is useful to hide private
/// information like user names and passwords in logs or UIs.
///
/// The output is not fully RFC1738 conformant but good enough for our current
/// purposes.
#[derive(Hash, Clone, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
// nosemgrep: ban-raw-url
pub struct SafeUrl(Url);

picomint_redb::consensus_key!(SafeUrl);

impl picomint_encoding::Encodable for SafeUrl {
    fn consensus_encode<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        self.to_string().consensus_encode(w)
    }
}

impl picomint_encoding::Decodable for SafeUrl {
    fn consensus_decode<R: std::io::Read>(r: &mut R) -> std::io::Result<Self> {
        String::consensus_decode(r)?
            .parse()
            .map_err(|e: url::ParseError| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid SafeUrl: {e}"),
                )
            })
    }
}

impl SafeUrl {
    pub fn parse(url_str: &str) -> Result<Self, ParseError> {
        Url::parse(url_str).map(SafeUrl)
    }

    /// Warning: This removes the safety.
    // nosemgrep: ban-raw-url
    pub fn to_unsafe(self) -> Url {
        self.0
    }

    #[allow(clippy::result_unit_err)] // just copying `url`'s API here
    pub fn set_username(&mut self, username: &str) -> Result<(), ()> {
        self.0.set_username(username)
    }

    #[allow(clippy::result_unit_err)] // just copying `url`'s API here
    pub fn set_password(&mut self, password: Option<&str>) -> Result<(), ()> {
        self.0.set_password(password)
    }

    #[allow(clippy::result_unit_err)] // just copying `url`'s API here
    pub fn without_auth(&self) -> Result<Self, ()> {
        let mut url = self.clone();

        url.set_username("").and_then(|()| url.set_password(None))?;

        Ok(url)
    }

    pub fn host(&self) -> Option<Host<&str>> {
        self.0.host()
    }
    pub fn host_str(&self) -> Option<&str> {
        self.0.host_str()
    }
    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }
    pub fn port(&self) -> Option<u16> {
        self.0.port()
    }
    pub fn port_or_known_default(&self) -> Option<u16> {
        self.0.port_or_known_default()
    }
    pub fn path(&self) -> &str {
        self.0.path()
    }
    /// Warning: This will expose username & password if present.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    pub fn join(&self, input: &str) -> Result<Self, ParseError> {
        self.0.join(input).map(SafeUrl)
    }
}

static SHOW_SECRETS: LazyLock<bool> = LazyLock::new(|| {
    let enable = is_env_var_set(DEBUG_SHOW_SECRETS_ENV);

    if enable {
        warn!(target: LOG_CORE, "{} enabled. Please don't use in production.", DEBUG_SHOW_SECRETS_ENV);
    }

    enable
});

impl Display for SafeUrl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://", self.0.scheme())?;

        if !self.0.username().is_empty() {
            let show_secrets = *SHOW_SECRETS;
            if show_secrets {
                write!(f, "{}", self.0.username())?;
            } else {
                write!(f, "REDACTEDUSER")?;
            }

            if self.0.password().is_some() {
                if show_secrets {
                    write!(
                        f,
                        ":{}",
                        self.0.password().expect("Just checked it's checked")
                    )?;
                } else {
                    write!(f, ":REDACTEDPASS")?;
                }
            }

            write!(f, "@")?;
        }

        if let Some(host) = self.0.host_str() {
            write!(f, "{host}")?;
        }

        if let Some(port) = self.0.port() {
            write!(f, ":{port}")?;
        }

        write!(f, "{}", self.0.path())?;

        Ok(())
    }
}

impl Debug for SafeUrl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SafeUrl(")?;
        Display::fmt(self, f)?;
        write!(f, ")")?;
        Ok(())
    }
}

impl From<Url> for SafeUrl {
    fn from(u: Url) -> Self {
        Self(u)
    }
}

impl FromStr for SafeUrl {
    type Err = ParseError;

    #[inline]
    fn from_str(input: &str) -> Result<Self, ParseError> {
        Self::parse(input)
    }
}

#[cfg(test)]
mod tests;
