# AWS Fixture Recording

UX-13 replay tests use sanitized AWS SDK HTTP fixtures. Record fixtures from
a throwaway account, then strip account-specific IDs before committing.

1. Set an isolated profile and region:

   ```sh
   export AWS_PROFILE=ghost-complete-fixtures
   export AWS_REGION=us-east-1
   ```

2. Capture the operation with SDK or proxy logging enabled. Keep the HTTP
   response body and remove credentials, request signatures, account aliases,
   and nonessential headers.

3. Replace account IDs with `123456789012`, request IDs with
   `00000000-0000-0000-0000-000000000000`, and role/user/group/policy suffixes
   with stable `fixture-*` names.

4. Store sanitized responses under
   `crates/gc-suggest/tests/aws/fixtures/<service>/<operation>.*` and add a
   `StaticReplayClient` assertion in `crates/gc-suggest/tests/aws_replay.rs`.

Current seed fixture:

- `iam/list_roles.xml` verifies that `aws-sdk-iam` decodes a replayed
  `ListRoles` response through the real generated SDK client.
