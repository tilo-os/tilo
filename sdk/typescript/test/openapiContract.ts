/**
 * Holds the fake API to `docs/api/openapi.json`.
 *
 * Every request the SDK sends and every answer `FakeApi` gives has to be one
 * the spec describes: a method and path the spec routes, a query and body
 * whose shape matches, a status the operation declares, and a JSON body
 * matching that status's schema. `FakeApi` checks each exchange through here,
 * so a test that drives the SDK proves the fake speaks the documented API;
 * the shapes can no longer drift apart silently.
 *
 * The schema walk covers what the spec's schemas use: `type`, `properties`,
 * `required`, `additionalProperties`, `items`, `enum`, `const`, `oneOf`,
 * `anyOf`, `minimum`, `maximum`, `minLength`, `maxItems`, `minItems`,
 * `pattern` and `$ref`. A property the schema does not name is refused, so an
 * answer that grows a field the API does not document fails too.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { Recorded } from "./fakeApi.ts";

/**
 * Where the spec lives beside the tree; `TILO_OPENAPI` names another copy, as
 * the CI image's temporary copy of the SDK does.
 */
const SPEC_PATH =
  process.env.TILO_OPENAPI ??
  fileURLToPath(new URL("../../../docs/api/openapi.json", import.meta.url));

export class ContractError extends Error {}

type Schema = Record<string, unknown>;

export class Contract {
  private readonly spec: Schema;
  private readonly paths: Record<string, Record<string, Schema>>;
  /** Longest templates first: a resource path may itself hold "/", so the
   * template that names `actions/{action}` has to be tried before the one
   * that ends at `resources/{path}`. */
  private readonly templates: string[];

  constructor(spec: Schema = JSON.parse(readFileSync(SPEC_PATH, "utf8")) as Schema) {
    this.spec = spec;
    this.paths = spec.paths as Record<string, Record<string, Schema>>;
    // Longest and most literal first, so `/invocations/{invocation}/cancel`
    // beats `/invocations/{invocation}/output` only by its words.
    const rank = (path: string): number[] => {
      const parts = segmentsOf(path);
      return [parts.length, parts.filter((part) => !part.startsWith("{")).length];
    };
    this.templates = Object.keys(this.paths).sort((left, right) => {
      const [leftDepth, leftLiterals] = rank(left) as [number, number];
      const [rightDepth, rightLiterals] = rank(right) as [number, number];
      return rightDepth - leftDepth || rightLiterals - leftLiterals || left.localeCompare(right);
    });
  }

  /** Raise `ContractError` unless `call` and its `response` are documented. */
  async check(call: Recorded, response: Response): Promise<void> {
    const segments = segmentsOf(call.raw.split("?")[0] ?? "");
    if (segments[0] !== "v1") {
      // The device flow (`/keys/new`, `/keys/poll`) is outside the versioned
      // API and its spec.
      return;
    }
    const [template, operation] = this.operation(call.method, segments);
    const errors: string[] = [];
    this.request(template, operation, call, segments, errors);
    await this.response(operation, call, response, errors);
    if (errors.length > 0) {
      throw new ContractError(
        `${call.method} /${segments.join("/")}: ${errors.join("; ")}`,
      );
    }
  }

  private operation(method: string, segments: string[]): [string, Schema] {
    for (const template of this.templates) {
      if (bind(segmentsOf(template), segments) === null) {
        continue;
      }
      const operation = this.paths[template]?.[method.toLowerCase()];
      if (operation === undefined) {
        throw new ContractError(
          `${method} /${segments.join("/")} matches ${template}, which declares no ${method}`,
        );
      }
      return [template, operation];
    }
    throw new ContractError(`${method} /${segments.join("/")} is not a route openapi.json names`);
  }

  private request(
    template: string,
    operation: Schema,
    call: Recorded,
    segments: string[],
    errors: string[],
  ): void {
    const parameters = ((operation.parameters as unknown[]) ?? []).map((parameter) =>
      this.resolve(parameter),
    ) as Schema[];
    const bound = bind(segmentsOf(template), segments) ?? {};
    for (const parameter of parameters) {
      const where = parameter.in as string;
      const name = parameter.name as string;
      const schema = (parameter.schema as Schema) ?? {};
      if (parameter.required === true && where === "path" && !(name in bound)) {
        errors.push(`the path does not bind ${name}`);
      }
      if (parameter.required === true && where === "query" && !(name in call.query)) {
        errors.push(`\`${name}\` is a required query parameter`);
      }
      if (where === "path" && name in bound) {
        this.node(schema, bound[name], `path.${name}`, errors);
      }
    }
    const declared = new Map(
      parameters
        .filter((parameter) => parameter.in === "query")
        .map((parameter) => [parameter.name as string, parameter]),
    );
    for (const name of Object.keys(call.query)) {
      if (declared.has(name)) {
        continue;
      }
      // An object parameter spreads over `name[key]` in the URL.
      const owner = name.split("[")[0] ?? name;
      if (declared.has(owner)) {
        continue;
      }
      errors.push(`the query carries \`${name}\`, which ${template} does not declare`);
    }
    for (const [name, parameter] of declared) {
      const schema = (parameter.schema as Schema) ?? {};
      let value: unknown;
      if (name in call.query) {
        value = call.query[name];
      } else if (schema.type === "object") {
        const spread: Record<string, string> = {};
        for (const [key, found] of Object.entries(call.query)) {
          if (key.startsWith(`${name}[`) && key.endsWith("]")) {
            spread[key.slice(name.length + 1, -1)] = found;
          }
        }
        if (Object.keys(spread).length === 0) {
          continue;
        }
        value = spread;
      } else {
        continue;
      }
      this.node(schema, value, `query.${name}`, errors);
    }
    const body = call.body;
    const contentType = (call.headers["content-type"] ?? "").split(";")[0]?.trim() ?? "";
    const requestBody = operation.requestBody as Schema | undefined;
    if (requestBody !== undefined) {
      const described = this.resolve(requestBody) as Schema;
      if (body === "") {
        if (described.required === true) {
          errors.push("the call has no body, which this route requires");
        }
      } else {
        const content = (described.content as Record<string, Schema>) ?? {};
        const schema = contentSchema(content, contentType);
        if (schema === undefined) {
          errors.push(`the route takes no \`${contentType || "unknown"}\` body`);
        } else if (contentType.startsWith("application/json") || contentType === "") {
          let sent: unknown;
          try {
            sent = JSON.parse(body);
          } catch {
            errors.push("the body is not JSON");
            return;
          }
          this.node(schema, sent, "body", errors);
        }
      }
    } else if (body !== "" && contentType.startsWith("application/json")) {
      errors.push("the route takes no body");
    }
  }

  private async response(
    operation: Schema,
    call: Recorded,
    response: Response,
    errors: string[],
  ): Promise<void> {
    const status = String(response.status);
    const responses = (operation.responses as Record<string, Schema>) ?? {};
    const answered = this.resolve(responses[status] ?? responses.default) as Schema | undefined;
    if (answered === undefined) {
      errors.push(`it answered ${status}, which it does not declare`);
      return;
    }
    const contentType = (response.headers.get("content-type") ?? "").split(";")[0]?.trim() ?? "";
    const content = (answered.content as Record<string, Schema>) ?? {};
    if (contentType === "application/json") {
      const schema = contentSchema(content, contentType);
      if (schema === undefined) {
        errors.push(`it answered ${status} JSON, which it does not declare`);
        return;
      }
      let value: unknown;
      try {
        value = JSON.parse(await response.clone().text());
      } catch {
        errors.push(`it answered ${status} a body that is not JSON`);
        return;
      }
      this.node(schema, value, "answer", errors);
    } else if (Object.keys(content).length > 0) {
      // A stream or a file: the status's content type has to be one the spec
      // names, and its body is not JSON to check.
      if (contentSchema(content, contentType) === undefined && !("*/*" in content)) {
        errors.push(`it answered ${status} \`${contentType}\`, which it does not declare`);
      }
    }
  }

  private resolve(node: unknown): unknown {
    let found = node;
    while (typeof found === "object" && found !== null && "$ref" in found) {
      const ref = (found as Schema).$ref as string;
      if (!ref.startsWith("#/")) {
        throw new ContractError(`cannot resolve a non-local reference ${ref}`);
      }
      let at: unknown = this.spec;
      for (const part of ref.slice(2).split("/")) {
        at = (at as Schema)[part.replaceAll("~1", "/").replaceAll("~0", "~")];
      }
      found = at;
    }
    return found;
  }

  /** @internal The schema walk, shared with `matching`. */
  node(schema: unknown, value: unknown, at: string, errors: string[]): void {
    const node = this.resolve(schema);
    if (node === true || (typeof node === "object" && node !== null && Object.keys(node).length === 0)) {
      return;
    }
    if (node === false) {
      errors.push(`${at} is not allowed`);
      return;
    }
    if (typeof node !== "object" || node === null) {
      return;
    }
    const schemaNode = node as Schema;
    if ("const" in schemaNode && !same(value, schemaNode.const)) {
      errors.push(`${at} is ${short(value)}, not ${short(schemaNode.const)}`);
      return;
    }
    const choices = schemaNode.enum as unknown[] | undefined;
    if (choices !== undefined && !choices.some((choice) => same(value, choice))) {
      errors.push(`${at} is ${short(value)}, not one of ${short(choices)}`);
      return;
    }
    const oneOf = schemaNode.oneOf as unknown[] | undefined;
    if (oneOf !== undefined) {
      const matches = matching(this, oneOf, value, at);
      if (matches.length !== 1) {
        errors.push(`${at} matches ${matches.length} of the oneOf branches, not one`);
        return;
      }
    }
    const anyOf = schemaNode.anyOf as unknown[] | undefined;
    if (anyOf !== undefined && matching(this, anyOf, value, at).length === 0) {
      errors.push(`${at} matches none of the anyOf branches`);
      return;
    }
    const types = nodeNames(schemaNode.type);
    if (types.length > 0 && !types.some((name) => isType(value, name))) {
      errors.push(`${at} is ${short(value)}, not ${types.join(" or ")}`);
      return;
    }
    if (typeof value === "string") {
      const minLength = schemaNode.minLength as number | undefined;
      if (minLength !== undefined && value.length < minLength) {
        errors.push(`${at} is shorter than ${minLength}`);
      }
      const pattern = schemaNode.pattern as string | undefined;
      if (pattern !== undefined && new RegExp(pattern, "u").test(value) === false) {
        errors.push(`${at} does not match ${pattern}`);
      }
    }
    if (typeof value === "number") {
      const minimum = schemaNode.minimum as number | undefined;
      if (minimum !== undefined && value < minimum) {
        errors.push(`${at} is below ${minimum}`);
      }
      const maximum = schemaNode.maximum as number | undefined;
      if (maximum !== undefined && value > maximum) {
        errors.push(`${at} is above ${maximum}`);
      }
    }
    if (Array.isArray(value)) {
      const minItems = schemaNode.minItems as number | undefined;
      if (minItems !== undefined && value.length < minItems) {
        errors.push(`${at} holds fewer than ${minItems} items`);
      }
      const maxItems = schemaNode.maxItems as number | undefined;
      if (maxItems !== undefined && value.length > maxItems) {
        errors.push(`${at} holds more than ${maxItems} items`);
      }
      if (schemaNode.items !== undefined) {
        value.forEach((member, index) => {
          this.node(schemaNode.items, member, `${at}[${index}]`, errors);
        });
      }
    }
    const objectShape =
      types.includes("object") ||
      "properties" in schemaNode ||
      "additionalProperties" in schemaNode;
    if (typeof value === "object" && value !== null && !Array.isArray(value) && objectShape) {
      const properties = (schemaNode.properties as Record<string, unknown>) ?? {};
      for (const name of (schemaNode.required as string[]) ?? []) {
        if (!(name in value)) {
          errors.push(`${at} has no \`${name}\``);
        }
      }
      // A bare `{"type": "object"}` is the spec's way of describing a body the
      // app types itself (an action's arguments), so its keys are the app's,
      // not the spec's.
      if (types.includes("object") && !("properties" in schemaNode) && !("additionalProperties" in schemaNode)) {
        return;
      }
      const additional = schemaNode.additionalProperties ?? false;
      for (const [name, member] of Object.entries(value as Record<string, unknown>)) {
        const where = `${at}.${name}`;
        if (name in properties) {
          this.node(properties[name], member, where, errors);
        } else if (additional === false) {
          errors.push(`${where} is not a property the spec names`);
        } else if (additional !== true) {
          this.node(additional, member, where, errors);
        }
      }
    }
  }
}

/** The parameter values `segments` binds to `template`, or `null`.
 *
 * A `{param}` takes one or more segments: a resource path is one parameter
 * though its segments stay in the URL, and a name may hold a `/`.
 */
export function bind(template: string[], segments: string[]): Record<string, string> | null {
  if (template.length === 0) {
    return segments.length === 0 ? {} : null;
  }
  const [head, ...rest] = template as [string, ...string[]];
  if (head.startsWith("{") && head.endsWith("}")) {
    const name = head.slice(1, -1);
    // Try the shortest take that still lets the rest match, so a literal
    // segment after the parameter lands where the template puts it.
    for (let take = 1; take <= segments.length; take += 1) {
      if (rest.length === 0 && take !== segments.length) {
        continue;
      }
      const bound = bind(rest, segments.slice(take));
      if (bound !== null) {
        bound[name] = segments.slice(0, take).join("/");
        return bound;
      }
    }
    return null;
  }
  if (segments[0] !== head) {
    return null;
  }
  return bind(rest, segments.slice(1));
}

function segmentsOf(path: string): string[] {
  return path
    .split("/")
    .filter((part) => part !== "")
    .map((part) => decodeURIComponent(part));
}

function matching(contract: Contract, branches: unknown[], value: unknown, at: string): number[] {
  return branches.flatMap((branch, index) => {
    const errors: string[] = [];
    contract.node(branch, value, at, errors);
    return errors.length === 0 ? [index] : [];
  });
}

function contentSchema(
  content: Record<string, Schema>,
  contentType: string,
): unknown | undefined {
  if (content[contentType]?.schema !== undefined) {
    return content[contentType]?.schema;
  }
  if (content["*/*"]?.schema !== undefined) {
    return content["*/*"]?.schema;
  }
  return undefined;
}

function nodeNames(types: unknown): string[] {
  if (types === undefined || types === null) {
    return [];
  }
  return Array.isArray(types) ? (types as string[]) : [types as string];
}

function isType(value: unknown, name: string): boolean {
  switch (name) {
    case "object":
      return typeof value === "object" && value !== null && !Array.isArray(value);
    case "array":
      return Array.isArray(value);
    case "string":
      return typeof value === "string";
    case "boolean":
      return typeof value === "boolean";
    case "integer":
      return typeof value === "number" && Number.isInteger(value);
    case "number":
      return typeof value === "number";
    case "null":
      return value === null;
    default:
      return true;
  }
}

function same(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function short(value: unknown): string {
  const text = JSON.stringify(value) ?? String(value);
  return text.length <= 120 ? text : `${text.slice(0, 117)}...`;
}
