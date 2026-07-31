import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";

import { compile } from "json-schema-to-typescript";

const SCHEMAS = [
  ["broker", "plugin-broker-v1.schema.json"],
  ["bridge", "plugin-ui-bridge-v1.schema.json"],
  ["contributions", "plugin-contribution-v1.schema.json"],
  ["settings", "plugin-settings-v1.schema.json"],
];

const options = parseArguments(process.argv.slice(2));
const mergedSchema = await mergeSchemas(options.schemaDir);
const generated = await compile(mergedSchema, "JarvisPluginUiContracts", {
  additionalProperties: false,
  bannerComment: "",
  cwd: options.schemaDir,
  enableConstEnums: false,
  ignoreMinAndMaxItems: true,
  strictIndexSignatures: true,
  unknownAny: true,
  style: {
    bracketSpacing: true,
    printWidth: 100,
    semi: true,
    singleQuote: false,
    tabWidth: 2,
    trailingComma: "all",
    useTabs: false,
  },
});

const header = [
  "/* eslint-disable */",
  "/**",
  " * Generated from Jarvis public plugin JSON Schemas.",
  " * Do not edit by hand; run `npm run generate:plugin-contracts`.",
  " */",
  "",
].join("\n");
const contents = `${header}${stableNewlines(generated)}`;

await fs.mkdir(path.dirname(options.typescriptOut), { recursive: true });
await fs.writeFile(options.typescriptOut, contents, "utf8");

async function mergeSchemas(schemaDir) {
  const definitions = {};
  const properties = {};

  for (const [property, filename] of SCHEMAS) {
    const source = await fs.readFile(path.join(schemaDir, filename), "utf8");
    const schema = JSON.parse(source);

    for (const [name, definition] of Object.entries(schema.definitions ?? {})) {
      const existing = definitions[name];
      if (existing !== undefined && stableJson(existing) !== stableJson(definition)) {
        throw new Error(`conflicting schema definition ${name} in ${filename}`);
      }
      definitions[name] = definition;
    }

    delete schema.$schema;
    delete schema.definitions;
    properties[property] = schema;
  }

  return {
    $schema: "http://json-schema.org/draft-07/schema#",
    additionalProperties: false,
    definitions,
    properties,
    required: SCHEMAS.map(([property]) => property),
    title: "JarvisPluginUiContracts",
    type: "object",
  };
}

function parseArguments(argumentsList) {
  const parsed = {
    schemaDir: path.resolve("schemas"),
    typescriptOut: path.resolve("packages/jarvis-plugin-ui/src/generated/contracts.ts"),
  };

  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    const value = argumentsList[index + 1];
    if (argument === "--schema-dir" || argument === "--typescript-out") {
      if (value === undefined) {
        throw new Error(`${argument} requires a path`);
      }
      const key = argument === "--schema-dir" ? "schemaDir" : "typescriptOut";
      parsed[key] = path.resolve(value);
      index += 1;
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }

  return parsed;
}

function stableJson(value) {
  if (Array.isArray(value)) {
    return `[${value.map(stableJson).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, child]) => `${JSON.stringify(key)}:${stableJson(child)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function stableNewlines(value) {
  return `${value.replaceAll("\r\n", "\n").replace(/[ \t]+\n/g, "\n").trimEnd()}\n`;
}
