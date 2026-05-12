use aws_sdk_iam::error::SdkError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AwsSdkErrorKind {
    AuthMissing,
    AuthExpired,
    Network,
    Other,
}

#[derive(Debug)]
pub(crate) struct AwsProviderError {
    pub(crate) kind: AwsSdkErrorKind,
    pub(crate) message: String,
}

impl std::fmt::Display for AwsProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for AwsProviderError {}

pub(crate) fn classify_sdk_error<E>(error: &SdkError<E>) -> AwsSdkErrorKind
where
    E: std::fmt::Debug + std::fmt::Display,
{
    let rendered = error.to_string().to_ascii_lowercase();
    if rendered.contains("expired") {
        AwsSdkErrorKind::AuthExpired
    } else if rendered.contains("credential")
        || rendered.contains("credentials")
        || rendered.contains("accessdenied")
        || rendered.contains("access denied")
        || rendered.contains("unauthorized")
    {
        AwsSdkErrorKind::AuthMissing
    } else if matches!(
        error,
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_)
    ) {
        AwsSdkErrorKind::Network
    } else {
        AwsSdkErrorKind::Other
    }
}
