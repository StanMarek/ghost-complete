#[path = "aws/replay.rs"]
mod replay;

#[tokio::test]
async fn static_replay_client_decodes_iam_list_roles_fixture() {
    let (client, _http_client) =
        replay::iam_client_for("ListRoles", include_str!("aws/fixtures/iam/list_roles.xml"));

    let output = client
        .list_roles()
        .send()
        .await
        .expect("static replay fixture should decode as ListRoles");

    let roles = output.roles();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].role_name(), "fixture-admin");
    assert_eq!(
        roles[0].arn(),
        "arn:aws:iam::123456789012:role/fixture-admin"
    );
}
