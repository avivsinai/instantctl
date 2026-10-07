//! Site administration reads and source-proven account actions.
//! Lock and unlock actions are intentionally absent: the portal bundle declares
//! their names but does not show request bodies or safety semantics.

use std::{collections::HashSet, path::Path, sync::Mutex};

use serde_json::{Value, json};

use super::{Client, reads};
use crate::{
    Error, ErrorKind, TokenSource,
    mutation::{Mutation, Plan},
    secret::SecretString,
};

mod token_output;

const RESOURCE: &str = "administration";
const ROLES: [&str; 4] = ["administrator", "operator", "delegate", "viewer"];
const USER_ROLES_CAPABILITY: &str = "user-roles";
const PERMISSION_ADD: &str = "administration_execute_addAccount";
const PERMISSION_REMOVE: &str = "administration_execute_removeAccount";
const PERMISSION_REMOVE_SELF: &str = "administration_execute_removeSelfAccount";
const PERMISSION_CHANGE_ROLE: &str = "administration_execute_changeRole";
const PERMISSION_GENERATE_SUPPORT_TOKEN: &str = "administration_execute_generateSupportToken";

fn site_path(site: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(config("--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/{RESOURCE}"))
}

fn permissions_path(site: &str) -> Result<String, Error> {
    if !reads::valid_site_id(site) {
        return Err(config("--site must be a UUID"));
    }
    Ok(format!("/sites/{site}/permissions"))
}

pub fn validate_email(email: &str) -> Result<(), Error> {
    let Some((local, domain)) = email.split_once('@') else {
        return Err(usage("email must contain one @ and a domain"));
    };
    if email.len() > 254
        || !email.is_ascii()
        || local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || domain.len() > 253
        || domain.starts_with('.')
        || domain.ends_with('.')
        || email
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return Err(usage("email address is malformed"));
    }
    Ok(())
}

pub fn validate_selector(selector: &str) -> Result<(), Error> {
    if selector.trim().is_empty() || selector.chars().any(char::is_control) {
        return Err(usage("account selector must be an exact user ID or email"));
    }
    Ok(())
}

pub fn validate_role(role: &str) -> Result<(), Error> {
    if ROLES.contains(&role) {
        Ok(())
    } else {
        Err(usage(
            "role must be administrator, operator, delegate, or viewer",
        ))
    }
}

fn validate_accounts(body: &Value) -> Result<(), Error> {
    if !body.is_object() {
        return Err(incomplete("administration response is not an object"));
    }
    if body
        .get("isApiVersionIncompatible")
        .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(false)))
    {
        return Err(incomplete(
            "administration API version is incompatible or unknown",
        ));
    }
    let accounts = body
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("administration response has no complete accounts list"))?;
    let mut collection = body.clone();
    collection["elements"] = Value::Array(accounts.clone());
    reads::parse_elements::<Value>(&collection)?;
    let mut emails = HashSet::new();
    let mut ids = HashSet::new();
    for account in accounts {
        let email = account
            .get("email")
            .and_then(Value::as_str)
            .filter(|email| !email.trim().is_empty() && !email.chars().any(char::is_control))
            .ok_or_else(|| incomplete("administration account email is missing or malformed"))?;
        if !emails.insert(email.to_owned()) {
            return Err(incomplete("administration account emails are duplicated"));
        }
        if let Some(id) = account.get("userId")
            && !id.is_null()
        {
            let id = id
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| incomplete("administration account user ID is malformed"))?;
            if !ids.insert(id.to_owned()) {
                return Err(incomplete("administration account user IDs are duplicated"));
            }
        }
        for field in ["isActivated", "isCurrentUser", "isMfaEnabled"] {
            if account
                .get(field)
                .is_some_and(|value| !value.is_boolean() && !value.is_null())
            {
                return Err(incomplete("administration account flags are malformed"));
            }
        }
        if let Some(capabilities) = account.get("capabilities") {
            if capabilities.is_null() {
                continue;
            }
            let capabilities = capabilities
                .as_object()
                .ok_or_else(|| incomplete("account capabilities are malformed"))?;
            for field in ["transferAccess", "removeAccess", "changeRole"] {
                if capabilities
                    .get(field)
                    .is_some_and(|value| !value.is_boolean() && !value.is_null())
                {
                    return Err(incomplete("account access capability is malformed"));
                }
            }
        }
    }
    Ok(())
}

async fn read_raw<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    let body = client.get(&site_path(site)?).await?;
    validate_accounts(&body)?;
    Ok(body)
}

/// Return the full administration object with token values redacted.
pub async fn get<T: TokenSource>(client: &Client<T>, site: &str) -> Result<Value, Error> {
    Ok(super::access::safe(read_raw(client, site).await?))
}

pub async fn permissions<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Vec<String>, Error> {
    let body = client.get(&permissions_path(site)?).await?;
    let rows = body
        .get("permissions")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("permissions response has no complete list"))?;
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let permission = row
            .get("permission")
            .and_then(Value::as_str)
            .filter(|permission| !permission.is_empty())
            .ok_or_else(|| incomplete("permissions response has a malformed entry"))?;
        if !seen.insert(permission.to_owned()) {
            return Err(incomplete("permissions response has duplicate entries"));
        }
        result.push(permission.to_owned());
    }
    Ok(result)
}

pub async fn check_account<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    email: &str,
) -> Result<bool, Error> {
    validate_email(email)?;
    let path = format!("{}?action=checkAccount", site_path(site)?);
    let response = client.create(&path, &json!({"email":email})).await?;
    if response.is_null() {
        // An activated site membership proves existence. Its absence cannot
        // prove that an account is absent from the global account directory.
        let administration = get(client, site).await?;
        if administration["accounts"]
            .as_array()
            .is_some_and(|accounts| {
                accounts.iter().any(|account| {
                    account.get("email").and_then(Value::as_str) == Some(email)
                        && account.get("isActivated").and_then(Value::as_bool) == Some(true)
                })
            })
        {
            return Ok(true);
        }
    }
    response
        .get("exists")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("account check response is missing its exists flag"))
}

struct Capabilities {
    names: HashSet<String>,
    max_user_account_count: Option<u64>,
}

async fn capabilities<T: TokenSource>(
    client: &Client<T>,
    site: &str,
) -> Result<Capabilities, Error> {
    let body = client.get(&format!("/sites/{site}/capabilities")).await?;
    let rows = body
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("site capabilities response is malformed"))?;
    let mut names = HashSet::new();
    for row in rows {
        let name = row
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| incomplete("site capabilities response is malformed"))?;
        if !names.insert(name.to_owned()) {
            return Err(incomplete("site capabilities response has duplicates"));
        }
    }
    let max_user_account_count = body
        .get("maxUserAccountCount")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| incomplete("maximum user account count is malformed"))
        })
        .transpose()?;
    Ok(Capabilities {
        names,
        max_user_account_count,
    })
}

async fn require_permission<T: TokenSource>(
    client: &Client<T>,
    site: &str,
    expected: &str,
) -> Result<(), Error> {
    if permissions(client, site)
        .await?
        .iter()
        .any(|permission| permission == expected)
    {
        Ok(())
    } else {
        Err(unsupported(&format!("missing permission {expected}")))
    }
}

fn require_capability(capabilities: &Capabilities, expected: &str) -> Result<(), Error> {
    if capabilities.names.contains(expected) {
        Ok(())
    } else {
        Err(unsupported(&format!("missing capability {expected}")))
    }
}

pub fn select<'a>(body: &'a Value, selector: &str) -> Result<&'a Value, Error> {
    validate_selector(selector)?;
    validate_accounts(body)?;
    let accounts = body
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("administration response has no accounts list"))?;
    if let Some(account) = accounts
        .iter()
        .find(|account| account.get("userId").and_then(Value::as_str) == Some(selector))
    {
        return Ok(account);
    }
    let mut matches = accounts
        .iter()
        .filter(|account| account.get("email").and_then(Value::as_str) == Some(selector));
    let account = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "no administration account matches selector",
        )
    })?;
    if matches.next().is_some() {
        return Err(usage("account email is ambiguous; select by exact user ID"));
    }
    Ok(account)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Target {
    id: Option<String>,
    email: String,
    role: String,
    activated: bool,
    current_user: bool,
}

fn target(body: &Value, selector: &str) -> Result<Target, Error> {
    let account = select(body, selector)?;
    target_from_account(account)
}

fn target_from_account(account: &Value) -> Result<Target, Error> {
    let id = match account.get("userId") {
        Some(Value::Null) | None => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        _ => return Err(incomplete("target administration account ID is malformed")),
    };
    let email = account
        .get("email")
        .and_then(Value::as_str)
        .ok_or_else(|| incomplete("target administration account email is unknown"))?;
    let role = account
        .get("roleOnSite")
        .and_then(Value::as_str)
        .filter(|role| ROLES.contains(role))
        .ok_or_else(|| incomplete("target administration account role is unknown"))?;
    let activated = account
        .get("isActivated")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("target account activation state is unknown"))?;
    let current_user = account
        .get("isCurrentUser")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("target current-user state is unknown"))?;
    Ok(Target {
        id,
        email: email.to_owned(),
        role: role.to_owned(),
        activated,
        current_user,
    })
}

fn find_id<'a>(body: &'a Value, id: &str) -> Result<&'a Value, Error> {
    let accounts = body
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("administration response has no accounts list"))?;
    let mut matches = accounts
        .iter()
        .filter(|account| account.get("userId").and_then(Value::as_str) == Some(id));
    let account = matches.next().ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "target administration account disappeared",
        )
    })?;
    if matches.next().is_some() {
        return Err(incomplete("target administration account ID is duplicated"));
    }
    Ok(account)
}

fn recheck_target<'a>(body: &'a Value, expected: &Target) -> Result<&'a Value, Error> {
    let account = match expected.id.as_deref() {
        Some(id) => find_id(body, id)?,
        None => select(body, &expected.email)?,
    };
    let current = target_from_account(account)?;
    if current != *expected {
        return Err(incomplete(
            "target administration account changed since planning",
        ));
    }
    Ok(account)
}

fn require_account_capability(account: &Value, name: &str) -> Result<(), Error> {
    match account
        .pointer(&format!("/capabilities/{name}"))
        .and_then(Value::as_bool)
    {
        Some(true) => Ok(()),
        Some(false) => Err(unsupported(&format!(
            "target account does not permit this operation: missing capability {name}"
        ))),
        None => Err(incomplete(&format!(
            "target account capability {name} is unknown"
        ))),
    }
}

fn ensure_other_active_administrator(body: &Value, target: &Target) -> Result<(), Error> {
    let accounts = body
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete("administration response has no accounts list"))?;
    let mut other_active_admin = false;
    for account in accounts {
        let role = account
            .get("roleOnSite")
            .and_then(Value::as_str)
            .filter(|role| ROLES.contains(role))
            .ok_or_else(|| {
                incomplete("administrator safety is unknown because a role is unknown")
            })?;
        let activated = account
            .get("isActivated")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                incomplete("administrator safety is unknown because activation is unknown")
            })?;
        if role == "administrator" && activated {
            let id = account
                .get("userId")
                .and_then(Value::as_str)
                .ok_or_else(|| incomplete("active administrator identity is unknown"))?;
            if target.id.as_deref() != Some(id)
                && account.get("email").and_then(Value::as_str) != Some(target.email.as_str())
            {
                other_active_admin = true;
            }
        }
    }
    if other_active_admin {
        Ok(())
    } else {
        Err(unsupported(
            "cannot remove or demote the final active administrator",
        ))
    }
}

#[derive(Clone)]
enum Operation {
    Add { email: String, role: String },
    Remove { target: Target },
    ChangeRole { target: Target, role: String },
    Maintenance { enabled: bool, current: bool },
}

struct AdminMutation<'a, T> {
    client: &'a Client<T>,
    site: &'a str,
    path: String,
    operation: Operation,
    desired: Value,
}

impl<T: TokenSource> AdminMutation<'_, T> {
    async fn raw(&self) -> Result<Value, Error> {
        read_raw(self.client, self.site).await
    }

    async fn read_state(&self) -> Result<Value, Error> {
        let body = self.raw().await?;
        match &self.operation {
            Operation::Add { email, .. } => {
                let row = body["accounts"]
                    .as_array()
                    .and_then(|rows| rows.iter().find(|row| row["email"].as_str() == Some(email)));
                Ok(json!({
                    "email":email,
                    "role":row.and_then(|row| row.get("roleOnSite")).cloned().unwrap_or(Value::Null),
                    "member":row.is_some()
                }))
            }
            Operation::Remove { target } => Ok(json!({
                "email":target.email,
                "member":body["accounts"].as_array().is_some_and(|rows| rows.iter().any(|row| {
                    row["email"].as_str() == Some(target.email.as_str())
                        || target.id.as_deref().is_some_and(|id| row["userId"].as_str() == Some(id))
                }))
            })),
            Operation::ChangeRole { target, .. } => {
                let id = target
                    .id
                    .as_deref()
                    .ok_or_else(|| incomplete("target account ID is unknown"))?;
                let row = find_id(&body, id)?;
                let role = row
                    .get("roleOnSite")
                    .and_then(Value::as_str)
                    .ok_or_else(|| incomplete("target account role readback is unknown"))?;
                Ok(json!({"user_id":id,"role":role}))
            }
            Operation::Maintenance { .. } => {
                let value = body
                    .get("isMaintenanceMode")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| incomplete("maintenance mode state is unknown"))?;
                Ok(json!({"is_maintenance_mode":value}))
            }
        }
    }

    async fn check_operation(&self, fresh: &Value) -> Result<(), Error> {
        match &self.operation {
            Operation::Add { email, role } => {
                if fresh["accounts"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row["email"].as_str() == Some(email)))
                {
                    return Err(usage("an account with this exact email already exists"));
                }
                let caps = capabilities(self.client, self.site).await?;
                let max = caps
                    .max_user_account_count
                    .ok_or_else(|| incomplete("maximum user account count is unknown"))?;
                let count = fresh["accounts"].as_array().map_or(0, Vec::len) as u64;
                if count >= max {
                    return Err(unsupported("site user account limit has been reached"));
                }
                if role != "administrator" {
                    require_capability(&caps, USER_ROLES_CAPABILITY)?;
                }
                require_permission(self.client, self.site, PERMISSION_ADD).await
            }
            Operation::Remove { target } => {
                let row = recheck_target(fresh, target)?;
                require_account_capability(row, "removeAccess")?;
                let permission = if target.current_user {
                    PERMISSION_REMOVE_SELF
                } else {
                    PERMISSION_REMOVE
                };
                require_permission(self.client, self.site, permission).await?;
                if target.role == "administrator" && target.activated {
                    ensure_other_active_administrator(fresh, target)?;
                }
                Ok(())
            }
            Operation::ChangeRole { target, role } => {
                let row = recheck_target(fresh, target)?;
                require_account_capability(row, "changeRole")?;
                require_capability(
                    &capabilities(self.client, self.site).await?,
                    USER_ROLES_CAPABILITY,
                )?;
                require_permission(self.client, self.site, PERMISSION_CHANGE_ROLE).await?;
                if target.role == "administrator" && role != "administrator" && target.activated {
                    ensure_other_active_administrator(fresh, target)?;
                }
                Ok(())
            }
            Operation::Maintenance {
                current: expected_current,
                ..
            } => {
                let current = fresh
                    .get("isMaintenanceMode")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| incomplete("maintenance mode state is unknown"))?;
                if current != *expected_current {
                    return Err(incomplete("maintenance mode changed since planning"));
                }
                Ok(())
            }
        }
    }

    async fn post(&self) -> Result<(), Error> {
        let (action, body) = match &self.operation {
            Operation::Add { email, role } => {
                ("addAccount", json!({"email":email,"roleOnSite":role}))
            }
            Operation::Remove { target } => ("removeAccount", json!({"email":target.email})),
            Operation::ChangeRole { target, role } => {
                let id = target
                    .id
                    .as_deref()
                    .ok_or_else(|| incomplete("target account ID is unknown"))?;
                ("changeRole", json!({"accountId":id,"roleOnSite":role}))
            }
            Operation::Maintenance { enabled, .. } => (
                if *enabled {
                    "enableMaintenanceMode"
                } else {
                    "disableMaintenanceMode"
                },
                json!({}),
            ),
        };
        self.client
            .create(&format!("{}?action={action}", self.path), &body)
            .await?;
        Ok(())
    }
}

impl<T: TokenSource> Mutation for AdminMutation<'_, T> {
    type State = Value;

    async fn read(&self) -> Result<Self::State, Error> {
        self.read_state().await
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired != &self.desired {
            return Err(config(
                "administration action does not match its prepared plan",
            ));
        }
        let fresh = self.raw().await?;
        self.check_operation(&fresh).await?;
        self.post().await
    }
}

async fn prepare_account_action<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    body: Value,
    operation: Operation,
    current: Value,
    desired: Value,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    let path = site_path(site)?;
    let mutation = AdminMutation {
        client,
        site,
        path,
        operation,
        desired: desired.clone(),
    };
    mutation.check_operation(&body).await?;
    Ok((mutation, Plan { current, desired }))
}

pub async fn add<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    email: String,
    role: String,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    validate_email(&email)?;
    validate_role(&role)?;
    let body = read_raw(client, site).await?;
    prepare_account_action(
        client,
        site,
        body,
        Operation::Add {
            email: email.clone(),
            role: role.clone(),
        },
        json!({"email":email,"role":Value::Null,"member":false}),
        json!({"email":email,"role":role,"member":true}),
    )
    .await
}

pub async fn remove<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    selector: String,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    validate_selector(&selector)?;
    let body = read_raw(client, site).await?;
    let target = target(&body, &selector)?;
    prepare_account_action(
        client,
        site,
        body,
        Operation::Remove {
            target: target.clone(),
        },
        json!({"email":target.email,"member":true}),
        json!({"email":target.email,"member":false}),
    )
    .await
}

pub async fn change_role<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    selector: String,
    role: String,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    validate_selector(&selector)?;
    validate_role(&role)?;
    let body = read_raw(client, site).await?;
    let target = target(&body, &selector)?;
    if target.id.is_none() {
        return Err(incomplete("target account ID is unknown"));
    }
    prepare_account_action(
        client,
        site,
        body,
        Operation::ChangeRole {
            target: target.clone(),
            role: role.clone(),
        },
        json!({"user_id":target.id,"role":target.role}),
        json!({"user_id":target.id,"role":role}),
    )
    .await
}

pub async fn maintenance<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
    enabled: bool,
) -> Result<(impl Mutation<State = Value> + 'a, Plan<Value>), Error> {
    let body = read_raw(client, site).await?;
    let current = body
        .get("isMaintenanceMode")
        .and_then(Value::as_bool)
        .ok_or_else(|| incomplete("maintenance mode state is unknown"))?;
    prepare_account_action(
        client,
        site,
        body,
        Operation::Maintenance { enabled, current },
        json!({"is_maintenance_mode":current}),
        json!({"is_maintenance_mode":enabled}),
    )
    .await
}

pub struct SupportTokenMutation<'a, T> {
    client: &'a Client<T>,
    site: &'a str,
    path: String,
    token: Mutex<Option<SecretString>>,
    verified: Mutex<bool>,
}

impl<T: TokenSource> SupportTokenMutation<'_, T> {
    /// Export only the token acknowledged by the action and matched by fresh GET.
    pub fn write_verified_token(&self, output: &Path) -> Result<(), Error> {
        let token = self.verified_token()?;
        token_output::TokenFile::create(output)?.write(&token)
    }

    pub fn verified_token(&self) -> Result<SecretString, Error> {
        if !*self
            .verified
            .lock()
            .map_err(|_| incomplete("support token verification state is unavailable"))?
        {
            return Err(incomplete("support token has no matching fresh readback"));
        }
        self.token
            .lock()
            .map_err(|_| incomplete("support token verification state is unavailable"))?
            .clone()
            .ok_or_else(|| incomplete("verified support token is unavailable"))
    }

    async fn read_raw(&self) -> Result<Value, Error> {
        read_raw(self.client, self.site).await
    }
}

impl<T: TokenSource> Mutation for SupportTokenMutation<'_, T> {
    type State = Value;

    async fn read(&self) -> Result<Self::State, Error> {
        let body = self.read_raw().await?;
        let expected = self
            .token
            .lock()
            .map_err(|_| incomplete("support token verification state is unavailable"))?
            .clone();
        let matched = expected.as_ref().is_some_and(|expected| {
            body.pointer("/supportToken/token").and_then(Value::as_str)
                == Some(expected.expose_secret())
        });
        *self
            .verified
            .lock()
            .map_err(|_| incomplete("support token verification state is unavailable"))? = matched;
        Ok(json!({"token_generated":matched}))
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        if desired != &json!({"token_generated":true}) {
            return Err(config(
                "support token action does not match its prepared plan",
            ));
        }
        require_permission(self.client, self.site, PERMISSION_GENERATE_SUPPORT_TOKEN).await?;
        let response = self
            .client
            .create(
                &format!("{}?action=generateSupportToken", self.path),
                &json!({}),
            )
            .await?;
        let token = response
            .pointer("/supportToken/token")
            .and_then(Value::as_str)
            .filter(|token| is_unmasked_token(token))
            .ok_or_else(|| incomplete("support token acknowledgment is missing or masked"))?;
        *self
            .token
            .lock()
            .map_err(|_| incomplete("support token state is unavailable"))? =
            Some(SecretString::new(token.to_owned()));
        *self
            .verified
            .lock()
            .map_err(|_| incomplete("support token state is unavailable"))? = false;
        Ok(())
    }
}

fn is_unmasked_token(token: &str) -> bool {
    let folded = token.trim().to_ascii_lowercase();
    !token.trim().is_empty()
        && !token.chars().any(char::is_control)
        && !token.contains('*')
        && !token.contains('•')
        && !matches!(
            folded.as_str(),
            "redacted" | "(redacted)" | "[redacted]" | "<redacted>" | "masked" | "hidden"
        )
}

pub async fn support_token<'a, T: TokenSource>(
    client: &'a Client<T>,
    site: &'a str,
) -> Result<(SupportTokenMutation<'a, T>, Plan<Value>), Error> {
    let path = site_path(site)?;
    read_raw(client, site).await?;
    require_permission(client, site, PERMISSION_GENERATE_SUPPORT_TOKEN).await?;
    Ok((
        SupportTokenMutation {
            client,
            site,
            path,
            token: Mutex::new(None),
            verified: Mutex::new(false),
        },
        Plan {
            current: json!({"token_generated":false}),
            desired: json!({"token_generated":true}),
        },
    ))
}

/// Validate a new support-token destination before credentials or requests.
pub fn validate_support_token_output(path: &Path) -> Result<(), Error> {
    token_output::validate_path(path)
}

fn usage(message: &str) -> Error {
    Error::new(ErrorKind::Usage, message)
}

fn config(message: &str) -> Error {
    Error::new(ErrorKind::Config, message)
}

fn unsupported(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

fn incomplete(message: &str) -> Error {
    Error::new(ErrorKind::Unverified, message)
}
