//! nV/Ec0/Rc0 serializers @5088067/@5086897/@5083710; page form
//! constraints in chunk-ERJJXAXS.js @25500..28889.
use super::radius;
use super::{
    Client, Error, Plan, SettingsMutation, State, TokenSource, incomplete, path, safe, usage,
};
use serde_json::{Value, json};
use url::Url;

const RESOURCE: &str = "guestPortalSettings";
#[derive(Clone, Copy, Debug)]
pub enum PortalType {
    InternalAck,
    External,
}
impl PortalType {
    fn wire(self) -> &'static str {
        match self {
            Self::InternalAck => "internalAck",
            Self::External => "external",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub portal_type: Option<PortalType>,
    pub welcome: Option<String>,
    pub terms_title: Option<String>,
    pub terms: Option<String>,
    pub agreement: Option<String>,
    pub accept_label: Option<String>,
    pub redirect_url: Option<String>,
    pub background_color: Option<String>,
    pub welcome_color: Option<String>,
    pub terms_color: Option<String>,
    pub agreement_color: Option<String>,
    pub button_color: Option<String>,
    pub button_text_color: Option<String>,
    pub welcome_size: Option<u8>,
    pub terms_title_size: Option<u8>,
    pub button_radius: Option<u8>,
    pub font_family: Option<String>,
    pub external_url: Option<String>,
    pub authentication: Option<bool>,
    pub radius_profile: Option<String>,
    pub accounting: Option<bool>,
    pub require_authenticator: Option<bool>,
    pub whitelisted_domains: Option<Vec<String>>,
}
impl Patch {
    pub fn is_empty(&self) -> bool {
        self.paths().is_empty()
    }
    pub fn validate(&self) -> Result<(), Error> {
        for (text, max) in [
            (&self.welcome, 64),
            (&self.terms_title, 128),
            (&self.terms, 16384),
            (&self.agreement, 128),
            (&self.accept_label, 32),
        ] {
            if text.as_deref().is_some_and(|text| {
                text.trim().is_empty()
                    || text.encode_utf16().count() > max
                    || text
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            }) {
                return Err(usage(
                    "guest portal text is empty, too long, or contains invalid control characters",
                ));
            }
        }
        for color in [
            &self.background_color,
            &self.welcome_color,
            &self.terms_color,
            &self.agreement_color,
            &self.button_color,
            &self.button_text_color,
        ] {
            if color
                .as_deref()
                .is_some_and(|color| color.trim().is_empty() || color.chars().any(char::is_control))
            {
                return Err(usage(
                    "guest portal colors must be nonempty and contain no control characters",
                ));
            }
        }
        if self
            .welcome_size
            .is_some_and(|size| !(16..=64).contains(&size) || size % 2 != 0)
            || self
                .terms_title_size
                .is_some_and(|size| !(12..=24).contains(&size) || size % 2 != 0)
            || self.button_radius.is_some_and(|size| size > 20)
        {
            return Err(usage(
                "guest portal font sizes or button radius are outside supported ranges",
            ));
        }
        if self.font_family.as_deref().is_some_and(|font| {
            ![
                "Arial",
                "Courier",
                "Georgia",
                "Helvetica",
                "Times New Roman",
                "Trebuchet MS",
                "Verdana",
            ]
            .contains(&font)
        }) {
            return Err(usage("guest portal font family is unsupported"));
        }
        if let Some(url) = &self.redirect_url
            && !url.is_empty()
        {
            valid_url(url)?;
        }
        if let Some(url) = &self.external_url {
            valid_url(url)?;
        }
        if self
            .radius_profile
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.chars().any(char::is_control))
        {
            return Err(usage(
                "RADIUS profile selector must not be empty or contain control characters",
            ));
        }
        if self.whitelisted_domains.as_ref().is_some_and(|domains| {
            domains.iter().any(|domain| !radius::valid_host(domain))
                || domains
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    != domains.len()
        }) {
            return Err(usage("whitelisted domains must be unique valid hostnames"));
        }
        if matches!(self.portal_type, Some(PortalType::InternalAck)) && self.has_external()
            || matches!(self.portal_type, Some(PortalType::External)) && self.has_internal()
        {
            return Err(usage(
                "guest portal page options do not match the selected type",
            ));
        }
        Ok(())
    }
    fn has_external(&self) -> bool {
        self.external_url.is_some()
            || self.authentication.is_some()
            || self.radius_profile.is_some()
            || self.accounting.is_some()
            || self.require_authenticator.is_some()
            || self.whitelisted_domains.is_some()
    }
    fn has_internal(&self) -> bool {
        self.welcome.is_some()
            || self.terms_title.is_some()
            || self.terms.is_some()
            || self.agreement.is_some()
            || self.accept_label.is_some()
            || self.background_color.is_some()
            || self.welcome_color.is_some()
            || self.terms_color.is_some()
            || self.agreement_color.is_some()
            || self.button_color.is_some()
            || self.button_text_color.is_some()
            || self.welcome_size.is_some()
            || self.terms_title_size.is_some()
            || self.button_radius.is_some()
            || self.font_family.is_some()
    }
    fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        if self.portal_type.is_some() {
            paths.push("guestPortalType".to_owned());
        }
        for (value, field) in [
            (&self.welcome, "welcomeMsgText"),
            (&self.terms_title, "termsTitle"),
            (&self.terms, "termsContent"),
            (&self.agreement, "termsAgreeText"),
            (&self.accept_label, "acceptBtnText"),
            (&self.background_color, "backgroundColor"),
            (&self.welcome_color, "welcomeMsgFontColor"),
            (&self.terms_color, "termsTitleFontColor"),
            (&self.agreement_color, "termsAgreeTextFontColor"),
            (&self.button_color, "acceptBtnBackgroundColor"),
            (&self.button_text_color, "acceptBtnFontColor"),
        ] {
            if value.is_some() {
                paths.push(format!("internalAckPageSettings/{field}"));
            }
        }
        for (value, field) in [
            (self.welcome_size, "welcomeMsgFontSizeInPx"),
            (self.terms_title_size, "termsTitleFontSizeInPx"),
            (self.button_radius, "acceptBtnBorderRadiusInPx"),
        ] {
            if value.is_some() {
                paths.push(format!("internalAckPageSettings/{field}"));
            }
        }
        if self.font_family.is_some() {
            paths.extend(
                [
                    "welcomeMsgFontFamily",
                    "termsTitleFontFamily",
                    "termsAgreeTextFontFamily",
                    "acceptBtnFontFamily",
                ]
                .iter()
                .map(|field| format!("internalAckPageSettings/{field}")),
            );
        }
        if self.external_url.is_some() {
            paths.extend(
                ["serverHost", "serverUrlPath", "serverPort", "useHttps"]
                    .iter()
                    .map(|field| format!("externalPageSettings/{field}")),
            );
        }
        for (value, field) in [
            (self.authentication, "isAuthenticationRequired"),
            (self.accounting, "isRadiusAccountingEnabled"),
            (
                self.require_authenticator,
                "isRadiusMessageAuthenticatorRequired",
            ),
        ] {
            if value.is_some() {
                paths.push(format!("externalPageSettings/{field}"));
            }
        }
        if self.radius_profile.is_some() {
            paths.push("externalPageSettings/radiusProfileId".to_owned());
        }
        if self.whitelisted_domains.is_some() {
            paths.push("externalPageSettings/whitelistedDomains".to_owned());
        }
        if self.redirect_url.is_some() {
            paths.push("__redirect".to_owned());
        }
        paths
    }
    fn apply(&self, body: &mut Value) -> Result<(), Error> {
        if let Some(kind) = self.portal_type {
            body["guestPortalType"] = json!(kind.wire());
        }
        let internal = match body["guestPortalType"].as_str() {
            Some("internalAck") => true,
            Some("external") => false,
            _ => return Err(incomplete("guest portal type is unknown")),
        };
        if internal && self.has_external() || !internal && self.has_internal() {
            return Err(usage(
                "guest portal page options do not match the current type",
            ));
        }
        let page = if internal {
            "internalAckPageSettings"
        } else {
            "externalPageSettings"
        };
        if !body.get(page).is_some_and(Value::is_object) {
            return Err(incomplete("guest portal page configuration is unavailable"));
        }
        if let Some(url) = &self.redirect_url {
            body[page]["redirectUrl"] = json!(if url.is_empty() {
                String::new()
            } else {
                valid_url(url)?.to_string()
            });
        }
        if internal {
            for (value, field) in [
                (&self.welcome, "welcomeMsgText"),
                (&self.terms_title, "termsTitle"),
                (&self.terms, "termsContent"),
                (&self.agreement, "termsAgreeText"),
                (&self.accept_label, "acceptBtnText"),
                (&self.background_color, "backgroundColor"),
                (&self.welcome_color, "welcomeMsgFontColor"),
                (&self.terms_color, "termsTitleFontColor"),
                (&self.agreement_color, "termsAgreeTextFontColor"),
                (&self.button_color, "acceptBtnBackgroundColor"),
                (&self.button_text_color, "acceptBtnFontColor"),
            ] {
                if let Some(value) = value {
                    body[page][field] = json!(value);
                }
            }
            for (value, field) in [
                (self.welcome_size, "welcomeMsgFontSizeInPx"),
                (self.terms_title_size, "termsTitleFontSizeInPx"),
                (self.button_radius, "acceptBtnBorderRadiusInPx"),
            ] {
                if let Some(value) = value {
                    body[page][field] = json!(value);
                }
            }
            if let Some(font) = &self.font_family {
                for field in [
                    "welcomeMsgFontFamily",
                    "termsTitleFontFamily",
                    "termsAgreeTextFontFamily",
                    "acceptBtnFontFamily",
                ] {
                    body[page][field] = json!(font);
                }
            }
        } else {
            if let Some(value) = self.authentication {
                if !value
                    && body[page]
                        .get("canDisableAuthentication")
                        .and_then(Value::as_bool)
                        != Some(true)
                {
                    return Err(usage(
                        "authentication cannot be disabled for this external portal",
                    ));
                }
                body[page]["isAuthenticationRequired"] = json!(value);
            }
            for (value, field) in [
                (self.accounting, "isRadiusAccountingEnabled"),
                (
                    self.require_authenticator,
                    "isRadiusMessageAuthenticatorRequired",
                ),
            ] {
                if let Some(value) = value {
                    body[page][field] = json!(value);
                }
            }
            if let Some(id) = &self.radius_profile {
                body[page]["radiusProfileId"] = json!(id);
            }
            if let Some(domains) = &self.whitelisted_domains {
                body[page]["whitelistedDomains"] = json!(domains);
            }
            if let Some(url) = &self.external_url {
                let url = valid_url(url)?;
                body[page]["serverHost"] = json!(url.host_str());
                body[page]["serverPort"] = json!(url.port_or_known_default());
                body[page]["useHttps"] = json!(url.scheme() == "https");
                body[page]["serverUrlPath"] = json!(format!(
                    "{}{}",
                    url.path(),
                    url.query()
                        .map(|query| format!("?{query}"))
                        .unwrap_or_default()
                ));
            }
            if body[page]
                .get("isAuthenticationRequired")
                .and_then(Value::as_bool)
                .is_none()
            {
                return Err(incomplete(
                    "external portal authentication state is unavailable",
                ));
            }
            if body[page]["isAuthenticationRequired"] == false && body[page]["useHttps"] != false {
                return Err(usage(
                    "an external portal without authentication requires HTTP",
                ));
            }
            if body[page]
                .get("serverPort")
                .and_then(Value::as_u64)
                .is_none_or(|port| port == 0 || port > 65535)
                || body[page]
                    .get("useHttps")
                    .and_then(Value::as_bool)
                    .is_none()
                || body[page]
                    .get("serverUrlPath")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(incomplete(
                    "external portal URL configuration is unavailable",
                ));
            }
            let host = body[page]
                .get("serverHost")
                .and_then(Value::as_str)
                .filter(|host| !host.is_empty() && host.len() <= 255)
                .ok_or_else(|| incomplete("external portal server host is unavailable"))?;
            let scheme = if body[page]["useHttps"] == true {
                "https"
            } else {
                "http"
            };
            let server = valid_url(&format!(
                "{scheme}://{host}:{}{}",
                body[page]["serverPort"],
                body[page]["serverUrlPath"].as_str().unwrap_or_default()
            ))?;
            if server
                .host_str()
                .is_none_or(|parsed| !parsed.eq_ignore_ascii_case(host))
            {
                return Err(incomplete("external portal server host is malformed"));
            }
            if body[page]["isAuthenticationRequired"] == true
                && body[page]
                    .get("radiusProfileId")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                radius::validate_server(&body[page]["radiusServerPrimary"])?;
            }
        }
        Ok(())
    }
}
fn valid_url(text: &str) -> Result<Url, Error> {
    let url =
        Url::parse(text).map_err(|_| usage("portal URL must be a valid HTTP or HTTPS URL"))?;
    if !text.is_ascii()
        || text.len() > 1023
        || text.chars().any(char::is_control)
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(usage(
            "portal URL must be HTTP or HTTPS without credentials or a fragment, at most 1023 characters",
        ));
    }
    Ok(url)
}
pub async fn show<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let mut body = client.get(&path(site, RESOURCE)?).await?;
    if !body.is_object() {
        return Err(incomplete("guest portal settings are unavailable"));
    }
    if !matches!(
        body.get("guestPortalType").and_then(Value::as_str),
        Some("internalAck" | "external")
    ) {
        body["guestPortalType"] = Value::Null;
    }
    for field in ["internalAckPageSettings", "externalPageSettings"] {
        if !body.get(field).is_some_and(Value::is_object) {
            body[field] = Value::Null;
        }
    }
    Ok(safe(body))
}
pub async fn update<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &str,
    mut patch: Patch,
) -> Result<(SettingsMutation<'a, T>, Plan<State>), Error> {
    patch.validate()?;
    if patch.is_empty() {
        return Err(usage("specify at least one guest portal setting"));
    }
    if let Some(selector) = &patch.radius_profile {
        let rows = super::parse(&client.get(&path(site, "radiusProfiles")?).await?)?;
        patch.radius_profile = Some(
            super::select(&rows, selector)?["id"]
                .as_str()
                .ok_or_else(|| incomplete("RADIUS profile identity is unavailable"))?
                .to_owned(),
        );
    }
    SettingsMutation::prepare(
        client,
        site,
        RESOURCE,
        |body| {
            let mut paths = patch.paths();
            if patch.redirect_url.is_some() {
                paths.retain(|path| path != "__redirect");
                let kind = patch
                    .portal_type
                    .map(PortalType::wire)
                    .or_else(|| body["guestPortalType"].as_str());
                let page = match kind {
                    Some("internalAck") => "internalAckPageSettings",
                    Some("external") => "externalPageSettings",
                    _ => return Err(incomplete("guest portal type is unknown")),
                };
                paths.push(format!("{page}/redirectUrl"));
            }
            Ok(paths)
        },
        |body| patch.apply(body),
    )
    .await
}
