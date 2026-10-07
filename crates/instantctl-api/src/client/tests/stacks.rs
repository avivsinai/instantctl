use super::*;
use crate::{ErrorKind, client::stacks};
use serde_json::{Value, json};
use std::time::Duration;

const SITE: &str = "123e4567-e89b-12d3-a456-426614174000";
const CONDUCTOR: &str = "aa:bb:cc:dd:ee:ff";
const MEMBER: &str = "11:22:33:44:55:66";

fn reply_json(value: Value) -> Reply {
    Reply::json(
        200,
        serde_json::to_vec(&value).expect("serialize stack fixture"),
    )
}

fn member(device_id: &str, role: Value, device_identity: &str, name: &str) -> Value {
    json!({
        "deviceId":device_id,
        "deviceStackRole":role,
        "device":{
            "id":device_identity,
            "macAddress":device_identity,
            "name":name,
            "serialNumber":"serial-safe-field",
            "sharedSecret":"never-project-this"
        }
    })
}

fn stack(id: &str, name: Option<&str>, members: Vec<Value>, conductor: Option<&str>) -> Value {
    let mut value = json!({"id":id,"deviceStackMembers":members});
    if let Some(name) = name {
        value["name"] = json!(name);
    }
    if let Some(conductor) = conductor {
        value["activeConductorId"] = json!(conductor);
    }
    value
}

fn collection(elements: Vec<Value>) -> Value {
    json!({
        "kind":"resourceList",
        "totalCount":elements.len(),
        "matchingFilterCount":elements.len(),
        "pendingAvailability":null,
        "elements":elements
    })
}

fn assert_get(request: &Request) {
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, format!("/api/sites/{SITE}/deviceStacks"));
    assert!(request.body.is_empty());
}

#[tokio::test]
async fn list_and_show_use_one_collection_and_project_safe_member_identity() {
    let primary = stack(
        "stack-opaque-7",
        Some("Core stack"),
        vec![
            member(CONDUCTOR, json!("conductor"), CONDUCTOR, "Core switch"),
            member(MEMBER, json!("future-role"), MEMBER, "Edge switch"),
        ],
        Some(CONDUCTOR),
    );
    let secondary = stack(
        "stack-opaque-8",
        None,
        vec![member(MEMBER, json!("conductor"), MEMBER, "Solo switch")],
        None,
    );
    let server = MockServer::start(vec![reply_json(collection(vec![primary, secondary]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let stacks = stacks::list(&api, SITE).await.expect("stacks list");
    assert_eq!(stacks.len(), 2);
    assert_eq!(stacks[0].id, "stack-opaque-7");
    assert_eq!(stacks[0].name.as_deref(), Some("Core stack"));
    assert_eq!(stacks[0].active_conductor_id.as_deref(), Some(CONDUCTOR));
    assert_eq!(stacks[0].members[1].role.as_deref(), Some("future-role"));
    assert_eq!(stacks[0].members[1].device_id, MEMBER);
    assert_eq!(stacks[1].name, None);
    assert_eq!(stacks[1].active_conductor_id, None);

    let by_id = stacks::select(&stacks, "stack-opaque-7").unwrap();
    let by_name = stacks::select(&stacks, "Core stack").unwrap();
    assert_eq!(by_id, by_name);
    let projected = serde_json::to_value(by_id).unwrap();
    assert_eq!(projected["members"][0]["mac_address"], CONDUCTOR);
    assert!(projected.get("sharedSecret").is_none());
    assert!(projected["members"][0].get("serialNumber").is_none());

    assert_get(&server.finish()[0]);
}

#[tokio::test]
async fn empty_collection_is_a_valid_list_and_unknown_name_does_not_fetch_an_item() {
    let server = MockServer::start(vec![reply_json(collection(vec![]))]);
    let api = make_client(&server, Duration::from_secs(1));
    let stacks = stacks::list(&api, SITE).await.unwrap();
    assert!(stacks.is_empty());
    assert_eq!(
        stacks::select(&stacks, "missing").unwrap_err().kind,
        ErrorKind::NotFound
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn incomplete_or_ambiguous_stack_collections_fail_closed() {
    let valid_member = member(CONDUCTOR, json!("conductor"), CONDUCTOR, "Core switch");
    let duplicate_id = stack(
        "duplicate",
        Some("One"),
        vec![valid_member.clone()],
        Some(CONDUCTOR),
    );
    let duplicate_member = stack(
        "member-duplicate",
        Some("Two"),
        vec![valid_member.clone(), valid_member.clone()],
        Some(CONDUCTOR),
    );
    let missing_members = json!({"id":"missing-members"});
    let null_members = json!({"id":"null-members","deviceStackMembers":null});
    let missing_device_identity = stack(
        "missing-device-id",
        Some("Three"),
        vec![
            json!({"deviceId":CONDUCTOR,"deviceStackRole":"conductor","device":{"macAddress":CONDUCTOR}}),
        ],
        Some(CONDUCTOR),
    );
    let mismatched_nested_identity = stack(
        "mismatch-device-id",
        Some("Four"),
        vec![member(
            CONDUCTOR,
            json!("conductor"),
            MEMBER,
            "Wrong device",
        )],
        Some(CONDUCTOR),
    );
    let conductor_not_in_members = stack(
        "missing-conductor-member",
        Some("Five"),
        vec![member(MEMBER, json!("conductor"), MEMBER, "Other switch")],
        Some(CONDUCTOR),
    );

    let cases = [
        collection(vec![duplicate_id.clone(), duplicate_id]),
        collection(vec![duplicate_member]),
        collection(vec![missing_members]),
        collection(vec![null_members]),
        collection(vec![missing_device_identity]),
        collection(vec![mismatched_nested_identity]),
        collection(vec![conductor_not_in_members]),
        {
            let mut partial = collection(vec![stack(
                "partial",
                Some("Partial"),
                vec![valid_member.clone()],
                Some(CONDUCTOR),
            )]);
            partial["totalCount"] = json!(2);
            partial
        },
        {
            let mut paged = collection(vec![stack(
                "paged",
                Some("Paged"),
                vec![valid_member.clone()],
                Some(CONDUCTOR),
            )]);
            paged["metaData"] = json!({"hasMore":true});
            paged
        },
    ];

    for payload in cases {
        let server = MockServer::start(vec![reply_json(payload)]);
        let api = make_client(&server, Duration::from_secs(1));
        let error = match stacks::list(&api, SITE).await {
            Ok(_) => panic!("incomplete stack collection was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Unverified);
        assert_eq!(
            server.finish().len(),
            1,
            "list must not guess an item route"
        );
    }
}

#[test]
fn selection_requires_exact_unique_names_and_ids() {
    let make = |id: &str, name: &str| stacks::Stack {
        id: id.into(),
        name: Some(name.into()),
        active_conductor_id: None,
        members: Vec::new(),
    };
    let rows = [make("stack-1", "Core"), make("stack-2", "Core")];
    assert_eq!(stacks::select(&rows, "stack-1").unwrap().id, "stack-1");
    assert_eq!(
        stacks::select(&rows, "Core").unwrap_err().kind,
        ErrorKind::Usage
    );
    assert_eq!(
        stacks::select(&rows, "core").unwrap_err().kind,
        ErrorKind::NotFound
    );
}
