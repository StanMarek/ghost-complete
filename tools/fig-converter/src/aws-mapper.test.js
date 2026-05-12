import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { mapAwsSdkGenerator } from './aws-mapper.js';
import { processGenerator } from './index.js';

describe('mapAwsSdkGenerator', () => {
  it('maps IAM list-roles to aws_sdk params', () => {
    assert.deepStrictEqual(
      mapAwsSdkGenerator('aws', ['aws', 'iam', 'list-roles']),
      {
        type: 'aws_sdk',
        params: {
          service: 'iam',
          operation: 'ListRoles',
          field: 'Roles[*].RoleName',
          description_field: 'Roles[*].Arn',
        },
      },
    );
  });

  it('maps IAM list-policies with Scope=Local', () => {
    assert.deepStrictEqual(
      mapAwsSdkGenerator('aws', ['aws', 'iam', 'list-policies']),
      {
        type: 'aws_sdk',
        params: {
          service: 'iam',
          operation: 'ListPolicies',
          field: 'Policies[*].PolicyName',
          description_field: 'Policies[*].Arn',
          Scope: 'Local',
        },
      },
    );
  });

  it('leaves non-IAM AWS scripts and unknown IAM operations unmapped', () => {
    assert.equal(mapAwsSdkGenerator('aws', ['aws', 's3', 'list-buckets']), null);
    assert.equal(mapAwsSdkGenerator('aws', ['aws', 'iam', 'list-server-certificates']), null);
  });

  it('only maps the aws spec and aws CLI script shape', () => {
    assert.equal(mapAwsSdkGenerator('aws/s3', ['aws', 'iam', 'list-roles']), null);
    assert.equal(mapAwsSdkGenerator('aws', ['bash', '-c', 'aws iam list-roles']), null);
    assert.equal(mapAwsSdkGenerator('aws', ['aws', 'iam']), null);
  });
});

describe('processGenerator AWS SDK mapping', () => {
  it('strips JS runtime fields but preserves CLI fallback script data', () => {
    const out = processGenerator(
      {
        script: ['aws', 'iam', 'list-users'],
        requires_js: true,
        js_runtime: {
          kind: 'post_process',
          source: 'old runtime',
        },
        _postProcessSource: 'function(t){return l(t,"Users","UserName","Arn")}',
      },
      'aws',
    );

    assert.deepStrictEqual(out, {
      type: 'aws_sdk',
      params: {
        service: 'iam',
        operation: 'ListUsers',
        field: 'Users[*].UserName',
        description_field: 'Users[*].Arn',
      },
      script: ['aws', 'iam', 'list-users'],
      transforms: [
        {
          type: 'json_path_extract',
          array: 'Users',
          name_field: 'UserName',
          description_field: 'Arn',
        },
      ],
      _lowered_from_requires_js: true,
    });
    assert.equal(out.requires_js, undefined);
    assert.equal(out.js_runtime, undefined);
  });
});
