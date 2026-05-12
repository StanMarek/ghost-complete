import { readFileSync } from 'node:fs';

const AWS_OPERATIONS = JSON.parse(
  readFileSync(new URL('./aws-operations.json', import.meta.url), 'utf8'),
);

function operationEntry(service, operation) {
  const serviceEntries = AWS_OPERATIONS[service];
  if (!serviceEntries || typeof serviceEntries !== 'object' || Array.isArray(serviceEntries)) {
    return null;
  }
  return serviceEntries[operation] ?? null;
}

function providerParams(service, entry) {
  const params = {
    service,
    operation: entry.operation,
    field: entry.field,
  };
  if (entry.description_field) {
    params.description_field = entry.description_field;
  }
  if (entry.params && typeof entry.params === 'object' && !Array.isArray(entry.params)) {
    for (const key of Object.keys(entry.params).sort()) {
      params[key] = entry.params[key];
    }
  }
  return params;
}

export function mapAwsSdkGenerator(specName, scriptArgv) {
  if (specName !== 'aws') return null;
  if (!Array.isArray(scriptArgv) || scriptArgv.length < 3) return null;
  if (scriptArgv[0] !== 'aws') return null;

  const service = scriptArgv[1];
  const cliOperation = scriptArgv[2];
  if (typeof service !== 'string' || typeof cliOperation !== 'string') return null;

  const entry = operationEntry(service, cliOperation);
  if (!entry) return null;

  return {
    type: 'aws_sdk',
    params: providerParams(service, entry),
  };
}
