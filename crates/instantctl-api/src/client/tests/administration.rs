use super::*;
use crate::{
    ErrorKind,
    client::administration::{
        add, change_role, check_account, get, maintenance, remove, select, support_token,
        validate_email, validate_role, validate_selector, validate_support_token_output,
    },
    mutation::{Mutation, Outcome},
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const ADMIN_PERMISSION: &str = "administration_execute_addAccount";
const REMOVE_PERMISSION: &str = "administration_execute_removeAccount";
const CHANGE_ROLE_PERMISSION: &str = "administration_execute_changeRole";
const SUPPORT_PERMISSION: &str = "administration_execute_generateSupportToken";
const USER_ROLES: &str = "user-roles";
const SECRET: &str = "support-token-secret-sentinel";
const GENERATED_TOKEN: &str = "fresh-support-token-secret-sentinel";

struct OutputDir(PathBuf);

impl OutputDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).expect("generate unique support-token output directory");
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "hpe-support-token-test-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create private support-token output directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("restrict support-token output directory permissions");
        }
        Self(path)
    }

    fn output_path(&self) -> PathBuf {
        self.0.join("support-token.txt")
    }
}

impl Drop for OutputDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn reply(value: Value) -> Reply {
    Reply::json(200, serde_json::to_vec(&value).expect("serialize reply"))
}

fn admin(accounts: Vec<Value>, maintenance: bool) -> Value {
    json!({
        "accounts":accounts,
        "isMaintenanceMode":maintenance,
        "supportToken":{"token":"stored-support-token-secret-sentinel","validUntil":"2030-01-01T00:00:00Z"},
        "deviceWebUiToken":{"token":"device-ui-token-secret-sentinel"}
    })
}

fn account(id: &str, email: &str, role: &str, activated: bool, capabilities: Value) -> Value {
    json!({
        "userId":id,"email":email,"isActivated":activated,"isCurrentUser":false,
        "isMfaEnabled":true,"roleOnSite":role,"capabilities":capabilities
    })
}

fn permissions(names: &[&str]) -> Value {
    json!({"permissions":names.iter().map(|permission| json!({"permission":permission})).collect::<Vec<_>>()})
}

fn capabilities(names: &[&str], max_accounts: u64) -> Value {
    json!({"capabilities":names,"maxUserAccountCount":max_accounts})
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).expect("JSON request body")
}

fn failure<T>(result: Result<T, crate::Error>) -> crate::Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("administration operation should fail"),
    }
}

fn target_account() -> Value {
    account(
        "user-1",
        "operator@example.test",
        "operator",
        true,
        json!({"removeAccess":true,"changeRole":true}),
    )
}

fn pending_invitation_without_id() -> Value {
    json!({
        "email":"pending@example.test","isActivated":false,
        "isCurrentUser":false,"isMfaEnabled":false,"roleOnSite":"viewer",
        "capabilities":{"removeAccess":true,"changeRole":false}
    })
}

fn pending_administrator_invitation() -> Value {
    json!({
        "userId":null,"email":"pending-admin@example.test","isActivated":false,
        "isCurrentUser":false,"isMfaEnabled":false,"roleOnSite":"administrator",
        "capabilities":{"removeAccess":true,"changeRole":true}
    })
}

#[tokio::test]
async fn administration_read_redacts_both_token_fields_and_preserves_account_wire_fields() {
    let server = MockServer::start(vec![reply(admin(vec![target_account()], false))]);
    let client = make_client(&server, Duration::from_secs(1));

    let result = get(&client, SITE).await.expect("administration read");
    assert_eq!(result["accounts"][0]["userId"], "user-1");
    assert_eq!(result["accounts"][0]["roleOnSite"], "operator");
    assert_eq!(result["accounts"][0]["isActivated"], true);
    assert_eq!(result["accounts"][0]["isCurrentUser"], false);
    assert_eq!(result["accounts"][0]["isMfaEnabled"], true);
    assert_eq!(result["accounts"][0]["capabilities"]["removeAccess"], true);
    assert_eq!(result["isMaintenanceMode"], false);
    for token_path in ["/supportToken/token", "/deviceWebUiToken/token"] {
        let token = result.pointer(token_path).and_then(Value::as_str);
        assert!(token.is_none() || token == Some("(redacted)"));
    }
    assert!(!result.to_string().contains("support-token-secret-sentinel"));
    assert!(
        !result
            .to_string()
            .contains("device-ui-token-secret-sentinel")
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        format!("/api/sites/{SITE}/administration")
    );
    assert!(requests[0].body.is_empty());
}

#[tokio::test]
async fn administration_read_preserves_null_pending_invitation_fields() {
    let mut invite = account("", "pending@example.test", "viewer", false, json!({}));
    invite["userId"] = Value::Null;
    invite["isActivated"] = Value::Null;
    invite["isCurrentUser"] = Value::Null;
    invite["isMfaEnabled"] = Value::Null;
    invite["capabilities"] = Value::Null;
    let server = MockServer::start(vec![reply(admin(vec![invite], false))]);
    let client = make_client(&server, Duration::from_secs(1));

    let result = get(&client, SITE)
        .await
        .expect("read preserves nullable invitation fields");
    let account = &result["accounts"][0];
    assert!(account["userId"].is_null());
    assert!(account["isActivated"].is_null());
    assert!(account["isCurrentUser"].is_null());
    assert!(account["isMfaEnabled"].is_null());
    assert!(account["capabilities"].is_null());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn account_selectors_and_role_validators_accept_only_exact_known_values() {
    validate_email("admin@example.test").expect("valid email");
    for invalid in ["", "missing-at", "a@@example.test", "a@bad..test", "a@x\ny"] {
        assert_eq!(validate_email(invalid).unwrap_err().kind, ErrorKind::Usage);
    }
    validate_selector("user-1").expect("exact user ID");
    validate_selector("admin@example.test").expect("exact email");
    for invalid in ["", "  ", "user\n1"] {
        assert_eq!(
            validate_selector(invalid).unwrap_err().kind,
            ErrorKind::Usage
        );
    }
    for role in ["administrator", "operator", "delegate", "viewer"] {
        validate_role(role).expect("known site role");
    }
    for role in ["owner", "admin", "Administrator", "unknown"] {
        assert_eq!(validate_role(role).unwrap_err().kind, ErrorKind::Usage);
    }

    let body = admin(
        vec![
            account(
                "user-1",
                "admin@example.test",
                "administrator",
                true,
                json!({}),
            ),
            account("user-2", "viewer@example.test", "viewer", true, json!({})),
        ],
        false,
    );
    assert_eq!(
        select(&body, "user-1").unwrap()["email"],
        "admin@example.test"
    );
    assert_eq!(
        select(&body, "viewer@example.test").unwrap()["userId"],
        "user-2"
    );
    assert_eq!(
        select(&body, "missing@example.test").unwrap_err().kind,
        ErrorKind::NotFound
    );
    let duplicate_email = admin(
        vec![
            account(
                "user-1",
                "same@example.test",
                "administrator",
                true,
                json!({}),
            ),
            account("user-2", "same@example.test", "viewer", true, json!({})),
        ],
        false,
    );
    assert_eq!(
        select(&duplicate_email, "user-1").unwrap_err().kind,
        ErrorKind::Unverified,
        "ambiguous account rows must not resolve even by ID"
    );
}

#[tokio::test]
async fn account_check_requires_a_boolean_result_or_fresh_activated_membership() {
    let email = "operator@example.test";
    let mut pending = target_account();
    pending["isActivated"] = json!(false);
    let mut unrelated = target_account();
    unrelated["email"] = json!("other@example.test");
    let mut unknown_activation = target_account();
    unknown_activation
        .as_object_mut()
        .unwrap()
        .remove("isActivated");
    for (response, accounts, expected) in [
        (reply(json!({"exists":true})), None, Ok(true)),
        (reply(json!({"exists":false})), None, Ok(false)),
        (
            Reply::json(204, vec![]),
            Some(vec![target_account()]),
            Ok(true),
        ),
        (
            Reply::json(204, vec![]),
            Some(vec![pending]),
            Err(ErrorKind::Unverified),
        ),
        (
            Reply::json(204, vec![]),
            Some(vec![unrelated]),
            Err(ErrorKind::Unverified),
        ),
        (
            Reply::json(204, vec![]),
            Some(vec![unknown_activation]),
            Err(ErrorKind::Unverified),
        ),
        (
            Reply::json(204, vec![]),
            Some(vec![]),
            Err(ErrorKind::Unverified),
        ),
        (
            reply(json!({"exists":"true"})),
            None,
            Err(ErrorKind::Unverified),
        ),
    ] {
        let fallback = accounts.is_some();
        let mut replies = vec![response];
        if let Some(accounts) = accounts {
            replies.push(reply(admin(accounts, false)));
        }
        let server = MockServer::start(replies);
        let client = make_client(&server, Duration::from_secs(1));
        let result = check_account(&client, SITE, email).await;
        match expected {
            Ok(exists) => assert_eq!(result.expect("verified account existence"), exists),
            Err(kind) => assert_eq!(failure(result).kind, kind),
        }
        let requests = server.finish();
        assert_eq!(requests.len(), if fallback { 2 } else { 1 });
        assert_eq!(requests[0].method, "POST");
        assert_eq!(
            requests[0].target,
            format!("/api/sites/{SITE}/administration?action=checkAccount")
        );
        assert_eq!(body(&requests[0]), json!({"email":email}));
        if fallback {
            assert_eq!(requests[1].method, "GET");
            assert_eq!(
                requests[1].target,
                format!("/api/sites/{SITE}/administration")
            );
        }
    }
}

#[tokio::test]
async fn invalid_email_and_site_fail_before_any_request() {
    let server = MockServer::start(vec![]);
    let client = make_client(&server, Duration::from_secs(1));

    assert_eq!(
        failure(check_account(&client, SITE, "invalid").await).kind,
        ErrorKind::Usage
    );
    assert_eq!(
        failure(get(&client, "not-a-site").await).kind,
        ErrorKind::Config
    );
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn remove_refuses_unknown_account_capability_before_permission_or_post() {
    let account = account(
        "user-1",
        "operator@example.test",
        "operator",
        true,
        json!({"changeRole":true}),
    );
    let server = MockServer::start(vec![reply(admin(vec![account], false))]);
    let client = make_client(&server, Duration::from_secs(1));

    let error = failure(remove(&client, SITE, "user-1".into()).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
}

#[tokio::test]
async fn incomplete_administration_page_is_not_used_to_authorize_mutations() {
    let mut partial = admin(vec![target_account()], false);
    partial["metaData"] = json!({"nextPageToken":"next-page"});
    let server = MockServer::start(vec![reply(partial)]);
    let client = make_client(&server, Duration::from_secs(1));

    let error = failure(remove(&client, SITE, "user-1".into()).await);
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(error.kind, ErrorKind::Unverified);
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn add_account_checks_limits_permissions_rechecks_and_verifies_one_post() {
    let _clock = keep_clock_paused().await;
    let empty = admin(vec![], false);
    let member = account("new-user", "new@example.test", "operator", false, json!({}));
    let server = MockServer::start(vec![
        reply(empty.clone()),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[ADMIN_PERMISSION])),
        reply(empty.clone()),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[ADMIN_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![member], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = add(&client, SITE, "new@example.test".into(), "operator".into())
        .await
        .expect("prepare account addition");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("apply account addition");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    assert_eq!(report.observed, Some(plan.desired));

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].target,
        format!("/api/sites/{SITE}/administration")
    );
    assert_eq!(
        requests[1].target,
        format!("/api/sites/{SITE}/capabilities")
    );
    assert_eq!(requests[2].target, format!("/api/sites/{SITE}/permissions"));
    assert_eq!(
        requests[3].method, "GET",
        "write rechecks fresh account state"
    );
    assert_eq!(
        requests[4].target,
        format!("/api/sites/{SITE}/capabilities")
    );
    assert_eq!(requests[5].target, format!("/api/sites/{SITE}/permissions"));
    assert_eq!(requests[6].method, "POST");
    assert_eq!(
        requests[6].target,
        format!("/api/sites/{SITE}/administration?action=addAccount")
    );
    assert_eq!(
        body(&requests[6]),
        json!({"email":"new@example.test","roleOnSite":"operator"})
    );
    assert_eq!(
        requests[7].target,
        format!("/api/sites/{SITE}/administration")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn add_role_requires_capability_and_permission_before_any_post() {
    let server = MockServer::start(vec![
        reply(admin(vec![], false)),
        reply(capabilities(&[], 4)),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = failure(add(&client, SITE, "new@example.test".into(), "operator".into()).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));

    let server = MockServer::start(vec![
        reply(admin(vec![], false)),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let error = failure(add(&client, SITE, "new@example.test".into(), "operator".into()).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test(start_paused = true)]
async fn add_with_wrong_role_readback_is_not_verified() {
    let _clock = keep_clock_paused().await;
    let empty = admin(vec![], false);
    let wrong_role = account("new-user", "new@example.test", "viewer", false, json!({}));
    let server = MockServer::start(vec![
        reply(empty.clone()),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[ADMIN_PERMISSION])),
        reply(empty.clone()),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[ADMIN_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![wrong_role], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = add(&client, SITE, "new@example.test".into(), "operator".into())
        .await
        .expect("prepare account addition");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("wrong-role readback is reported as unverified");
    assert_eq!(report.outcome, Outcome::Unverified, "{report:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn removal_refuses_final_active_administrator_even_with_pending_admin() {
    let last_active = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({"removeAccess":true}),
    );
    let pending_admin = pending_administrator_invitation();
    let server = MockServer::start(vec![
        reply(admin(vec![last_active, pending_admin], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));

    let error = failure(remove(&client, SITE, "admin-1".into()).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("final active administrator"));
    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn role_change_refuses_demotion_of_final_active_administrator() {
    let administrator = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({"changeRole":true}),
    );
    let pending = pending_administrator_invitation();
    let server = MockServer::start(vec![
        reply(admin(vec![administrator, pending], false)),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[CHANGE_ROLE_PERMISSION])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));

    let error = failure(change_role(&client, SITE, "admin-1".into(), "operator".into()).await);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("final active administrator"));
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test(start_paused = true)]
async fn pending_invitation_without_user_id_can_be_removed_by_email_and_verified() {
    let _clock = keep_clock_paused().await;
    let invite = pending_invitation_without_id();
    let administrator = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({}),
    );
    let server = MockServer::start(vec![
        reply(admin(vec![invite.clone(), administrator.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(admin(vec![invite.clone(), administrator.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![administrator], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = remove(&client, SITE, "pending@example.test".into())
        .await
        .expect("pending invite can be selected by email");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("remove and verify pending invitation");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");

    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[4].method, "POST");
    assert_eq!(
        requests[4].target,
        format!("/api/sites/{SITE}/administration?action=removeAccount")
    );
    assert_eq!(body(&requests[4]), json!({"email":"pending@example.test"}));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn removed_email_recreated_with_new_user_id_is_not_verified_as_removed() {
    let _clock = keep_clock_paused().await;
    let original = target_account();
    let active_admin = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({}),
    );
    let recreated = account(
        "replacement-user-id",
        "operator@example.test",
        "operator",
        true,
        json!({"removeAccess":true,"changeRole":true}),
    );
    let server = MockServer::start(vec![
        reply(admin(vec![original.clone(), active_admin.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(admin(vec![original, active_admin.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![recreated, active_admin], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = remove(&client, SITE, "user-1".into())
        .await
        .expect("prepare account removal");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("same email under another identity remains present");
    assert_eq!(report.outcome, Outcome::Unverified, "{report:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[4].method, "POST");
    assert_eq!(body(&requests[4]), json!({"email":"operator@example.test"}));
}

#[tokio::test(start_paused = true)]
async fn removed_account_id_with_renamed_email_is_not_verified_as_removed() {
    let _clock = keep_clock_paused().await;
    let original = target_account();
    let active_admin = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({}),
    );
    let renamed = account(
        "user-1",
        "renamed@example.test",
        "operator",
        true,
        json!({"removeAccess":true,"changeRole":true}),
    );
    let server = MockServer::start(vec![
        reply(admin(vec![original.clone(), active_admin.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(admin(vec![original, active_admin.clone()], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![renamed, active_admin], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = remove(&client, SITE, "user-1".into())
        .await
        .expect("prepare account removal");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("same identity with renamed email remains present");
    assert_eq!(report.outcome, Outcome::Unverified, "{report:?}");
    let requests = server.finish();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[4].method, "POST");
    assert_eq!(body(&requests[4]), json!({"email":"operator@example.test"}));
}

#[tokio::test]
async fn removal_rechecks_last_administrator_after_plan_before_post() {
    let target = account(
        "admin-1",
        "admin@example.test",
        "administrator",
        true,
        json!({"removeAccess":true}),
    );
    let other = account(
        "admin-2",
        "other@example.test",
        "administrator",
        true,
        json!({"removeAccess":true}),
    );
    let server = MockServer::start(vec![
        reply(admin(vec![target.clone(), other], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
        reply(admin(vec![target], false)),
        reply(permissions(&[REMOVE_PERMISSION])),
    ]);
    let client = make_client(&server, Duration::from_secs(1));
    let (mutation, plan) = remove(&client, SITE, "admin-1".into())
        .await
        .expect("plan while another active admin exists");

    let error = mutation
        .write(&plan.desired)
        .await
        .expect_err("fresh account read must protect the final administrator");
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test(start_paused = true)]
async fn change_role_sends_exact_role_body_and_preserves_other_admin() {
    let _clock = keep_clock_paused().await;
    let target = target_account();
    let other = account(
        "admin-2",
        "admin@example.test",
        "administrator",
        true,
        json!({}),
    );
    let desired_target = account(
        "user-1",
        "operator@example.test",
        "viewer",
        true,
        json!({"removeAccess":true,"changeRole":true}),
    );
    let server = MockServer::start(vec![
        reply(admin(vec![target.clone(), other.clone()], false)),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[CHANGE_ROLE_PERMISSION])),
        reply(admin(vec![target.clone(), other.clone()], false)),
        reply(capabilities(&[USER_ROLES], 4)),
        reply(permissions(&[CHANGE_ROLE_PERMISSION])),
        reply(json!({"accepted":true})),
        reply(admin(vec![desired_target, other], false)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = change_role(&client, SITE, "user-1".into(), "viewer".into())
        .await
        .expect("prepare role update");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("apply role update");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");

    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    assert_eq!(requests[6].method, "POST");
    assert_eq!(
        requests[6].target,
        format!("/api/sites/{SITE}/administration?action=changeRole")
    );
    assert_eq!(
        body(&requests[6]),
        json!({"accountId":"user-1","roleOnSite":"viewer"})
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn maintenance_uses_empty_action_body_and_reads_back_boolean_state() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(admin(vec![target_account()], false)),
        reply(json!({"accepted":true})),
        reply(admin(vec![target_account()], true)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = maintenance(&client, SITE, true)
        .await
        .expect("prepare maintenance action");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("apply maintenance action");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    assert_eq!(report.observed, Some(json!({"is_maintenance_mode":true})));
    let requests = server.finish();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].method, "POST");
    assert_eq!(
        requests[2].target,
        format!("/api/sites/{SITE}/administration?action=enableMaintenanceMode")
    );
    assert_eq!(body(&requests[2]), json!({}));
}

#[tokio::test(start_paused = true)]
async fn failed_maintenance_action_is_not_success_even_when_readback_matches() {
    let _clock = keep_clock_paused().await;
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(admin(vec![target_account()], false)),
        Reply::json(500, br#"{"error":"request failed"}"#.to_vec()),
        reply(admin(vec![target_account()], true)),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = maintenance(&client, SITE, true)
        .await
        .expect("prepare maintenance action");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("request failure is returned in mutation report");
    assert_eq!(
        report.outcome,
        Outcome::RequestFailedStateMatches,
        "{report:?}"
    );
    assert!(report.error_kind().is_some());
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn support_token_is_exportable_only_after_matching_fresh_get() {
    let _clock = keep_clock_paused().await;
    let first = admin(vec![target_account()], false);
    let mut readback = admin(vec![target_account()], false);
    readback["supportToken"] = json!({
        "token":SECRET,
        "validUntil":"2030-01-01T00:00:00Z"
    });
    let server = MockServer::start(vec![
        reply(first),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(json!({"supportToken":{"token":SECRET,"validUntil":"2030-01-01T00:00:00Z"}})),
        reply(readback),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = support_token(&client, SITE)
        .await
        .expect("prepare support token request");
    assert!(
        mutation.verified_token().is_err(),
        "no secret before action and readback"
    );
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("generate and verify support token");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    let token = mutation
        .verified_token()
        .expect("matching GET verifies token");
    assert_eq!(token.expose_secret(), SECRET);
    assert!(!format!("{token:?}").contains(SECRET));

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[3].method, "POST");
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/administration?action=generateSupportToken")
    );
    assert_eq!(body(&requests[3]), json!({}));
    assert_eq!(requests[4].method, "GET");
    assert_eq!(
        requests[4].target,
        format!("/api/sites/{SITE}/administration")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn masked_support_token_acknowledgement_never_becomes_exportable() {
    for masked in [
        "(redacted)",
        "[redacted]",
        "<redacted>",
        "masked",
        "******",
        "••••••",
    ] {
        let server = MockServer::start(vec![
            reply(admin(vec![target_account()], false)),
            reply(permissions(&[SUPPORT_PERMISSION])),
            reply(permissions(&[SUPPORT_PERMISSION])),
            reply(json!({"supportToken":{"token":masked}})),
        ]);
        let client = make_client(&server, Duration::from_secs(1));
        let (mutation, plan) = support_token(&client, SITE)
            .await
            .expect("prepare support token request");
        let error = mutation
            .write(&plan.desired)
            .await
            .expect_err("masked acknowledgement must be rejected");
        assert_eq!(error.kind, ErrorKind::Unverified, "{masked}");
        assert!(mutation.verified_token().is_err(), "{masked}");
        let token_field = json!({"token":masked}).to_string();
        assert!(!format!("{error:?}").contains(&token_field), "{masked}");
        assert_eq!(server.finish().len(), 4, "{masked}");
    }
}

#[tokio::test(start_paused = true)]
async fn mismatched_support_token_readback_stays_unverified_and_unexportable() {
    let _clock = keep_clock_paused().await;
    let mut mismatch = admin(vec![target_account()], false);
    mismatch["supportToken"] = json!({
        "token":"different-support-token-sentinel",
        "validUntil":"2030-01-01T00:00:00Z"
    });
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(json!({"supportToken":{"token":SECRET,"validUntil":"2030-01-01T00:00:00Z"}})),
        reply(mismatch),
    ]);
    let client = make_client(&server, Duration::from_secs(2));
    let (mutation, plan) = support_token(&client, SITE)
        .await
        .expect("prepare support token request");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("token mismatch is a report, not a leaked value");
    assert_eq!(report.outcome, Outcome::Unverified, "{report:?}");
    assert!(mutation.verified_token().is_err());
    let requests = server.finish();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/administration?action=generateSupportToken")
    );
}

#[tokio::test(start_paused = true)]
async fn support_token_file_plan_performs_gets_without_creating_output() {
    let _clock = keep_clock_paused().await;
    let output = OutputDir::new();
    let path = output.output_path();
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(permissions(&[SUPPORT_PERMISSION])),
    ]);
    let client = make_client(&server, Duration::from_secs(2));

    validate_support_token_output(&path).expect("validate new private output path");
    let (mutation, plan) = support_token(&client, SITE)
        .await
        .expect("prepare support-token output");
    assert_eq!(
        mutation.write_verified_token(&path).unwrap_err().kind,
        ErrorKind::Unverified
    );
    assert!(!path.exists(), "planning must not create the token file");
    assert!(!format!("{plan:?}").contains(GENERATED_TOKEN));
    assert_eq!(plan.desired, json!({"token_generated":true}));

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
    assert_eq!(
        requests[0].target,
        format!("/api/sites/{SITE}/administration")
    );
    assert_eq!(requests[1].target, format!("/api/sites/{SITE}/permissions"));
}

#[tokio::test(start_paused = true)]
async fn support_token_file_apply_writes_exact_secret_only_after_matching_readback() {
    let _clock = keep_clock_paused().await;
    let output = OutputDir::new();
    let path = output.output_path();
    let mut matching = admin(vec![target_account()], false);
    matching["supportToken"] = json!({"token":GENERATED_TOKEN});
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(json!({"supportToken":{"token":GENERATED_TOKEN}})),
        reply(matching),
    ]);
    let client = make_client(&server, Duration::from_secs(2));

    let (mutation, plan) = support_token(&client, SITE)
        .await
        .expect("prepare support-token output");
    assert!(!path.exists());
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("write only after matching fresh readback");
    assert_eq!(report.outcome, Outcome::Verified, "{report:?}");
    assert!(!path.exists(), "readback does not export by itself");
    mutation
        .write_verified_token(&path)
        .expect("save the verified token privately");
    assert_eq!(
        std::fs::read(&path).expect("read private token file"),
        GENERATED_TOKEN.as_bytes()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let safe_output = serde_json::to_string(&json!({"plan":plan,"report":report})).unwrap();
    assert!(!safe_output.contains(GENERATED_TOKEN));
    assert!(!safe_output.contains(SECRET));

    let requests = server.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[3].method, "POST");
    assert_eq!(
        requests[3].target,
        format!("/api/sites/{SITE}/administration?action=generateSupportToken")
    );
    assert_eq!(body(&requests[3]), json!({}));
    assert_eq!(requests[4].method, "GET");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn support_token_file_mismatch_keeps_output_absent_and_report_safe() {
    let _clock = keep_clock_paused().await;
    let output = OutputDir::new();
    let path = output.output_path();
    let mut mismatch = admin(vec![target_account()], false);
    mismatch["supportToken"] = json!({"token":"different-support-token-sentinel"});
    let server = MockServer::start(vec![
        reply(admin(vec![target_account()], false)),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(permissions(&[SUPPORT_PERMISSION])),
        reply(json!({"supportToken":{"token":GENERATED_TOKEN}})),
        reply(mismatch),
    ]);
    let client = make_client(&server, Duration::from_secs(2));

    let (mutation, plan) = support_token(&client, SITE)
        .await
        .expect("prepare support-token output");
    let report = apply_readback_once(&mutation, &plan, Duration::from_secs(2))
        .await
        .expect("token mismatch is reported safely");
    assert_eq!(report.outcome, Outcome::Unverified, "{report:?}");
    assert_eq!(
        mutation.write_verified_token(&path).unwrap_err().kind,
        ErrorKind::Unverified
    );
    assert!(!path.exists(), "mismatched readback must not create output");
    let safe_output = serde_json::to_string(&json!({"plan":plan,"report":report})).unwrap();
    assert!(!safe_output.contains(GENERATED_TOKEN));
    assert!(!safe_output.contains("different-support-token-sentinel"));
    assert_eq!(server.finish().len(), 5);
}
