// ---------------------------------------------------------------------------
// xdg-settings properties, expressed as MIME associations
// ---------------------------------------------------------------------------

use crate::desktop::DesktopEntry;
use crate::failure::Failure;
use crate::mime::{self, Resolver};

pub enum Setting {
    WebBrowser,
    SchemeHandler(String),
}

impl Setting {
    /// Parse `PROPERTY [ARGUMENT]`. Returns the setting and the arguments
    /// that follow it.
    pub fn parse<'a>(
        property: &str,
        rest: &'a [String],
    ) -> Result<(Setting, &'a [String]), Failure> {
        match property {
            "default-web-browser" => Ok((Setting::WebBrowser, rest)),
            "default-url-scheme-handler" => match rest.split_first() {
                Some((scheme, rest)) => Ok((Setting::SchemeHandler(scheme.clone()), rest)),
                None => Err(Failure::new(
                    Failure::USAGE,
                    "default-url-scheme-handler needs a scheme",
                )),
            },
            other => Err(Failure::new(
                Failure::USAGE,
                format!("unknown property '{}'", other),
            )),
        }
    }

    /// The entry currently configured for this setting.
    pub fn get<'r>(&self, resolver: &'r Resolver) -> Option<&'r DesktopEntry> {
        match self {
            Setting::WebBrowser => resolver
                .resolve("text/html")
                .or_else(|| resolver.resolve("x-scheme-handler/http")),
            Setting::SchemeHandler(scheme) => resolver.resolve(&mime::scheme_mime(scheme)),
        }
    }

    /// Make `app` (a `*.desktop` ID) the default for this setting.
    pub fn set(&self, app: &str) -> Result<(), Failure> {
        let mimes: Vec<String> = match self {
            Setting::WebBrowser => [
                "text/html",
                "x-scheme-handler/http",
                "x-scheme-handler/https",
                "x-scheme-handler/about",
                "x-scheme-handler/unknown",
            ]
            .map(String::from)
            .to_vec(),
            Setting::SchemeHandler(scheme) => vec![mime::scheme_mime(scheme)],
        };
        mime::set_default(app, &mimes)
    }
}
