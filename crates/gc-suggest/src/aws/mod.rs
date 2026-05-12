pub(crate) mod clients;
pub(crate) mod error;

/// Set `AWS_EC2_METADATA_DISABLED=true` so that the AWS SDK never
/// attempts a 169.254.169.254 probe for completion suggestions. The
/// `aws_config` crate has no typed builder for this in 1.6.3, so we
/// fall back to the env var the SDK already consults.
///
/// # Safety
///
/// **Must be called before the tokio runtime is built**, while the
/// process is still single-threaded. `std::env::set_var` is racy
/// against any concurrent reader or writer of the environment — but
/// the contract here is that this runs in `fn main` before any thread
/// is spawned. The function is `unsafe` to make that contract visible
/// at every call site.
pub unsafe fn set_imds_disabled_env() {
    if std::env::var_os("AWS_EC2_METADATA_DISABLED").is_some() {
        return;
    }
    // SAFETY: Forwarded — see function-level docs.
    unsafe {
        std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    }
}
