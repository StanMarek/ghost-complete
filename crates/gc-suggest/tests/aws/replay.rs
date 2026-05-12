use aws_credential_types::Credentials;
use aws_sdk_iam::config::Region;
use aws_smithy_runtime::client::http::test_util::{ReplayEvent, StaticReplayClient};
use aws_smithy_types::body::SdkBody;

pub fn iam_client_for(
    operation: &str,
    response_body: &'static str,
) -> (aws_sdk_iam::Client, StaticReplayClient) {
    let http_client = StaticReplayClient::new(vec![ReplayEvent::new(
        http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .body(SdkBody::empty())
            .expect("fixture request must build"),
        http::Response::builder()
            .status(200)
            .header("content-type", "text/xml")
            .body(SdkBody::from(response_body))
            .expect("fixture response must build"),
    )]);
    let config = aws_sdk_iam::Config::builder()
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "AKIDEXAMPLE",
            "SECRETEXAMPLE",
            None,
            None,
            "static-replay",
        ))
        .http_client(http_client.clone())
        .endpoint_url("https://iam.amazonaws.com")
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    tracing::trace!(operation, "created static AWS replay IAM client");
    (aws_sdk_iam::Client::from_conf(config), http_client)
}
